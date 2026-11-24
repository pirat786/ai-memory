use crate::db::{save_config, AppConfig, Database, MergeStats, SyncSnapshot};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::Mutex;
use std::time::Duration as StdDuration;
use thiserror::Error;
use tiny_http::{Response, Server};
use url::Url;
use uuid::Uuid;

const SYNC_FILENAME: &str = "ai-memory-sync.json.gz";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
static SYNC_LOCK: Mutex<()> = Mutex::new(());

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(StdDuration::from_secs(10))
        .timeout(StdDuration::from_secs(30))
        .build()
}

#[derive(Error, Debug)]
pub enum SyncError {
    #[error("Google Client ID is not configured. Set it in Web UI -> Sync or AIMEM_GOOGLE_CLIENT_ID env var.")]
    MissingClientId,
    #[error("Not authenticated with Google Drive. Run `aimem auth google` or connect via Web UI.")]
    NotAuthenticated,
    #[error("OAuth error: {0}")]
    OAuth(String),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("DB error: {0}")]
    Db(#[from] crate::db::DbError),
}

pub type Result<T> = std::result::Result<T, SyncError>;

#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub merged: MergeStats,
    pub uploaded_memories: usize,
    pub uploaded_projects: usize,
    pub synced_at: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct DriveFileList {
    files: Vec<DriveFile>,
}

#[derive(Deserialize)]
struct DriveFile {
    id: String,
    #[serde(rename = "md5Checksum")]
    md5_checksum: Option<String>,
}

pub fn resolve_client_creds(cfg: &AppConfig) -> Result<(String, Option<String>)> {
    let client_id = std::env::var("AIMEM_GOOGLE_CLIENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| cfg.google_client_id.clone())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| option_env!("AIMEM_GOOGLE_CLIENT_ID").map(str::to_string))
        .filter(|s| !s.trim().is_empty())
        .ok_or(SyncError::MissingClientId)?;

    let client_secret = std::env::var("AIMEM_GOOGLE_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| cfg.google_client_secret.clone())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| option_env!("AIMEM_GOOGLE_CLIENT_SECRET").map(str::to_string))
        .filter(|s| !s.trim().is_empty());

    Ok((client_id, client_secret))
}

pub fn authenticate_google(cfg: &mut AppConfig) -> Result<()> {
    let (client_id, client_secret) = resolve_client_creds(cfg)?;

    let server = Server::http("127.0.0.1:0").map_err(|e| SyncError::OAuth(e.to_string()))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or_else(|| SyncError::OAuth("Failed to bind callback port".into()))?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let verifier_raw = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier_raw.as_bytes()));
    let state = Uuid::new_v4().simple().to_string();

    let mut auth_url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|e| SyncError::OAuth(e.to_string()))?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", DRIVE_SCOPE)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");

    println!("Opening browser for Google Drive authorization...");
    if let Err(e) = open_browser(auth_url.as_str()) {
        eprintln!("Could not open browser automatically ({e}). Open this URL manually:\n{auth_url}");
    }

    let request = server
        .recv_timeout(std::time::Duration::from_secs(180))
        .map_err(|e| SyncError::OAuth(format!("Callback receive failed: {e}")))?
        .ok_or_else(|| SyncError::OAuth("Authorization timed out (3 minutes)".into()))?;

    let req_url = format!("http://127.0.0.1:{port}{}", request.url());
    let parsed = Url::parse(&req_url).map_err(|e| SyncError::OAuth(e.to_string()))?;

    let mut code = None;
    let mut got_state = None;
    let mut oauth_err = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.to_string()),
            "state" => got_state = Some(v.to_string()),
            "error" => oauth_err = Some(v.to_string()),
            _ => {}
        }
    }

    if let Some(err) = oauth_err {
        let _ = request.respond(Response::from_string(format!("Authorization failed: {err}")));
        return Err(SyncError::OAuth(err));
    }

    if got_state.as_deref() != Some(&state) {
        let _ = request.respond(Response::from_string("State mismatch."));
        return Err(SyncError::OAuth("OAuth state mismatch".into()));
    }

    let code = code.ok_or_else(|| SyncError::OAuth("Missing authorization code".into()))?;
    let html = "<html><body style='font-family:sans-serif;background:#111;color:#eee;padding:40px'><h2>ai-memory connected to Google Drive</h2><p>You can close this tab and return to the app.</p></body></html>";
    let resp = Response::from_string(html).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
    );
    let _ = request.respond(resp);

    let mut form = vec![
        ("code", code.as_str()),
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier_raw.as_str()),
    ];
    if let Some(ref secret) = client_secret {
        form.push(("client_secret", secret.as_str()));
    }

    let agent = http_agent();
    let token: TokenResponse = agent
        .post("https://oauth2.googleapis.com/token")
        .send_form(&form)
        .map_err(|e| SyncError::Http(e.to_string()))?
        .into_json()
        .map_err(|e| SyncError::OAuth(e.to_string()))?;

    let expires_in = token.expires_in.unwrap_or(3500);
    cfg.google_access_token = Some(token.access_token);
    cfg.google_token_expires_at = Some((Utc::now() + Duration::seconds(expires_in - 60)).to_rfc3339());
    if let Some(rt) = token.refresh_token {
        cfg.google_refresh_token = Some(rt);
    }
    save_config(cfg)?;
    Ok(())
}

pub fn ensure_access_token(cfg: &mut AppConfig) -> Result<String> {
    ensure_access_token_with_agent(&http_agent(), cfg)
}

fn ensure_access_token_with_agent(agent: &ureq::Agent, cfg: &mut AppConfig) -> Result<String> {
    if let (Some(tok), Some(exp_str)) = (&cfg.google_access_token, &cfg.google_token_expires_at) {
        if let Ok(exp) = DateTime::parse_from_rfc3339(exp_str) {
            if Utc::now() < exp.with_timezone(&Utc) {
                return Ok(tok.clone());
            }
        }
    }

    let refresh_token = cfg
        .google_refresh_token
        .clone()
        .ok_or(SyncError::NotAuthenticated)?;
    let (client_id, client_secret) = resolve_client_creds(cfg)?;

    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    if let Some(ref secret) = client_secret {
        form.push(("client_secret", secret.as_str()));
    }

    let token: TokenResponse = agent
        .post("https://oauth2.googleapis.com/token")
        .send_form(&form)
        .map_err(|e| SyncError::Http(e.to_string()))?
        .into_json()
        .map_err(|e| SyncError::OAuth(e.to_string()))?;

    let expires_in = token.expires_in.unwrap_or(3500);
    cfg.google_access_token = Some(token.access_token.clone());
    cfg.google_token_expires_at = Some((Utc::now() + Duration::seconds(expires_in - 60)).to_rfc3339());
    save_config(cfg)?;
    Ok(token.access_token)
}

pub fn sync_with_drive(db: &mut Database, cfg: &mut AppConfig) -> Result<SyncReport> {
    let _guard = SYNC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let agent = http_agent();
    let mut token = ensure_access_token_with_agent(&agent, cfg)?;
    let mut auth_header = format!("Bearer {token}");

    let (mut file_id, mut remote_md5) = match cfg.drive_file_id.clone() {
        Some(id) => match get_drive_file_meta(&agent, &auth_header, &id) {
            Ok(Some(f)) => (Some(f.id), f.md5_checksum),
            Ok(None) => (None, None),
            Err(SyncError::Http(msg)) if msg.contains("401") => {
                cfg.google_access_token = None;
                token = ensure_access_token_with_agent(&agent, cfg)?;
                auth_header = format!("Bearer {token}");
                match get_drive_file_meta(&agent, &auth_header, &id)? {
                    Some(f) => (Some(f.id), f.md5_checksum),
                    None => (None, None),
                }
            }
            Err(e) => return Err(e),
        },
        None => match find_drive_file(&agent, &auth_header) {
            Ok(Some(f)) => (Some(f.id), f.md5_checksum),
            Ok(None) => (None, None),
            Err(SyncError::Http(msg)) if msg.contains("401") => {
                cfg.google_access_token = None;
                token = ensure_access_token_with_agent(&agent, cfg)?;
                auth_header = format!("Bearer {token}");
                match find_drive_file(&agent, &auth_header)? {
                    Some(f) => (Some(f.id), f.md5_checksum),
                    None => (None, None),
                }
            }
            Err(e) => return Err(e),
        },
    };

    let local_max_ts = db.max_updated_at()?;
    let local_unchanged = match (&local_max_ts, &cfg.last_synced_local_ts) {
        (Some(cur), Some(prev)) => cur <= prev,
        (None, Some(_)) => true,
        _ => false,
    };
    let remote_unchanged = file_id.is_some()
        && remote_md5.is_some()
        && remote_md5 == cfg.last_remote_md5;

    if local_unchanged && remote_unchanged {
        let now = Utc::now().to_rfc3339();
        cfg.last_sync_at = Some(now.clone());
        save_config(cfg)?;
        return Ok(SyncReport {
            merged: MergeStats::default(),
            uploaded_memories: 0,
            uploaded_projects: 0,
            synced_at: now,
        });
    }

    let mut merged = MergeStats::default();
    if let Some(ref fid) = file_id {
        if !remote_unchanged {
            if let Some(remote_snap) = download_snapshot(&agent, &auth_header, fid)? {
                merged = db.merge_snapshot(&remote_snap)?;
            } else {
                file_id = None;
                remote_md5 = None;
                cfg.drive_file_id = None;
            }
        }
    }

    // Skip upload if nothing changed locally and remote already matches
    let local_after_merge = db.max_updated_at()?;
    let local_still_unchanged = match (&local_after_merge, &cfg.last_synced_local_ts) {
        (Some(cur), Some(prev)) => cur <= prev,
        (None, _) => true,
        _ => false,
    };
    if file_id.is_some() && local_still_unchanged && merged.memories_added == 0 && merged.memories_updated == 0 && merged.projects_merged == 0 {
        let now = Utc::now().to_rfc3339();
        cfg.drive_file_id = file_id;
        cfg.last_remote_md5 = remote_md5;
        cfg.last_synced_local_ts = local_after_merge;
        cfg.last_sync_at = Some(now.clone());
        save_config(cfg)?;
        return Ok(SyncReport {
            merged,
            uploaded_memories: 0,
            uploaded_projects: 0,
            synced_at: now,
        });
    }

    // Re-check remote md5 right before upload to avoid overwriting concurrent changes from another PC
    if let Some(ref fid) = file_id {
        if let Ok(Some(latest_meta)) = get_drive_file_meta(&agent, &auth_header, fid) {
            if latest_meta.md5_checksum != remote_md5 {
                if let Some(remote_snap) = download_snapshot(&agent, &auth_header, fid)? {
                    let extra = db.merge_snapshot(&remote_snap)?;
                    merged.projects_merged += extra.projects_merged;
                    merged.memories_added += extra.memories_added;
                    merged.memories_updated += extra.memories_updated;
                }
            }
        }
    }

    let device = cfg
        .device_name
        .clone()
        .unwrap_or_else(default_device_name);
    let local_snap = db.export_snapshot(&device)?;
    let uploaded_memories = local_snap.memories.len();
    let uploaded_projects = local_snap.projects.len();

    let gz_bytes = gzip_json(&local_snap)?;
    let uploaded_file = upload_snapshot(&agent, &auth_header, file_id.as_deref(), &gz_bytes)?;

    let now = Utc::now().to_rfc3339();
    cfg.drive_file_id = Some(uploaded_file.id);
    cfg.last_remote_md5 = uploaded_file.md5_checksum;
    cfg.last_synced_local_ts = db.max_updated_at()?;
    cfg.last_sync_at = Some(now.clone());
    save_config(cfg)?;

    Ok(SyncReport {
        merged,
        uploaded_memories,
        uploaded_projects,
        synced_at: now,
    })
}

fn find_drive_file(agent: &ureq::Agent, auth_header: &str) -> Result<Option<DriveFile>> {
    let mut url = Url::parse("https://www.googleapis.com/drive/v3/files")
        .map_err(|e| SyncError::Http(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("spaces", "appDataFolder")
        .append_pair("q", &format!("name='{SYNC_FILENAME}' and trashed=false"))
        .append_pair("fields", "files(id,md5Checksum)");

    let list: DriveFileList = agent
        .get(url.as_str())
        .set("Authorization", auth_header)
        .call()
        .map_err(|e| SyncError::Http(e.to_string()))?
        .into_json()
        .map_err(|e| SyncError::Http(e.to_string()))?;

    Ok(list.files.into_iter().next())
}

fn get_drive_file_meta(agent: &ureq::Agent, auth_header: &str, file_id: &str) -> Result<Option<DriveFile>> {
    let url = format!("https://www.googleapis.com/drive/v3/files/{file_id}?fields=id,md5Checksum,trashed");
    match agent.get(&url).set("Authorization", auth_header).call() {
        Ok(r) => {
            let file: DriveFile = r.into_json().map_err(|e| SyncError::Http(e.to_string()))?;
            Ok(Some(file))
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(SyncError::Http(e.to_string())),
    }
}

fn download_snapshot(agent: &ureq::Agent, auth_header: &str, file_id: &str) -> Result<Option<SyncSnapshot>> {
    let url = format!("https://www.googleapis.com/drive/v3/files/{file_id}?alt=media");
    let resp = match agent.get(&url).set("Authorization", auth_header).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(404, _)) => return Ok(None),
        Err(e) => return Err(SyncError::Http(e.to_string())),
    };

    let mut bytes = Vec::new();
    resp.into_reader().read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut decoder = GzDecoder::new(&bytes[..]);
    let mut json_str = String::new();
    decoder.read_to_string(&mut json_str)?;
    let snap: SyncSnapshot = serde_json::from_str(&json_str)?;
    Ok(Some(snap))
}

fn upload_snapshot(
    agent: &ureq::Agent,
    auth_header: &str,
    existing_id: Option<&str>,
    gz_bytes: &[u8],
) -> Result<DriveFile> {
    let boundary = format!("aimem_boundary_{}", Uuid::new_v4().simple());
    let metadata = if existing_id.is_some() {
        serde_json::json!({ "name": SYNC_FILENAME }).to_string()
    } else {
        serde_json::json!({ "name": SYNC_FILENAME, "parents": ["appDataFolder"] }).to_string()
    };

    let mut body = Vec::new();
    write!(
        body,
        "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{boundary}\r\nContent-Type: application/gzip\r\n\r\n"
    )?;
    body.extend_from_slice(gz_bytes);
    write!(body, "\r\n--{boundary}--\r\n")?;

    let content_type = format!("multipart/related; boundary={boundary}");

    let resp = if let Some(fid) = existing_id {
        let url = format!("https://www.googleapis.com/upload/drive/v3/files/{fid}?uploadType=multipart&fields=id,md5Checksum");
        agent
            .patch(&url)
            .set("Authorization", auth_header)
            .set("Content-Type", &content_type)
            .send_bytes(&body)
            .map_err(|e| SyncError::Http(e.to_string()))?
    } else {
        let url = "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&fields=id,md5Checksum";
        agent
            .post(url)
            .set("Authorization", auth_header)
            .set("Content-Type", &content_type)
            .send_bytes(&body)
            .map_err(|e| SyncError::Http(e.to_string()))?
    };

    let file: DriveFile = resp.into_json().map_err(|e| SyncError::Http(e.to_string()))?;
    Ok(file)
}

fn gzip_json(snap: &SyncSnapshot) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(snap)?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&json)?;
    Ok(encoder.finish()?)
}

pub fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "my-pc".to_string())
}

pub fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
        Ok(())
    }
}
