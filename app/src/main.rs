#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod inbox;

use aimem_core::db::{
    data_dir, load_config, mark_compacted, save_config, should_auto_compact, Database, Memory,
};
use aimem_core::importer::{import_all, ImportReport};
use aimem_core::rules::{ensure_aimem_in_path, install_global_rules};
use aimem_core::sync::{
    authenticate_google, default_device_name, resolve_client_creds, sync_with_drive, SyncReport,
};
use serde::Serialize;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};

#[derive(Serialize)]
struct ProjectWithCount {
    id: String,
    name: String,
    git_remote: Option<String>,
    summary: String,
    updated_at: String,
    count: usize,
}

#[derive(Serialize)]
struct StatusDto {
    version: String,
    data_dir: String,
    projects_count: usize,
    memories_count: usize,
    google_client_id: Option<String>,
    google_connected: bool,
    last_sync_at: Option<String>,
    device_name: String,
    auto_sync: bool,
    scan_dirs: Vec<String>,
}

#[tauri::command]
fn get_status() -> Result<StatusDto, String> {
    let db = Database::open_default().map_err(|e| e.to_string())?;
    let cfg = load_config();
    let projects = db.list_projects().map_err(|e| e.to_string())?;
    let memories_count = db.count_memories().map_err(|e| e.to_string())?;
    let client_id = resolve_client_creds(&cfg).ok().map(|(id, _)| id);
    Ok(StatusDto {
        version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: data_dir().display().to_string(),
        projects_count: projects.len(),
        memories_count,
        google_client_id: client_id,
        google_connected: cfg.google_refresh_token.is_some(),
        last_sync_at: cfg.last_sync_at,
        device_name: cfg.device_name.unwrap_or_else(default_device_name),
        auto_sync: cfg.auto_sync,
        scan_dirs: cfg.scan_dirs,
    })
}

#[tauri::command]
fn list_projects() -> Result<Vec<ProjectWithCount>, String> {
    let db = Database::open_default().map_err(|e| e.to_string())?;
    let projects = db.list_projects_with_counts().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(projects.len());
    for (p, count) in projects {
        if count == 0 && p.id != "global" {
            continue;
        }
        out.push(ProjectWithCount {
            id: p.id,
            name: p.name,
            git_remote: p.git_remote,
            summary: p.summary,
            updated_at: p.updated_at,
            count,
        });
    }
    Ok(out)
}

#[tauri::command]
fn list_memories(
    project_id: Option<String>,
    query: Option<String>,
    kind: Option<String>,
    agent: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<Vec<Memory>, String> {
    let db = Database::open_default().map_err(|e| e.to_string())?;
    let pid = project_id.filter(|s| !s.is_empty() && s != "all");
    let q = query.filter(|s| !s.trim().is_empty());
    let lim = limit.unwrap_or(100).clamp(1, 500);
    let off = offset.unwrap_or(0);
    if let Some(q_str) = q {
        let mut items = db
            .search_memories(&q_str, pid.as_deref(), lim + off)
            .map_err(|e| e.to_string())?;
        if let Some(k) = kind.as_deref().filter(|s| !s.is_empty() && *s != "all") {
            items.retain(|m| m.kind == k);
        }
        if let Some(a) = agent.as_deref().filter(|s| !s.is_empty() && *s != "all") {
            items.retain(|m| m.source_agent == a);
        }
        Ok(items.into_iter().skip(off).take(lim).collect())
    } else {
        db.list_memories_filtered(pid.as_deref(), kind.as_deref(), agent.as_deref(), lim, off)
            .map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn add_memory(
    project_id: Option<String>,
    kind: Option<String>,
    content: String,
    tags: Option<String>,
    source_agent: Option<String>,
) -> Result<Memory, String> {
    let db = Database::open_default().map_err(|e| e.to_string())?;
    let pid = project_id
        .filter(|s| !s.is_empty() && s != "all")
        .unwrap_or_else(|| "global".to_string());
    if db.get_project(&pid).map_err(|e| e.to_string())?.is_none() {
        db.ensure_project(&pid, &pid, None).map_err(|e| e.to_string())?;
    }
    db.add_memory(
        &pid,
        kind.as_deref().unwrap_or("note"),
        &content,
        tags.as_deref().unwrap_or(""),
        source_agent.as_deref().unwrap_or("user"),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_memory(
    id: String,
    kind: Option<String>,
    content: Option<String>,
    tags: Option<String>,
) -> Result<Option<Memory>, String> {
    let db = Database::open_default().map_err(|e| e.to_string())?;
    db.update_memory(&id, kind.as_deref(), content.as_deref(), tags.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_memory(id: String) -> Result<bool, String> {
    let db = Database::open_default().map_err(|e| e.to_string())?;
    db.soft_delete_memory(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn save_settings(
    device_name: Option<String>,
    google_client_id: Option<String>,
    google_client_secret: Option<String>,
    auto_sync: Option<bool>,
    scan_dirs: Option<Vec<String>>,
) -> Result<(), String> {
    let mut cfg = load_config();
    if let Some(d) = device_name {
        cfg.device_name = if d.trim().is_empty() {
            None
        } else {
            Some(d.trim().to_string())
        };
    }
    if let Some(id) = google_client_id {
        cfg.google_client_id = if id.trim().is_empty() {
            None
        } else {
            Some(id.trim().to_string())
        };
    }
    if let Some(sec) = google_client_secret {
        if !sec.trim().is_empty() {
            cfg.google_client_secret = Some(sec.trim().to_string());
        }
    }
    if let Some(v) = auto_sync {
        cfg.auto_sync = v;
    }
    if let Some(dirs) = scan_dirs {
        cfg.scan_dirs = dirs
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    save_config(&cfg).map_err(|e| e.to_string())
}

#[tauri::command]
async fn auth_google() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut cfg = load_config();
        authenticate_google(&mut cfg).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn sync_now() -> Result<SyncReport, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut db = Database::open_default().map_err(|e| e.to_string())?;
        let mut cfg = load_config();
        sync_with_drive(&mut db, &mut cfg).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn install_rules() -> Result<Vec<String>, String> {
    let files = install_global_rules().map_err(|e| e.to_string())?;
    Ok(files.into_iter().map(|p| p.display().to_string()).collect())
}

#[tauri::command]
async fn import_memories(projects_dir: Option<String>) -> Result<ImportReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let db = Database::open_default().map_err(|e| e.to_string())?;
        let p = projects_dir
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        import_all(&db, p.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn disconnect_google() -> Result<(), String> {
    let mut cfg = load_config();
    cfg.google_refresh_token = None;
    cfg.google_access_token = None;
    cfg.google_token_expires_at = None;
    cfg.drive_file_id = None;
    cfg.last_sync_at = None;
    cfg.last_remote_md5 = None;
    cfg.last_synced_local_ts = None;
    save_config(&cfg).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| e.to_string())?;
    } else {
        mgr.disable().map_err(|e| e.to_string())?;
    }
    Ok(mgr.is_enabled().unwrap_or(enabled))
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_updater::UpdaterExt;
    let updater = app.updater().map_err(|e| e.to_string())?;
    if let Some(update) = updater.check().await.map_err(|e| e.to_string())? {
        update
            .download_and_install(|_, _| {}, || {})
            .await
            .map_err(|e| e.to_string())?;
        app.restart();
    }
    Ok(false)
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            get_status,
            list_projects,
            list_memories,
            add_memory,
            update_memory,
            delete_memory,
            save_settings,
            inbox::get_inbox_settings,
            inbox::save_inbox_settings,
            inbox::import_inbox_now,
            auth_google,
            sync_now,
            install_rules,
            import_memories,
            disconnect_google,
            install_update,
            get_autostart,
            set_autostart,
        ])
        .setup(|app| {
            let open_item = MenuItem::with_id(app, "open", "Открыть AI Memory", true, None::<&str>)?;
            let sync_item = MenuItem::with_id(app, "sync", "Синхронизировать сейчас", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open_item, &sync_item, &quit_item])?;

            let mut builder = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("AI Memory")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main_window(app),
                    "sync" => {
                        let handle = app.clone();
                        std::thread::spawn(move || {
                            if let Ok(mut db) = Database::open_default() {
                                let mut cfg = load_config();
                                if sync_with_drive(&mut db, &mut cfg).is_ok() {
                                    let _ = handle.emit("memory-synced", ());
                                }
                            }
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                builder = builder.icon(icon.clone());
            }
            let _tray = builder.build(app)?;

            inbox::start(app.handle().clone());

            // Auto-configure PATH, agent rules, first-run import, and background sync + updater
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let _ = ensure_aimem_in_path();
                let _ = install_global_rules();

                let mut cfg = load_config();
                if !cfg.autostart_configured {
                    use tauri_plugin_autostart::ManagerExt;
                    let _ = handle.autolaunch().enable();
                    cfg.autostart_configured = true;
                    let _ = save_config(&cfg);
                }
                if !cfg.auto_import_done {
                    if let Ok(db) = Database::open_default() {
                        if import_all(&db, None).is_ok() {
                            cfg.auto_import_done = true;
                            let _ = save_config(&cfg);
                            let _ = handle.emit("memory-synced", ());
                        }
                    }
                }

                {
                    let upd_handle = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        use tauri_plugin_updater::UpdaterExt;
                        if let Ok(updater) = upd_handle.updater() {
                            if let Ok(Some(update)) = updater.check().await {
                                let _ = upd_handle.emit("update-available", update.version.clone());
                            }
                        }
                    });
                }

                loop {
                    let mut cfg = load_config();
                    if cfg.google_refresh_token.is_some() && cfg.auto_sync {
                        if let Ok(mut db) = Database::open_default() {
                            if sync_with_drive(&mut db, &mut cfg).is_ok() {
                                let _ = handle.emit("memory-synced", ());
                            }
                        }
                    }
                    if should_auto_compact(&cfg) {
                        if let Ok(mut db) = Database::open_default() {
                            if db.compact(&data_dir().join("memory.db"), Some(90)).is_ok() {
                                mark_compacted(&mut cfg);
                                let _ = save_config(&cfg);
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_secs(300));
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
