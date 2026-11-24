use crate::db::{data_dir, load_config, save_config, Database};
use crate::rules::install_global_rules;
use crate::sync::{authenticate_google, default_device_name, open_browser, sync_with_drive};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server};
use url::Url;

const INDEX_HTML: &str = include_str!("../ui/index.html");

#[derive(Serialize)]
struct ProjectWithCount {
    id: String,
    name: String,
    git_remote: Option<String>,
    summary: String,
    updated_at: String,
    count: usize,
}

#[derive(Deserialize)]
struct AddMemoryReq {
    project_id: Option<String>,
    kind: Option<String>,
    content: String,
    tags: Option<String>,
    source_agent: Option<String>,
}

#[derive(Deserialize)]
struct ConfigReq {
    device_name: Option<String>,
    google_client_id: Option<String>,
    google_client_secret: Option<String>,
}

pub fn serve(port: u16, open: bool) -> Result<(), String> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|e| format!("Failed to bind {addr}: {e}"))?;
    let url = format!("http://{addr}");
    println!("ai-memory UI running at {url} (Ctrl+C to stop)");
    if open {
        let _ = open_browser(&url);
    }

    for req in server.incoming_requests() {
        let method = req.method().clone();
        let raw_url = req.url().to_string();
        if let Err(e) = handle_request(req, method, raw_url) {
            eprintln!("HTTP handler error: {e}");
        }
    }
    Ok(())
}

fn handle_request(mut req: Request, method: Method, raw_url: String) -> Result<(), String> {
    let parsed = Url::parse(&format!("http://localhost{raw_url}")).map_err(|e| e.to_string())?;
    let path = parsed.path().to_string();

    if method == Method::Get && path == "/" {
        let mut resp = Response::from_string(INDEX_HTML);
        resp.add_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        );
        return req.respond(resp).map_err(|e| e.to_string());
    }

    if method == Method::Get && path == "/api/status" {
        let db = match Database::open_default() {
            Ok(d) => d,
            Err(e) => return respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
        let cfg = load_config();
        let projects = db.list_projects().unwrap_or_default();
        let memories = db.list_memories(None, 10000).unwrap_or_default();
        let status = serde_json::json!({
            "data_dir": data_dir().display().to_string(),
            "projects_count": projects.len(),
            "memories_count": memories.len(),
            "google_client_id": cfg.google_client_id,
            "google_connected": cfg.google_refresh_token.is_some(),
            "last_sync_at": cfg.last_sync_at,
            "device_name": cfg.device_name.unwrap_or_else(default_device_name),
        });
        return respond_json(req, 200, &status);
    }

    if method == Method::Get && path == "/api/projects" {
        let db = match Database::open_default() {
            Ok(d) => d,
            Err(e) => return respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
        let projects = db.list_projects().unwrap_or_default();
        let mut out = Vec::with_capacity(projects.len());
        for p in projects {
            let count = db.list_memories(Some(&p.id), 10000).map(|v| v.len()).unwrap_or(0);
            out.push(ProjectWithCount {
                id: p.id,
                name: p.name,
                git_remote: p.git_remote,
                summary: p.summary,
                updated_at: p.updated_at,
                count,
            });
        }
        return respond_json(req, 200, &out);
    }

    if method == Method::Get && path == "/api/memories" {
        let db = match Database::open_default() {
            Ok(d) => d,
            Err(e) => return respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
        let mut project_id = None;
        let mut query = None;
        for (k, v) in parsed.query_pairs() {
            match k.as_ref() {
                "project_id" if !v.is_empty() => project_id = Some(v.to_string()),
                "q" if !v.is_empty() => query = Some(v.to_string()),
                _ => {}
            }
        }
        let items = if let Some(q) = query {
            db.search_memories(&q, project_id.as_deref(), 200)
        } else {
            db.list_memories(project_id.as_deref(), 200)
        };
        return match items {
            Ok(list) => respond_json(req, 200, &list),
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    if method == Method::Post && path == "/api/memories" {
        let body: AddMemoryReq = match read_json(&mut req) {
            Ok(b) => b,
            Err(e) => return respond_json(req, 400, &serde_json::json!({ "error": e })),
        };
        let db = match Database::open_default() {
            Ok(d) => d,
            Err(e) => return respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
        let pid = body.project_id.unwrap_or_else(|| "global".to_string());
        if db.get_project(&pid).ok().flatten().is_none() {
            let _ = db.ensure_project(&pid, &pid, None);
        }
        return match db.add_memory(
            &pid,
            body.kind.as_deref().unwrap_or("note"),
            &body.content,
            body.tags.as_deref().unwrap_or(""),
            body.source_agent.as_deref().unwrap_or("user"),
        ) {
            Ok(m) => respond_json(req, 200, &m),
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    if method == Method::Delete && path.starts_with("/api/memories/") {
        let id = path.trim_start_matches("/api/memories/").to_string();
        let db = match Database::open_default() {
            Ok(d) => d,
            Err(e) => return respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
        return match db.soft_delete_memory(&id) {
            Ok(deleted) => respond_json(req, 200, &serde_json::json!({ "deleted": deleted })),
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    if method == Method::Post && path == "/api/config" {
        let body: ConfigReq = match read_json(&mut req) {
            Ok(b) => b,
            Err(e) => return respond_json(req, 400, &serde_json::json!({ "error": e })),
        };
        let mut cfg = load_config();
        if let Some(d) = body.device_name {
            cfg.device_name = if d.trim().is_empty() { None } else { Some(d.trim().to_string()) };
        }
        if let Some(id) = body.google_client_id {
            cfg.google_client_id = if id.trim().is_empty() { None } else { Some(id.trim().to_string()) };
        }
        if let Some(sec) = body.google_client_secret {
            if !sec.trim().is_empty() {
                cfg.google_client_secret = Some(sec.trim().to_string());
            }
        }
        return match save_config(&cfg) {
            Ok(()) => respond_json(req, 200, &serde_json::json!({ "ok": true })),
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    if method == Method::Post && path == "/api/auth/google" {
        let mut cfg = load_config();
        return match authenticate_google(&mut cfg) {
            Ok(()) => respond_json(req, 200, &serde_json::json!({ "ok": true })),
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    if method == Method::Post && path == "/api/sync" {
        let mut db = match Database::open_default() {
            Ok(d) => d,
            Err(e) => return respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
        let mut cfg = load_config();
        return match sync_with_drive(&mut db, &mut cfg) {
            Ok(report) => respond_json(req, 200, &report),
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    if method == Method::Post && path == "/api/rules/install" {
        return match install_global_rules() {
            Ok(files) => {
                let list: Vec<String> = files.into_iter().map(|p| p.display().to_string()).collect();
                respond_json(req, 200, &serde_json::json!({ "files": list }))
            }
            Err(e) => respond_json(req, 500, &serde_json::json!({ "error": e.to_string() })),
        };
    }

    respond_json(req, 404, &serde_json::json!({ "error": "Not found" }))
}

fn read_json<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T, String> {
    let mut body = String::new();
    req.as_reader()
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn respond_json<T: Serialize>(req: Request, status: u16, value: &T) -> Result<(), String> {
    let body = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let mut resp = Response::from_string(body).with_status_code(status);
    resp.add_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
    req.respond(resp).map_err(|e| e.to_string())
}
