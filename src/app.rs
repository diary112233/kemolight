use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
};

use eframe::egui;

use crate::{
    brightness::{
        apply_step, clamp_percent, create_backend, BrightnessBackend, BrightnessValue, DisplayId,
        DisplayInfo, Result,
    },
    config::AppConfig,
    tray, ui,
};

const APP_TITLE: &str = "Brightness Controller";
const APP_ID: &str = "dev.kemolight.BrightnessController";

#[derive(Debug, Clone)]
pub struct DisplayState {
    pub info: DisplayInfo,
    pub pending: bool,
    pub error: Option<String>,
    pending_request_id: u64,
}

#[derive(Debug, Clone, Default)]
pub struct AppSnapshot {
    pub displays: Vec<DisplayState>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
enum BrightnessCommand {
    Refresh,
    Set {
        display_id: DisplayId,
        value: u32,
        request_id: u64,
    },
    Adjust {
        display_id: DisplayId,
        delta: i32,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum AppCommand {
    OpenWindow,
    HideWindow,
    Quit,
}

#[derive(Clone)]
pub struct AppController {
    inner: Arc<AppControllerInner>,
}

struct AppControllerInner {
    config: AppConfig,
    snapshot: Arc<Mutex<AppSnapshot>>,
    brightness_tx: Sender<BrightnessCommand>,
    state_tx: Sender<()>,
    state_rx: Mutex<Receiver<()>>,
    tray_update_tx: Arc<Mutex<Option<Sender<()>>>>,
    app_tx: Sender<AppCommand>,
    app_rx: Mutex<Receiver<AppCommand>>,
    tray_available: AtomicBool,
    is_quitting: AtomicBool,
    next_request_id: AtomicU64,
}

impl AppController {
    pub fn new(config: AppConfig) -> Self {
        let backend = create_backend(config.preferred_backend);
        Self::with_backend(config, backend)
    }

    pub fn with_backend(config: AppConfig, backend: Arc<dyn BrightnessBackend>) -> Self {
        let (brightness_tx, brightness_rx) = mpsc::channel();
        let (state_tx, state_rx) = mpsc::channel();
        let (app_tx, app_rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(AppSnapshot::default()));
        let tray_update_tx = Arc::new(Mutex::new(None));

        spawn_brightness_worker(
            backend,
            Arc::clone(&snapshot),
            brightness_rx,
            state_tx.clone(),
            Arc::clone(&tray_update_tx),
            config.normalized_step(),
        );

        Self {
            inner: Arc::new(AppControllerInner {
                config,
                snapshot,
                brightness_tx,
                state_tx,
                state_rx: Mutex::new(state_rx),
                tray_update_tx,
                app_tx,
                app_rx: Mutex::new(app_rx),
                tray_available: AtomicBool::new(false),
                is_quitting: AtomicBool::new(false),
                next_request_id: AtomicU64::new(1),
            }),
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.inner.config
    }

    pub fn snapshot(&self) -> AppSnapshot {
        self.inner
            .snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|_| AppSnapshot {
                displays: Vec::new(),
                last_error: Some("application state lock was poisoned".to_string()),
            })
    }

    pub fn refresh(&self) {
        let _ = self.inner.brightness_tx.send(BrightnessCommand::Refresh);
    }

    pub fn set_brightness(&self, display_id: DisplayId, value: u32) {
        let value = clamp_percent(value);
        let request_id = self.preview_brightness(display_id.clone(), value);
        self.commit_brightness(display_id, value, request_id);
    }

    pub fn adjust_brightness(&self, display_id: DisplayId, delta: i32) {
        if let Some(current) = self.current_brightness(&display_id) {
            self.set_brightness(
                display_id,
                apply_step(current, delta, self.inner.config.normalized_step()),
            );
        } else {
            let _ = self
                .inner
                .brightness_tx
                .send(BrightnessCommand::Adjust { display_id, delta });
        }
    }

    pub fn preview_brightness(&self, display_id: DisplayId, value: u32) -> u64 {
        let request_id = self.inner.next_request_id.fetch_add(1, Ordering::Relaxed);
        update_pending_brightness(
            &self.inner.snapshot,
            &display_id,
            clamp_percent(value),
            request_id,
        );
        notify_state_changed(&self.inner.state_tx);
        request_id
    }

    pub fn commit_brightness(&self, display_id: DisplayId, value: u32, request_id: u64) {
        let _ = self.inner.brightness_tx.send(BrightnessCommand::Set {
            display_id,
            value: clamp_percent(value),
            request_id,
        });
    }

    pub fn request_open_window(&self) {
        let _ = self.inner.app_tx.send(AppCommand::OpenWindow);
    }

    pub fn request_hide_window(&self) {
        let _ = self.inner.app_tx.send(AppCommand::HideWindow);
    }

    pub fn request_quit(&self) {
        let _ = self.inner.app_tx.send(AppCommand::Quit);
    }

    pub fn shutdown(&self) {
        self.inner.is_quitting.store(true, Ordering::Relaxed);
        self.set_tray_update_sender(None);
        let _ = self.inner.brightness_tx.send(BrightnessCommand::Shutdown);
    }

    pub fn set_tray_available(&self, available: bool) {
        self.inner
            .tray_available
            .store(available, Ordering::Relaxed);
    }

    pub fn tray_available(&self) -> bool {
        self.inner.tray_available.load(Ordering::Relaxed)
    }

    pub fn is_quitting(&self) -> bool {
        self.inner.is_quitting.load(Ordering::Relaxed)
    }

    fn set_tray_update_sender(&self, update_tx: Option<Sender<()>>) {
        let Ok(mut current) = self.inner.tray_update_tx.lock() else {
            return;
        };
        *current = update_tx;
    }

    fn request_tray_update(&self) {
        notify_tray_changed(&self.inner.tray_update_tx);
    }

    pub fn drain_state_events(&self) -> bool {
        let Ok(rx) = self.inner.state_rx.lock() else {
            return true;
        };
        let mut changed = false;
        while rx.try_recv().is_ok() {
            changed = true;
        }
        changed
    }

    pub fn drain_app_commands(&self) -> Vec<AppCommand> {
        let Ok(rx) = self.inner.app_rx.lock() else {
            return vec![AppCommand::Quit];
        };
        let mut commands = Vec::new();
        while let Ok(command) = rx.try_recv() {
            commands.push(command);
        }
        commands
    }

    fn current_brightness(&self, display_id: &DisplayId) -> Option<u32> {
        self.inner.snapshot.lock().ok().and_then(|snapshot| {
            snapshot
                .displays
                .iter()
                .find(|display| &display.info.id == display_id)
                .and_then(|display| display.info.current)
        })
    }
}

pub fn run() {
    let config = AppConfig::load().unwrap_or_else(|err| {
        eprintln!("Failed to load config, using defaults: {err}");
        AppConfig::default()
    });
    let controller = AppController::new(config);
    controller.refresh();

    let tray_handle = tray::start_tray(controller.clone());
    controller.set_tray_available(tray_handle.is_available());
    controller.set_tray_update_sender(tray_handle.update_sender());
    controller.request_tray_update();

    let start_visible = !controller.config().start_hidden || !controller.tray_available();
    let prefer_x11 = should_prefer_x11_for_hide_to_tray(controller.tray_available());
    let mut result = run_main_window(controller.clone(), start_visible, prefer_x11);

    if result.is_err() && prefer_x11 {
        eprintln!(
            "Failed to start the X11/XWayland UI backend; falling back to the default backend. \
             Hide to Tray may not hide the window on Wayland."
        );
        result = run_main_window(controller.clone(), start_visible, false);
    }

    if let Err(err) = result {
        eprintln!("Failed to run UI: {err}");
    }

    controller.shutdown();
    drop(tray_handle);
}

fn run_main_window(
    controller: AppController,
    start_visible: bool,
    prefer_x11: bool,
) -> eframe::Result {
    let mut native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(APP_TITLE)
            .with_app_id(APP_ID)
            .with_inner_size([560.0, 320.0])
            .with_min_inner_size([420.0, 240.0])
            .with_visible(start_visible),
        centered: true,
        ..Default::default()
    };
    configure_linux_backend(&mut native_options, prefer_x11);

    let app_controller = controller.clone();
    eframe::run_native(
        APP_TITLE,
        native_options,
        Box::new(move |creation_context| {
            creation_context.egui_ctx.request_repaint();
            Ok(Box::new(ui::main_window::MainWindow::new(
                app_controller.clone(),
                start_visible,
            )))
        }),
    )
}

#[cfg(target_os = "linux")]
fn should_prefer_x11_for_hide_to_tray(tray_available: bool) -> bool {
    let display = std::env::var("DISPLAY").ok();
    should_prefer_x11_for_hide_to_tray_with_display(tray_available, display.as_deref())
}

#[cfg(target_os = "linux")]
fn should_prefer_x11_for_hide_to_tray_with_display(
    tray_available: bool,
    display: Option<&str>,
) -> bool {
    tray_available && display.is_some_and(|display| !display.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn should_prefer_x11_for_hide_to_tray(_tray_available: bool) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn configure_linux_backend(native_options: &mut eframe::NativeOptions, prefer_x11: bool) {
    if !prefer_x11 {
        return;
    }

    // winit cannot hide and later re-show a Wayland window. X11/XWayland supports
    // the reversible unmap/map behavior needed for a tray-first desktop app.
    native_options.event_loop_builder = Some(Box::new(|builder| {
        use winit::platform::x11::EventLoopBuilderExtX11;

        builder.with_x11();
    }));
}

#[cfg(not(target_os = "linux"))]
fn configure_linux_backend(_native_options: &mut eframe::NativeOptions, _prefer_x11: bool) {}

fn spawn_brightness_worker(
    backend: Arc<dyn BrightnessBackend>,
    snapshot: Arc<Mutex<AppSnapshot>>,
    rx: Receiver<BrightnessCommand>,
    state_tx: Sender<()>,
    tray_update_tx: Arc<Mutex<Option<Sender<()>>>>,
    step: u32,
) {
    thread::spawn(move || {
        refresh_snapshot(backend.as_ref(), &snapshot);
        notify_state_changed(&state_tx);
        notify_tray_changed(&tray_update_tx);

        while let Ok(command) = rx.recv() {
            match command {
                BrightnessCommand::Refresh => refresh_snapshot(backend.as_ref(), &snapshot),
                BrightnessCommand::Set {
                    display_id,
                    value,
                    request_id,
                } => set_display_brightness(
                    backend.as_ref(),
                    &snapshot,
                    &display_id,
                    value,
                    request_id,
                ),
                BrightnessCommand::Adjust { display_id, delta } => {
                    adjust_display_brightness(backend.as_ref(), &snapshot, &display_id, delta, step)
                }
                BrightnessCommand::Shutdown => break,
            }
            notify_state_changed(&state_tx);
            notify_tray_changed(&tray_update_tx);
        }
    });
}

fn notify_state_changed(state_tx: &Sender<()>) {
    let _ = state_tx.send(());
}

fn notify_tray_changed(tray_update_tx: &Arc<Mutex<Option<Sender<()>>>>) {
    let Ok(current) = tray_update_tx.lock() else {
        return;
    };
    if let Some(update_tx) = current.as_ref() {
        let _ = update_tx.send(());
    }
}

fn refresh_snapshot(backend: &dyn BrightnessBackend, snapshot: &Arc<Mutex<AppSnapshot>>) {
    let result = backend.list_displays();
    let Ok(mut snapshot) = snapshot.lock() else {
        return;
    };

    match result {
        Ok(displays) => {
            let previous_errors = snapshot
                .displays
                .iter()
                .filter_map(|display| {
                    display
                        .error
                        .as_ref()
                        .map(|error| (display.info.id.clone(), error.clone()))
                })
                .collect::<HashMap<_, _>>();

            snapshot.displays = displays
                .into_iter()
                .map(|info| {
                    let error = previous_errors.get(&info.id).cloned();
                    DisplayState {
                        info,
                        pending: false,
                        error,
                        pending_request_id: 0,
                    }
                })
                .collect();
            snapshot.last_error = None;
        }
        Err(error) => {
            snapshot.last_error = Some(error.to_string());
            for display in &mut snapshot.displays {
                display.pending = false;
            }
        }
    }
}

fn set_display_brightness(
    backend: &dyn BrightnessBackend,
    snapshot: &Arc<Mutex<AppSnapshot>>,
    display_id: &DisplayId,
    value: u32,
    request_id: u64,
) {
    mark_pending(snapshot, display_id, true, None);
    let result = backend
        .set_brightness(display_id, value)
        .and_then(|_| backend.get_brightness(display_id));
    apply_brightness_result(snapshot, display_id, result, request_id);
}

fn adjust_display_brightness(
    backend: &dyn BrightnessBackend,
    snapshot: &Arc<Mutex<AppSnapshot>>,
    display_id: &DisplayId,
    delta: i32,
    step: u32,
) {
    let current = snapshot
        .lock()
        .ok()
        .and_then(|snapshot| {
            snapshot
                .displays
                .iter()
                .find(|display| &display.info.id == display_id)
                .and_then(|display| display.info.current)
        })
        .map(Ok)
        .unwrap_or_else(|| {
            backend
                .get_brightness(display_id)
                .map(BrightnessValue::percent)
        });

    match current {
        Ok(current) => {
            let value = apply_step(current, delta, step);
            update_pending_brightness(snapshot, display_id, value, 0);
            set_display_brightness(backend, snapshot, display_id, value, 0);
        }
        Err(error) => apply_brightness_result(snapshot, display_id, Err(error), 0),
    }
}

fn mark_pending(
    snapshot: &Arc<Mutex<AppSnapshot>>,
    display_id: &DisplayId,
    pending: bool,
    error: Option<String>,
) {
    let Ok(mut snapshot) = snapshot.lock() else {
        return;
    };

    if let Some(display) = snapshot
        .displays
        .iter_mut()
        .find(|display| &display.info.id == display_id)
    {
        display.pending = pending;
        display.error = error;
    }
}

fn update_pending_brightness(
    snapshot: &Arc<Mutex<AppSnapshot>>,
    display_id: &DisplayId,
    value: u32,
    request_id: u64,
) {
    let Ok(mut snapshot) = snapshot.lock() else {
        return;
    };

    if let Some(display) = snapshot
        .displays
        .iter_mut()
        .find(|display| &display.info.id == display_id)
    {
        display.info.current = Some(clamp_percent(value));
        display.pending = true;
        display.error = None;
        display.pending_request_id = request_id;
    }
}

fn apply_brightness_result(
    snapshot: &Arc<Mutex<AppSnapshot>>,
    display_id: &DisplayId,
    result: Result<BrightnessValue>,
    request_id: u64,
) {
    let Ok(mut snapshot) = snapshot.lock() else {
        return;
    };

    match result {
        Ok(value) => {
            if let Some(display) = snapshot
                .displays
                .iter_mut()
                .find(|display| &display.info.id == display_id)
            {
                if request_id < display.pending_request_id {
                    return;
                }
                display.info.current = Some(value.percent());
                display.pending = false;
                display.error = None;
                display.pending_request_id = 0;
            }
        }
        Err(error) => {
            if let Some(display) = snapshot
                .displays
                .iter_mut()
                .find(|display| &display.info.id == display_id)
            {
                if request_id < display.pending_request_id {
                    return;
                }
                display.pending = false;
                display.error = Some(error.to_string());
                display.pending_request_id = 0;
            } else {
                snapshot.last_error = Some(error.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brightness::{BrightnessError, UnsupportedBackend};
    use std::time::Duration;

    #[test]
    fn backend_error_updates_state_without_panic() {
        let snapshot = Arc::new(Mutex::new(AppSnapshot::default()));
        let backend = UnsupportedBackend::new("test backend unavailable");

        refresh_snapshot(&backend, &snapshot);

        let snapshot = snapshot.lock().map(|snapshot| snapshot.clone());
        assert!(snapshot.is_ok());
        let snapshot = snapshot.map_err(|_| BrightnessError::Other("poisoned".into()));
        assert!(snapshot.is_ok());
        let snapshot = snapshot.unwrap_or_default();
        assert!(snapshot.last_error.is_some());
        assert!(snapshot.displays.is_empty());
    }

    #[test]
    fn controller_can_request_tray_update_event() {
        let controller = AppController::with_backend(
            AppConfig::default(),
            Arc::new(UnsupportedBackend::new("test backend unavailable")),
        );
        let (update_tx, update_rx) = mpsc::channel();

        controller.set_tray_update_sender(Some(update_tx));
        controller.request_tray_update();

        assert!(update_rx.recv_timeout(Duration::from_secs(1)).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn x11_hide_backend_is_used_only_when_tray_and_display_are_available() {
        assert!(should_prefer_x11_for_hide_to_tray_with_display(
            true,
            Some(":0")
        ));
        assert!(!should_prefer_x11_for_hide_to_tray_with_display(
            false,
            Some(":0")
        ));
        assert!(!should_prefer_x11_for_hide_to_tray_with_display(true, None));
        assert!(!should_prefer_x11_for_hide_to_tray_with_display(
            true,
            Some("")
        ));
    }
}
