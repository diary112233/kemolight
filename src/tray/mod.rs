use std::sync::mpsc::Sender;

use crate::app::AppController;

#[cfg(all(target_os = "linux", feature = "tray"))]
pub mod linux;
pub mod mock;

pub struct TrayHandle {
    available: bool,
    update_tx: Option<Sender<()>>,
    shutdown: Option<Box<dyn FnOnce()>>,
}

impl TrayHandle {
    pub fn unavailable() -> Self {
        Self {
            available: false,
            update_tx: None,
            shutdown: None,
        }
    }

    pub fn available(update_tx: Sender<()>, shutdown: impl FnOnce() + 'static) -> Self {
        Self {
            available: true,
            update_tx: Some(update_tx),
            shutdown: Some(Box::new(shutdown)),
        }
    }

    pub fn is_available(&self) -> bool {
        self.available
    }

    pub fn update_sender(&self) -> Option<Sender<()>> {
        self.update_tx.clone()
    }

    pub fn request_update(&self) -> bool {
        self.update_tx
            .as_ref()
            .is_some_and(|update_tx| update_tx.send(()).is_ok())
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn unavailable_tray_update_is_noop() {
        let handle = TrayHandle::unavailable();

        assert!(!handle.is_available());
        assert!(handle.update_sender().is_none());
        assert!(!handle.request_update());
    }

    #[test]
    fn available_tray_update_sends_event() {
        let (tx, rx) = mpsc::channel();
        let handle = TrayHandle::available(tx, || {});

        assert!(handle.is_available());
        assert!(handle.update_sender().is_some());
        assert!(handle.request_update());
        assert!(rx.try_recv().is_ok());
    }
}

pub fn start_tray(controller: AppController) -> TrayHandle {
    #[cfg(all(target_os = "linux", feature = "tray"))]
    {
        linux::start_tray(controller)
    }

    #[cfg(not(all(target_os = "linux", feature = "tray")))]
    {
        mock::start_tray(controller)
    }
}
