use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub git_remote: Option<String>,
    pub summary: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub content: String,
    pub tags: String,
    pub source_agent: String,
    pub created_at: String,
    pub updated_at: String,
    pub deleted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSnapshot {
    pub version: u32,
    pub exported_at: String,
    pub device_name: String,
    pub projects: Vec<Project>,
    pub memories: Vec<Memory>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MergeStats {
    pub projects_merged: usize,
    pub memories_added: usize,
    pub memories_updated: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub google_refresh_token: Option<String>,
    pub google_access_token: Option<String>,
    pub google_token_expires_at: Option<String>,
    pub drive_file_id: Option<String>,
    pub last_sync_at: Option<String>,
    pub device_name: Option<String>,
    pub auto_sync: bool,
    pub auto_import_done: bool,
}

pub fn data_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("AIMEM_DATA_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("ai-memory");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("ai-memory");
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".ai-memory")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn load_config() -> AppConfig {
    let path = config_path();
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(cfg) = serde_json::from_slice::<AppConfig>(&bytes) {
            return cfg;
        }
    }
    AppConfig::default()
}

pub fn save_config(cfg: &AppConfig) -> Result<()> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(cfg)?;
    std::fs::write(config_path(), json)?;
    Ok(())
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open_default() -> Result<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir)?;
        Self::open(&dir.join("memory.db"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                git_remote TEXT,
                summary TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'note',
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                source_agent TEXT NOT NULL DEFAULT 'user',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memories_project
                ON memories(project_id, deleted_at, updated_at DESC);

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                content,
                tags,
                kind,
                content='memories',
                content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, content, tags, kind)
                VALUES (new.rowid, new.content, new.tags, new.kind);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, tags, kind)
                VALUES ('delete', old.rowid, old.content, old.tags, old.kind);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, content, tags, kind)
                VALUES ('delete', old.rowid, old.content, old.tags, old.kind);
                INSERT INTO memories_fts(rowid, content, tags, kind)
                VALUES (new.rowid, new.content, new.tags, new.kind);
            END;
            "#,
        )?;

        // Ensure built-in global project exists
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO projects (id, name, git_remote, summary, created_at, updated_at)
             VALUES ('global', 'Global Preferences', NULL, 'Shared user preferences, style, and cross-project rules.', ?1, ?1)",
            params![now],
        )?;
        Ok(())
    }

    pub fn ensure_project(&self, id: &str, name: &str, git_remote: Option<&str>) -> Result<Project> {
        let now = Utc::now().to_rfc3339();
        if let Some(existing) = self.get_project(id)? {
            if existing.name != name || (git_remote.is_some() && existing.git_remote.as_deref() != git_remote) {
                self.conn.execute(
                    "UPDATE projects SET name = ?1, git_remote = COALESCE(?2, git_remote), updated_at = ?3 WHERE id = ?4",
                    params![name, git_remote, now, id],
                )?;
                return Ok(self.get_project(id)?.unwrap_or(existing));
            }
            return Ok(existing);
        }

        self.conn.execute(
            "INSERT INTO projects (id, name, git_remote, summary, created_at, updated_at) VALUES (?1, ?2, ?3, '', ?4, ?4)",
            params![id, name, git_remote, now],
        )?;
        Ok(self.get_project(id)?.expect("inserted project"))
    }

    pub fn get_project(&self, id: &str) -> Result<Option<Project>> {
        self.conn
            .query_row(
                "SELECT id, name, git_remote, summary, created_at, updated_at FROM projects WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        git_remote: row.get(2)?,
                        summary: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(DbError::from)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, git_remote, summary, created_at, updated_at
             FROM projects
             ORDER BY CASE WHEN id = 'global' THEN 0 ELSE 1 END, updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                git_remote: row.get(2)?,
                summary: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn update_project_summary(&self, id: &str, summary: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE projects SET summary = ?1, updated_at = ?2 WHERE id = ?3",
            params![summary, now, id],
        )?;
        Ok(changed > 0)
    }

    pub fn add_memory(
        &self,
        project_id: &str,
        kind: &str,
        content: &str,
        tags: &str,
        source_agent: &str,
    ) -> Result<Memory> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let norm_kind = normalize_kind(kind);
        self.conn.execute(
            "INSERT INTO memories (id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, NULL)",
            params![id, project_id, norm_kind, content.trim(), tags.trim(), source_agent.trim(), now],
        )?;
        self.conn.execute(
            "UPDATE projects SET updated_at = ?1 WHERE id = ?2",
            params![now, project_id],
        )?;
        Ok(self.get_memory(&id)?.expect("inserted memory"))
    }

    pub fn get_memory(&self, id: &str) -> Result<Option<Memory>> {
        self.conn
            .query_row(
                "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
                 FROM memories WHERE id = ?1",
                params![id],
                row_to_memory,
            )
            .optional()
            .map_err(DbError::from)
    }

    pub fn update_memory(
        &self,
        id: &str,
        kind: Option<&str>,
        content: Option<&str>,
        tags: Option<&str>,
    ) -> Result<Option<Memory>> {
        let Some(existing) = self.get_memory(id)? else {
            return Ok(None);
        };
        let now = Utc::now().to_rfc3339();
        let new_kind = kind.map(normalize_kind).unwrap_or(existing.kind);
        let new_content = content.map(|s| s.trim().to_string()).unwrap_or(existing.content);
        let new_tags = tags.map(|s| s.trim().to_string()).unwrap_or(existing.tags);
        self.conn.execute(
            "UPDATE memories SET kind = ?1, content = ?2, tags = ?3, updated_at = ?4 WHERE id = ?5",
            params![new_kind, new_content, new_tags, now, id],
        )?;
        self.get_memory(id)
    }

    pub fn soft_delete_memory(&self, id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let changed = self.conn.execute(
            "UPDATE memories SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id],
        )?;
        Ok(changed > 0)
    }

    pub fn list_memories(&self, project_id: Option<&str>, limit: usize) -> Result<Vec<Memory>> {
        let limit_i = limit as i64;
        let mut results = Vec::new();
        if let Some(pid) = project_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
                 FROM memories
                 WHERE deleted_at IS NULL AND project_id = ?1
                 ORDER BY updated_at DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![pid, limit_i], row_to_memory)?;
            for r in rows {
                results.push(r?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
                 FROM memories
                 WHERE deleted_at IS NULL
                 ORDER BY updated_at DESC
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit_i], row_to_memory)?;
            for r in rows {
                results.push(r?);
            }
        }
        Ok(results)
    }

    pub fn search_memories(&self, query: &str, project_id: Option<&str>, limit: usize) -> Result<Vec<Memory>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.list_memories(project_id, limit);
        }
        let fts_query = build_fts_query(trimmed);
        let limit_i = limit as i64;
        let mut results = Vec::new();

        if let Some(pid) = project_id {
            let mut stmt = self.conn.prepare(
                "SELECT m.id, m.project_id, m.kind, m.content, m.tags, m.source_agent, m.created_at, m.updated_at, m.deleted_at
                 FROM memories_fts f
                 JOIN memories m ON m.rowid = f.rowid
                 WHERE memories_fts MATCH ?1 AND m.deleted_at IS NULL AND (m.project_id = ?2 OR m.project_id = 'global')
                 ORDER BY bm25(memories_fts), m.updated_at DESC
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![fts_query, pid, limit_i], row_to_memory);
            if let Ok(mapped) = rows {
                for r in mapped {
                    results.push(r?);
                }
                return Ok(results);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT m.id, m.project_id, m.kind, m.content, m.tags, m.source_agent, m.created_at, m.updated_at, m.deleted_at
                 FROM memories_fts f
                 JOIN memories m ON m.rowid = f.rowid
                 WHERE memories_fts MATCH ?1 AND m.deleted_at IS NULL
                 ORDER BY bm25(memories_fts), m.updated_at DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![fts_query, limit_i], row_to_memory);
            if let Ok(mapped) = rows {
                for r in mapped {
                    results.push(r?);
                }
                return Ok(results);
            }
        }

        // Fallback LIKE search if FTS query syntax had special characters
        let like = format!("%{trimmed}%");
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
             FROM memories
             WHERE deleted_at IS NULL AND (content LIKE ?1 OR tags LIKE ?1)
             ORDER BY updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![like, limit_i], row_to_memory)?;
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    pub fn export_snapshot(&self, device_name: &str) -> Result<SyncSnapshot> {
        let projects = self.list_projects()?;
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
             FROM memories",
        )?;
        let memories = stmt
            .query_map([], row_to_memory)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(SyncSnapshot {
            version: 1,
            exported_at: Utc::now().to_rfc3339(),
            device_name: device_name.to_string(),
            projects,
            memories,
        })
    }

    pub fn merge_snapshot(&mut self, snap: &SyncSnapshot) -> Result<MergeStats> {
        let tx = self.conn.transaction()?;
        let mut stats = MergeStats::default();

        for p in &snap.projects {
            let local_updated: Option<String> = tx
                .query_row(
                    "SELECT updated_at FROM projects WHERE id = ?1",
                    params![p.id],
                    |row| row.get(0),
                )
                .optional()?;

            match local_updated {
                None => {
                    tx.execute(
                        "INSERT INTO projects (id, name, git_remote, summary, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![p.id, p.name, p.git_remote, p.summary, p.created_at, p.updated_at],
                    )?;
                    stats.projects_merged += 1;
                }
                Some(ts) if p.updated_at > ts => {
                    tx.execute(
                        "UPDATE projects SET name = ?1, git_remote = COALESCE(?2, git_remote), summary = ?3, updated_at = ?4
                         WHERE id = ?5",
                        params![p.name, p.git_remote, p.summary, p.updated_at, p.id],
                    )?;
                    stats.projects_merged += 1;
                }
                _ => {}
            }
        }

        for m in &snap.memories {
            let local_updated: Option<String> = tx
                .query_row(
                    "SELECT updated_at FROM memories WHERE id = ?1",
                    params![m.id],
                    |row| row.get(0),
                )
                .optional()?;

            match local_updated {
                None => {
                    tx.execute(
                        "INSERT INTO memories (id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                        params![
                            m.id,
                            m.project_id,
                            m.kind,
                            m.content,
                            m.tags,
                            m.source_agent,
                            m.created_at,
                            m.updated_at,
                            m.deleted_at
                        ],
                    )?;
                    stats.memories_added += 1;
                }
                Some(ts) if m.updated_at > ts => {
                    tx.execute(
                        "UPDATE memories
                         SET project_id = ?1, kind = ?2, content = ?3, tags = ?4, source_agent = ?5, updated_at = ?6, deleted_at = ?7
                         WHERE id = ?8",
                        params![
                            m.project_id,
                            m.kind,
                            m.content,
                            m.tags,
                            m.source_agent,
                            m.updated_at,
                            m.deleted_at,
                            m.id
                        ],
                    )?;
                    stats.memories_updated += 1;
                }
                _ => {}
            }
        }

        tx.commit()?;
        Ok(stats)
    }
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get(0)?,
        project_id: row.get(1)?,
        kind: row.get(2)?,
        content: row.get(3)?,
        tags: row.get(4)?,
        source_agent: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        deleted_at: row.get(8)?,
    })
}

pub fn normalize_kind(kind: &str) -> String {
    match kind.trim().to_lowercase().as_str() {
        "decision" | "dec" => "decision".to_string(),
        "arch" | "architecture" => "arch".to_string(),
        "bug" | "fix" | "gotcha" => "bug".to_string(),
        "pref" | "preference" | "rule" => "pref".to_string(),
        _ => "note".to_string(),
    }
}

fn build_fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .map(|w| {
            let clean: String = w.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
            if clean.is_empty() {
                String::new()
            } else {
                format!("{clean}*")
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" AND ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crud_search_and_merge() {
        let dir = std::env::temp_dir().join(format!("aimem_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut db1 = Database::open(&dir.join("db1.db")).unwrap();
        let proj = db1
            .ensure_project("github.com/user/demo", "demo", Some("https://github.com/user/demo.git"))
            .unwrap();
        let m1 = db1
            .add_memory(&proj.id, "decision", "Use SQLite FTS5 for instant search", "sqlite,search", "antigravity")
            .unwrap();
        db1.add_memory("global", "pref", "Always write concise Rust tests", "rust", "claude")
            .unwrap();

        let found = db1.search_memories("FTS5", Some(&proj.id), 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, m1.id);

        let snap = db1.export_snapshot("pc-1").unwrap();

        let mut db2 = Database::open(&dir.join("db2.db")).unwrap();
        let stats = db2.merge_snapshot(&snap).unwrap();
        assert_eq!(stats.memories_added, 2);

        db1.soft_delete_memory(&m1.id).unwrap();
        let snap2 = db1.export_snapshot("pc-1").unwrap();
        let stats2 = db2.merge_snapshot(&snap2).unwrap();
        assert_eq!(stats2.memories_updated, 1);
        assert_eq!(db2.list_memories(Some(&proj.id), 10).unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
