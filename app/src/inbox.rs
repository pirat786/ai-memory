use aimem_core::db::Database;
use aimem_core::inbox::{export_mirror, import_configured, ImportReport, InboxSettings, MIRROR_FILENAME};
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
    std::thread::spawn(move || {
        let mut last_mirror_ts: Option<String> = None;
        loop {
            if let Ok(report) = import_configured() {
                if report.imported > 0 {
                    let _ = handle.emit("memory-synced", ());
                }
            }
            if let Ok(settings) = InboxSettings::load() {
                if let Some(dir) = settings.directory {
                    if let Ok(db) = Database::open_read_only_default() {
                        let current_ts = db.max_updated_at().ok().flatten();
                        let missing = !dir.join(MIRROR_FILENAME).is_file();
                        if missing || current_ts != last_mirror_ts {
                            if export_mirror(&db, &dir).is_ok() {
                                last_mirror_ts = current_ts;
                            }
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}
