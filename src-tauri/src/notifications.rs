#![allow(dead_code)]

use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

pub fn notify(app_handle: &AppHandle, title: &str, body: &str) {
    let _ = app_handle
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show();
}
