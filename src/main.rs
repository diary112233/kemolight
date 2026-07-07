#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    let Some(_single_instance) = kemolight::single_instance::SingleInstance::acquire() else {
        return;
    };

    kemolight::app::run();
}
