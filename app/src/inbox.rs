use aimem_core::inbox::{import_configured, ImportReport, InboxSettings};
use std::path::PathBuf;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[tauri::command]
pub fn get_inbox_settings() -> Result<InboxSettings, String> {
    InboxSettings::load().map_err(|err| err.to_string())
}

#[tauri::command]
pub fn save_inbox_settings(directory: String, projects: Vec<String>) -> Result<(), String> {
    let path = (!directory.trim().is_empty()).then(|| PathBuf::from(directory.trim()));
    InboxSettings::save(path, projects).map_err(|err| err.to_string())
}

#[tauri::command]
pub async fn import_inbox_now() -> Result<ImportReport, String> {
    tauri::async_runtime::spawn_blocking(|| import_configured().map_err(|err| err.to_string()))
        .await
        .map_err(|err| err.to_string())?
}

pub fn start(handle: AppHandle) {
    std::thread::spawn(move || loop {
        if let Ok(report) = import_configured() {
            if report.imported > 0 {
                let _ = handle.emit("memory-synced", ());
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}
