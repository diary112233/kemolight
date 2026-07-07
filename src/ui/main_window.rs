use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

use eframe::egui::{self, RichText, Slider, ViewportCommand};

use crate::{
    app::{AppCommand, AppController, DisplayState},
    brightness::DisplayId,
};

const PENDING_REPAINT_INTERVAL: Duration = Duration::from_millis(100);
const SLIDER_DEBOUNCE: Duration = Duration::from_millis(250);
const DISPLAY_NAME_WIDTH: f32 = 170.0;
const PERCENT_WIDTH: f32 = 48.0;
const SLIDER_WIDTH: f32 = 210.0;
const STEP_BUTTON_SIZE: f32 = 24.0;

pub struct MainWindow {
    controller: AppController,
    visible: bool,
    quitting: bool,
    commits: DebouncedBrightnessCommits,
}

impl MainWindow {
    pub fn new(controller: AppController, visible: bool) -> Self {
        Self {
            controller,
            visible,
            quitting: false,
            commits: DebouncedBrightnessCommits::default(),
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if !close_requested {
            return;
        }

        if self.quitting || !self.controller.tray_available() {
            self.quit(ctx);
        } else {
            self.hide_to_tray(ctx, true);
        }
    }

    fn handle_app_commands(&mut self, ctx: &egui::Context) {
        for command in self.controller.drain_app_commands() {
            match command {
                AppCommand::OpenWindow => self.show_window(ctx),
                AppCommand::HideWindow => self.hide_to_tray(ctx, false),
                AppCommand::Quit => self.quit(ctx),
            }
        }
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context, cancel_close: bool) {
        if !self.controller.tray_available() {
            self.quit(ctx);
            return;
        }

        self.visible = false;
        self.commits.clear_active();

        if cancel_close {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn quit(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }

        self.quitting = true;
        for (display_id, commit) in self.commits.drain_all() {
            self.controller
                .commit_brightness(display_id, commit.value, commit.request_id);
        }
        self.controller.shutdown();
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    fn process_pending_commits(&mut self, now: Instant, ctx: &egui::Context) {
        for (display_id, commit) in self.commits.drain_ready(now) {
            self.controller
                .commit_brightness(display_id, commit.value, commit.request_id);
            ctx.request_repaint();
        }
    }

    fn schedule_repaint(&self, now: Instant, ctx: &egui::Context) {
        if let Some(delay) = self.commits.next_due(now) {
            ctx.request_repaint_after(delay.min(SLIDER_DEBOUNCE));
            return;
        }

        if self
            .controller
            .snapshot()
            .displays
            .iter()
            .any(|display| display.pending)
        {
            ctx.request_repaint_after(PENDING_REPAINT_INTERVAL);
        }
    }

    fn queue_slider_value(&mut self, display_id: DisplayId, value: u32) {
        let value = value.min(100);
        let request_id = self
            .controller
            .preview_brightness(display_id.clone(), value);
        self.commits.queue(
            display_id,
            value,
            request_id,
            Instant::now(),
            SLIDER_DEBOUNCE,
        );
    }

    fn render_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.controller.refresh();
                ui.ctx().request_repaint();
            }

            let hide_label = if self.controller.tray_available() {
                "Hide to Tray"
            } else {
                "Quit"
            };
            if ui.button(hide_label).clicked() {
                if self.controller.tray_available() {
                    self.hide_to_tray(ui.ctx(), false);
                } else {
                    self.quit(ui.ctx());
                }
            }

            if ui.button("Quit").clicked() {
                self.quit(ui.ctx());
            }
        });
    }

    fn render_display_row(&mut self, ui: &mut egui::Ui, display: DisplayState) {
        let display_id = display.info.id.clone();
        let name = display.info.name;
        let kind = display.info.kind.label();
        let enabled = display.info.supported;
        let mut value = display.info.current.unwrap_or(0).min(100);

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(DISPLAY_NAME_WIDTH);
                ui.add_sized(
                    [DISPLAY_NAME_WIDTH, 18.0],
                    egui::Label::new(name).truncate(),
                );
                ui.add_sized(
                    [DISPLAY_NAME_WIDTH, 16.0],
                    egui::Label::new(
                        RichText::new(kind)
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    )
                    .truncate(),
                );
            });

            let percent = display
                .info
                .current
                .map(|current| format!("{}%", current.min(100)))
                .unwrap_or_else(|| "-".to_string());
            ui.add_sized([PERCENT_WIDTH, 20.0], egui::Label::new(percent));

            ui.scope(|ui| {
                ui.spacing_mut().slider_width = SLIDER_WIDTH;
                let response =
                    ui.add_enabled(enabled, Slider::new(&mut value, 0..=100).show_value(false));

                if response.drag_started() {
                    self.commits.mark_active(display_id.clone());
                }
                if response.changed() {
                    self.queue_slider_value(display_id.clone(), value);
                }
                if response.drag_stopped() {
                    self.commits
                        .mark_inactive(&display_id, Instant::now(), SLIDER_DEBOUNCE);
                }
            });

            if step_button(ui, enabled, "−").clicked() {
                self.controller.adjust_brightness(display_id.clone(), -1);
                ui.ctx().request_repaint();
            }
            if step_button(ui, enabled, "+").clicked() {
                self.controller.adjust_brightness(display_id, 1);
                ui.ctx().request_repaint();
            }

            ui.allocate_ui_with_layout(
                [18.0, 20.0].into(),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if display.pending {
                        ui.spinner();
                    }
                },
            );
        });

        if let Some(error) = display.error {
            ui.small(RichText::new(error).color(ui.visuals().error_fg_color));
        }
    }
}

fn step_button(ui: &mut egui::Ui, enabled: bool, label: &'static str) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized(
            [STEP_BUTTON_SIZE, STEP_BUTTON_SIZE],
            egui::Button::new(label),
        )
    })
    .inner
}

impl eframe::App for MainWindow {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_close_request(ctx);
        self.handle_app_commands(ctx);

        if self.controller.drain_state_events() {
            ctx.request_repaint();
        }

        let now = Instant::now();
        if !ctx.input(|input| input.pointer.primary_down()) {
            self.commits.release_active(now, SLIDER_DEBOUNCE);
        }
        self.process_pending_commits(now, ctx);
        self.schedule_repaint(now, ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.spacing_mut().item_spacing = [8.0, 4.0].into();

            self.render_toolbar(ui);
            ui.separator();

            let snapshot = self.controller.snapshot();
            if let Some(error) = snapshot.last_error {
                ui.small(RichText::new(error).color(ui.visuals().error_fg_color));
                ui.separator();
            }

            if snapshot.displays.is_empty() {
                ui.label(
                    RichText::new("No displays detected").color(ui.visuals().weak_text_color()),
                );
                return;
            }

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for display in snapshot.displays {
                        self.render_display_row(ui, display);
                        ui.separator();
                    }
                });
        });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingBrightnessCommit {
    value: u32,
    request_id: u64,
    due_at: Instant,
}

#[derive(Default)]
struct DebouncedBrightnessCommits {
    pending: HashMap<DisplayId, PendingBrightnessCommit>,
    active: HashSet<DisplayId>,
}

impl DebouncedBrightnessCommits {
    fn queue(
        &mut self,
        display_id: DisplayId,
        value: u32,
        request_id: u64,
        now: Instant,
        debounce: Duration,
    ) {
        self.pending.insert(
            display_id,
            PendingBrightnessCommit {
                value: value.min(100),
                request_id,
                due_at: now + debounce,
            },
        );
    }

    fn mark_active(&mut self, display_id: DisplayId) {
        self.active.insert(display_id);
    }

    fn mark_inactive(&mut self, display_id: &DisplayId, now: Instant, debounce: Duration) {
        self.active.remove(display_id);
        if let Some(commit) = self.pending.get_mut(display_id) {
            commit.due_at = now + debounce;
        }
    }

    fn release_active(&mut self, now: Instant, debounce: Duration) {
        for display_id in self.active.drain() {
            if let Some(commit) = self.pending.get_mut(&display_id) {
                commit.due_at = now + debounce;
            }
        }
    }

    fn clear_active(&mut self) {
        self.active.clear();
    }

    fn drain_ready(&mut self, now: Instant) -> Vec<(DisplayId, PendingBrightnessCommit)> {
        let ready = self
            .pending
            .iter()
            .filter_map(|(display_id, commit)| {
                if commit.due_at <= now && !self.active.contains(display_id) {
                    Some(display_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        ready
            .into_iter()
            .filter_map(|display_id| {
                self.pending
                    .remove(&display_id)
                    .map(|commit| (display_id, commit))
            })
            .collect()
    }

    fn drain_all(&mut self) -> Vec<(DisplayId, PendingBrightnessCommit)> {
        self.active.clear();
        self.pending.drain().collect()
    }

    fn next_due(&self, now: Instant) -> Option<Duration> {
        self.pending
            .iter()
            .filter(|(display_id, _)| !self.active.contains(*display_id))
            .map(|(_, commit)| commit.due_at.saturating_duration_since(now))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_keeps_only_last_pending_value() {
        let mut commits = DebouncedBrightnessCommits::default();
        let display_id = DisplayId::new("display-1");
        let now = Instant::now();

        commits.queue(display_id.clone(), 25, 1, now, SLIDER_DEBOUNCE);
        commits.queue(
            display_id.clone(),
            75,
            2,
            now + Duration::from_millis(50),
            SLIDER_DEBOUNCE,
        );

        assert!(commits
            .drain_ready(now + Duration::from_millis(249))
            .is_empty());

        let ready = commits.drain_ready(now + Duration::from_millis(301));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, display_id);
        assert_eq!(ready[0].1.value, 75);
        assert_eq!(ready[0].1.request_id, 2);
    }

    #[test]
    fn active_drag_does_not_commit_until_released() {
        let mut commits = DebouncedBrightnessCommits::default();
        let display_id = DisplayId::new("display-1");
        let now = Instant::now();

        commits.mark_active(display_id.clone());
        commits.queue(display_id.clone(), 10, 1, now, SLIDER_DEBOUNCE);
        assert!(commits
            .drain_ready(now + Duration::from_millis(500))
            .is_empty());

        let release_at = now + Duration::from_millis(500);
        commits.mark_inactive(&display_id, release_at, SLIDER_DEBOUNCE);
        assert!(commits
            .drain_ready(release_at + Duration::from_millis(249))
            .is_empty());

        let ready = commits.drain_ready(release_at + Duration::from_millis(251));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].1.value, 10);
    }

    #[test]
    fn release_active_unblocks_pending_commit() {
        let mut commits = DebouncedBrightnessCommits::default();
        let display_id = DisplayId::new("display-1");
        let now = Instant::now();

        commits.mark_active(display_id.clone());
        commits.queue(display_id.clone(), 40, 1, now, SLIDER_DEBOUNCE);
        commits.release_active(now + Duration::from_millis(300), SLIDER_DEBOUNCE);

        let ready = commits.drain_ready(now + Duration::from_millis(551));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].0, display_id);
        assert_eq!(ready[0].1.value, 40);
    }
}
