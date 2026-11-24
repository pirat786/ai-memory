use crate::db::{save_config, AppConfig, Database, MergeStats, SyncSnapshot};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use thiserror::Error;
use tiny_http::{Response, Server};
use url::Url;
use uuid::Uuid;

const SYNC_FILENAME: &str = "ai-memory-sync.json.gz";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";

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
        .recv()
        .map_err(|e| SyncError::OAuth(format!("Callback receive failed: {e}")))?;

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

    let token: TokenResponse = ureq::post("https://oauth2.googleapis.com/token")
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

    let token: TokenResponse = ureq::post("https://oauth2.googleapis.com/token")
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
    let token = ensure_access_token(cfg)?;
    let auth_header = format!("Bearer {token}");

    let file_id = match cfg.drive_file_id.clone() {
        Some(id) => Some(id),
        None => find_drive_file_id(&auth_header)?,
    };

    let mut merged = MergeStats::default();
    if let Some(ref fid) = file_id {
        if let Some(remote_snap) = download_snapshot(&auth_header, fid)? {
            merged = db.merge_snapshot(&remote_snap)?;
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
    let new_file_id = upload_snapshot(&auth_header, file_id.as_deref(), &gz_bytes)?;

    let now = Utc::now().to_rfc3339();
    cfg.drive_file_id = Some(new_file_id);
    cfg.last_sync_at = Some(now.clone());
    save_config(cfg)?;

    Ok(SyncReport {
        merged,
        uploaded_memories,
        uploaded_projects,
        synced_at: now,
    })
}

fn find_drive_file_id(auth_header: &str) -> Result<Option<String>> {
    let mut url = Url::parse("https://www.googleapis.com/drive/v3/files")
        .map_err(|e| SyncError::Http(e.to_string()))?;
    url.query_pairs_mut()
        .append_pair("spaces", "appDataFolder")
        .append_pair("q", &format!("name='{SYNC_FILENAME}' and trashed=false"))
        .append_pair("fields", "files(id)");

    let list: DriveFileList = ureq::get(url.as_str())
        .set("Authorization", auth_header)
        .call()
        .map_err(|e| SyncError::Http(e.to_string()))?
        .into_json()
        .map_err(|e| SyncError::Http(e.to_string()))?;

    Ok(list.files.into_iter().next().map(|f| f.id))
}

fn download_snapshot(auth_header: &str, file_id: &str) -> Result<Option<SyncSnapshot>> {
    let url = format!("https://www.googleapis.com/drive/v3/files/{file_id}?alt=media");
    let resp = match ureq::get(&url).set("Authorization", auth_header).call() {
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

fn upload_snapshot(auth_header: &str, existing_id: Option<&str>, gz_bytes: &[u8]) -> Result<String> {
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
        let url = format!("https://www.googleapis.com/upload/drive/v3/files/{fid}?uploadType=multipart");
        ureq::patch(&url)
            .set("Authorization", auth_header)
            .set("Content-Type", &content_type)
            .send_bytes(&body)
            .map_err(|e| SyncError::Http(e.to_string()))?
    } else {
        let url = "https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart";
        ureq::post(url)
            .set("Authorization", auth_header)
            .set("Content-Type", &content_type)
            .send_bytes(&body)
            .map_err(|e| SyncError::Http(e.to_string()))?
    };

    let file: DriveFile = resp.into_json().map_err(|e| SyncError::Http(e.to_string()))?;
    Ok(file.id)
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
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
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
