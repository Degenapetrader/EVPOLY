// Prevents an extra console window when launching the packaged desktop app on
// Windows. Keep this enabled for both debug and release bundles.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    app_lib::run();
}
