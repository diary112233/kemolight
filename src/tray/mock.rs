use crate::app::AppController;

use super::TrayHandle;

pub fn start_tray(_controller: AppController) -> TrayHandle {
    TrayHandle::unavailable()
}
