pub mod app;
pub mod brightness;
pub mod config;
#[cfg(target_os = "windows")]
mod display_events;
pub mod logo;
pub mod profiles;
pub mod single_instance;
pub mod tray;
pub mod ui;
