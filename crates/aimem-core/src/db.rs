use chrono::{Duration as ChronoDuration, Utc};
use crate::inbox::InboxEntry;
use rusqlite::{params, Connection, ErrorCode, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    InvalidInput(String),
}

impl DbError {
    pub fn is_access_failure(&self) -> bool {
        match self {
            Self::Io(err) => err.kind() == std::io::ErrorKind::PermissionDenied,
            Self::Sql(rusqlite::Error::SqliteFailure(err, _)) => matches!(
                err.code,
                ErrorCode::CannotOpen | ErrorCode::ReadOnly | ErrorCode::PermissionDenied
            ),
            _ => false,
        }
    }
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
pub struct CompactReport {
    pub size_before: u64,
    pub size_after: u64,
    pub tombstones_scrubbed: usize,
    pub tombstones_purged: usize,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
    pub google_refresh_token: Option<String>,
    pub google_access_token: Option<String>,
    pub google_token_expires_at: Option<String>,
    pub drive_file_id: Option<String>,
    pub last_sync_at: Option<String>,
    pub last_remote_md5: Option<String>,
    pub last_synced_local_ts: Option<String>,
    pub last_compact_at: Option<String>,
    pub device_name: Option<String>,
    #[serde(default = "default_true")]
    pub auto_sync: bool,
    pub auto_import_done: bool,
    pub autostart_configured: bool,
    pub scan_dirs: Vec<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            google_client_id: None,
            google_client_secret: None,
            google_refresh_token: None,
            google_access_token: None,
            google_token_expires_at: None,
            drive_file_id: None,
            last_sync_at: None,
            last_remote_md5: None,
            last_synced_local_ts: None,
            last_compact_at: None,
            device_name: None,
            auto_sync: true,
            auto_import_done: false,
            autostart_configured: false,
            scan_dirs: Vec::new(),
        }
    }
}

pub fn should_auto_compact(cfg: &AppConfig) -> bool {
    let Some(ref ts) = cfg.last_compact_at else {
        return true;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return true;
    };
    Utc::now() - parsed.with_timezone(&Utc) >= ChronoDuration::hours(24)
}

pub fn mark_compacted(cfg: &mut AppConfig) {
    cfg.last_compact_at = Some(Utc::now().to_rfc3339());
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
    let target = config_path();
    let tmp = dir.join(format!("config.json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &target)?;
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

    pub fn open_read_only_default() -> Result<Self> {
        Self::open_read_only(&data_dir().join("memory.db"))
    }

    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let version = |conn: &Connection| {
            conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        };
        if version(&self.conn)? >= 2 {
            return Ok(());
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = version(&tx)?;
        if current < 1 {
            Self::create_initial_schema(&tx)?;
        }
        if current < 2 {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS inbox_receipts (id TEXT PRIMARY KEY);",
            )?;
            tx.pragma_update(None, "user_version", 2)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn create_initial_schema(tx: &Transaction<'_>) -> Result<()> {
        tx.execute_batch(
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
        tx.execute(
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

    pub fn list_projects_with_counts(&self) -> Result<Vec<(Project, usize)>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.git_remote, p.summary, p.created_at, p.updated_at,
                    COUNT(m.id) AS mem_count
             FROM projects p
             LEFT JOIN memories m ON m.project_id = p.id AND m.deleted_at IS NULL
             GROUP BY p.id
             ORDER BY CASE WHEN p.id = 'global' THEN 0 ELSE 1 END, p.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let count: i64 = row.get(6)?;
            Ok((
                Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    git_remote: row.get(2)?,
                    summary: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                },
                count.max(0) as usize,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn count_memories(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE deleted_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn max_updated_at(&self) -> Result<Option<String>> {
        let mem_max: Option<String> = self
            .conn
            .query_row("SELECT MAX(updated_at) FROM memories", [], |row| row.get(0))?;
        let proj_max: Option<String> = self
            .conn
            .query_row("SELECT MAX(updated_at) FROM projects", [], |row| row.get(0))?;
        Ok(match (mem_max, proj_max) {
            (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        })
    }

    pub fn list_memories_filtered(
        &self,
        project_id: Option<&str>,
        kind: Option<&str>,
        agent: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Memory>> {
        let mut sql = String::from(
            "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
             FROM memories WHERE deleted_at IS NULL",
        );
        let mut owned_params: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(pid) = project_id {
            sql.push_str(" AND project_id = ?");
            owned_params.push(pid.to_string().into());
        }
        if let Some(k) = kind.filter(|s| !s.trim().is_empty() && *s != "all") {
            sql.push_str(" AND kind = ?");
            owned_params.push(normalize_kind(k).into());
        }
        if let Some(a) = agent.filter(|s| !s.trim().is_empty() && *s != "all") {
            sql.push_str(" AND source_agent = ?");
            owned_params.push(a.trim().to_string().into());
        }
        sql.push_str(" ORDER BY updated_at DESC LIMIT ? OFFSET ?");
        owned_params.push((limit as i64).into());
        owned_params.push((offset as i64).into());

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(owned_params), row_to_memory)?;
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
        self.add_entry(&InboxEntry::new(
            project_id.to_owned(), kind.to_owned(), content.to_owned(), tags.to_owned(),
            source_agent.to_owned(),
        ))
    }

    pub fn find_active_duplicate(&self, project_id: &str, content: &str) -> Result<Option<Memory>> {
        self.conn
            .query_row(
                "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
                 FROM memories
                 WHERE deleted_at IS NULL AND project_id = ?1 AND TRIM(content) = ?2
                 LIMIT 1",
                params![project_id, content.trim()],
                row_to_memory,
            )
            .optional()
            .map_err(DbError::from)
    }

    pub fn add_entry_dedup(&self, entry: &InboxEntry) -> Result<(Memory, bool)> {
        if let Some(existing) = self.find_active_duplicate(&entry.project_id, &entry.content)? {
            return Ok((existing, false));
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        Self::insert_entry(&tx, entry)?;
        let memory = tx.query_row(
            "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
             FROM memories WHERE id = ?1",
            [&entry.id],
            row_to_memory,
        )?;
        tx.commit()?;
        Ok((memory, true))
    }

    pub fn add_entry(&self, entry: &InboxEntry) -> Result<Memory> {
        let (memory, _) = self.add_entry_dedup(entry)?;
        Ok(memory)
    }

    pub(crate) fn import_inbox_entry(&mut self, entry: &InboxEntry) -> Result<bool> {
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let accepted = tx.execute(
            "INSERT INTO inbox_receipts (id) VALUES (?1) ON CONFLICT(id) DO NOTHING",
            [&entry.id],
        )?;
        if accepted == 0 {
            return Ok(false);
        }

        let name = entry.project_id.rsplit('/').next().unwrap_or(&entry.project_id);
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO projects (id, name, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(id) DO NOTHING",
            params![entry.project_id, name, now],
        )?;
        let inserted = Self::insert_entry(&tx, entry)?;
        tx.commit()?;
        Ok(inserted)
    }

    fn insert_entry(tx: &Transaction<'_>, entry: &InboxEntry) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let inserted = tx.execute(
            "INSERT INTO memories (id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, NULL) ON CONFLICT(id) DO NOTHING",
            params![
                entry.id, entry.project_id, normalize_kind(&entry.kind), entry.content.trim(),
                entry.tags.trim(), entry.source_agent.trim(), now
            ],
        )? > 0;
        if inserted {
            tx.execute(
                "UPDATE projects SET updated_at = ?1 WHERE id = ?2",
                params![now, entry.project_id],
            )?;
        }
        Ok(inserted)
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

    fn resolve_active_memory_id(&self, id_or_prefix: &str) -> Result<Option<String>> {
        let trimmed = id_or_prefix.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let exact: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM memories WHERE id = ?1 AND deleted_at IS NULL",
                params![trimmed],
                |row| row.get(0),
            )
            .optional()?;
        if exact.is_some() {
            return Ok(exact);
        }
        if trimmed.len() < 4 {
            return Err(DbError::InvalidInput(
                "ID prefix must be at least 4 characters.".to_string(),
            ));
        }
        let pattern = format!("{}%", escape_like(trimmed));
        let mut stmt = self.conn.prepare(
            "SELECT id FROM memories WHERE deleted_at IS NULL AND id LIKE ?1 ESCAPE '\\' LIMIT 5",
        )?;
        let matches: Vec<String> = stmt
            .query_map(params![pattern], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            _ => Err(DbError::InvalidInput(format!(
                "Ambiguous ID prefix '{trimmed}': matches {} memories (e.g. {}). Provide more characters.",
                matches.len(),
                matches.iter().map(|s| s.get(..8).unwrap_or(s)).collect::<Vec<_>>().join(", ")
            ))),
        }
    }

    pub fn update_memory_by_prefix(
        &self,
        id_or_prefix: &str,
        kind: Option<&str>,
        content: Option<&str>,
        tags: Option<&str>,
    ) -> Result<Option<Memory>> {
        let Some(full_id) = self.resolve_active_memory_id(id_or_prefix)? else {
            return Ok(None);
        };
        self.update_memory(&full_id, kind, content, tags)
    }

    pub fn soft_delete_memory_by_prefix(&self, id_or_prefix: &str) -> Result<Option<String>> {
        let Some(full_id) = self.resolve_active_memory_id(id_or_prefix)? else {
            return Ok(None);
        };
        self.soft_delete_memory(&full_id)?;
        Ok(Some(full_id))
    }

    pub fn gc_deleted(&self, older_than_days: i64) -> Result<usize> {
        let cutoff = (Utc::now() - ChronoDuration::days(older_than_days.max(0))).to_rfc3339();
        let removed = self.conn.execute(
            "DELETE FROM memories WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            params![cutoff],
        )?;
        Ok(removed)
    }

    /// Scrubs payload from soft-deleted tombstones (preserving sync IDs/timestamps),
    /// rebuilds/optimizes FTS5, checkpoints WAL, and runs VACUUM.
    pub fn compact(&mut self, db_path: &Path, purge_deleted_days: Option<i64>) -> Result<CompactReport> {
        let size_before = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
        let tombstones_purged = match purge_deleted_days {
            Some(days) => self.gc_deleted(days)?,
            None => 0,
        };
        let tombstones_scrubbed = self.conn.execute(
            "UPDATE memories SET content = '', tags = ''
             WHERE deleted_at IS NOT NULL AND (content != '' OR tags != '')",
            [],
        )?;
        self.conn.execute_batch(
            "INSERT INTO memories_fts(memories_fts) VALUES('rebuild');
             INSERT INTO memories_fts(memories_fts) VALUES('optimize');
             PRAGMA wal_checkpoint(TRUNCATE);
             VACUUM;",
        )?;
        let size_after = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
        Ok(CompactReport {
            size_before,
            size_after,
            tombstones_scrubbed,
            tombstones_purged,
        })
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

    pub fn list_all_active_memories(&self) -> Result<Vec<Memory>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
             FROM memories
             WHERE deleted_at IS NULL
             ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_memory)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn search_memories(&self, query: &str, project_id: Option<&str>, limit: usize) -> Result<Vec<Memory>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.list_memories(project_id, limit);
        }
        let fts_query = build_fts_query(trimmed);
        let limit_i = limit as i64;
        let mut results = Vec::new();

        if !fts_query.is_empty() {
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
        }

        // Fallback LIKE search when query has only special chars or FTS syntax failed
        let like = format!("%{}%", escape_like(trimmed));
        if let Some(pid) = project_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
                 FROM memories
                 WHERE deleted_at IS NULL
                   AND (project_id = ?2 OR project_id = 'global')
                   AND (content LIKE ?1 ESCAPE '\\' OR tags LIKE ?1 ESCAPE '\\')
                 ORDER BY updated_at DESC
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![like, pid, limit_i], row_to_memory)?;
            for r in rows {
                results.push(r?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at
                 FROM memories
                 WHERE deleted_at IS NULL
                   AND (content LIKE ?1 ESCAPE '\\' OR tags LIKE ?1 ESCAPE '\\')
                 ORDER BY updated_at DESC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![like, limit_i], row_to_memory)?;
            for r in rows {
                results.push(r?);
            }
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

        let existing_projects: HashMap<String, String> = {
            let mut stmt = tx.prepare("SELECT id, updated_at FROM projects")?;
            let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<HashMap<_, _>, _>>()?
        };

        {
            let mut ins_proj = tx.prepare_cached(
                "INSERT INTO projects (id, name, git_remote, summary, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut upd_proj = tx.prepare_cached(
                "UPDATE projects SET name = ?1, git_remote = COALESCE(?2, git_remote), summary = ?3, updated_at = ?4
                 WHERE id = ?5",
            )?;

            for p in &snap.projects {
                match existing_projects.get(&p.id) {
                    None => {
                        ins_proj.execute(params![p.id, p.name, p.git_remote, p.summary, p.created_at, p.updated_at])?;
                        stats.projects_merged += 1;
                    }
                    Some(ts) if p.updated_at > *ts => {
                        upd_proj.execute(params![p.name, p.git_remote, p.summary, p.updated_at, p.id])?;
                        stats.projects_merged += 1;
                    }
                    _ => {}
                }
            }
        }

        let existing_memories: HashMap<String, String> = {
            let mut stmt = tx.prepare("SELECT id, updated_at FROM memories")?;
            let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            rows.collect::<std::result::Result<HashMap<_, _>, _>>()?
        };

        {
            let mut ins_mem = tx.prepare_cached(
                "INSERT INTO memories (id, project_id, kind, content, tags, source_agent, created_at, updated_at, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut upd_mem = tx.prepare_cached(
                "UPDATE memories
                 SET project_id = ?1, kind = ?2, content = ?3, tags = ?4, source_agent = ?5, updated_at = ?6, deleted_at = ?7
                 WHERE id = ?8",
            )?;

            for m in &snap.memories {
                match existing_memories.get(&m.id) {
                    None => {
                        ins_mem.execute(params![
                            m.id,
                            m.project_id,
                            m.kind,
                            m.content,
                            m.tags,
                            m.source_agent,
                            m.created_at,
                            m.updated_at,
                            m.deleted_at
                        ])?;
                        stats.memories_added += 1;
                    }
                    Some(ts) if m.updated_at > *ts => {
                        upd_mem.execute(params![
                            m.project_id,
                            m.kind,
                            m.content,
                            m.tags,
                            m.source_agent,
                            m.updated_at,
                            m.deleted_at,
                            m.id
                        ])?;
                        stats.memories_updated += 1;
                    }
                    _ => {}
                }
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

fn escape_like(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
    use uuid::Uuid;

    #[test]
    fn crud_search_and_merge() {
        let dir = std::env::temp_dir().join(format!("aimem_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let db1 = Database::open(&dir.join("db1.db")).unwrap();
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

        // Test count_memories, list_projects_with_counts, prefix delete, and LIKE fallback filter
        let m2 = db1.add_memory(&proj.id, "note", "C++ & Rust % special", "", "user").unwrap();
        let other = db1.ensure_project("local/other", "other", None).unwrap();
        db1.add_memory(&other.id, "note", "C++ % other project", "", "user").unwrap();

        assert_eq!(db1.count_memories().unwrap(), 3);
        let with_counts = db1.list_projects_with_counts().unwrap();
        assert_eq!(with_counts.iter().find(|(p, _)| p.id == proj.id).unwrap().1, 1);

        let special_hits = db1.search_memories("%", Some(&proj.id), 10).unwrap();
        assert_eq!(special_hits.len(), 1);
        assert_eq!(special_hits[0].id, m2.id);

        let prefix = &m2.id[..8];
        assert_eq!(db1.soft_delete_memory_by_prefix(prefix).unwrap(), Some(m2.id));
        assert_eq!(db1.count_memories().unwrap(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_dedup_and_compact() {
        let dir = std::env::temp_dir().join(format!("aimem_compact_test_{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("memory.db");

        let mut db = Database::open(&db_path).unwrap();
        let proj = db.ensure_project("local/demo", "demo", None).unwrap();

        let e1 = InboxEntry::new(
            proj.id.clone(),
            "decision".into(),
            "Use WAL mode".into(),
            "sqlite".into(),
            "claude".into(),
        );
        let (m1, created1) = db.add_entry_dedup(&e1).unwrap();
        assert!(created1);

        let e2 = InboxEntry::new(
            proj.id.clone(),
            "decision".into(),
            "  Use WAL mode  ".into(),
            "sqlite".into(),
            "codex".into(),
        );
        let (m2, created2) = db.add_entry_dedup(&e2).unwrap();
        assert!(!created2);
        assert_eq!(m1.id, m2.id);

        let to_del = db
            .add_memory(&proj.id, "note", "Temporary large payload", "tmp", "user")
            .unwrap();
        db.soft_delete_memory(&to_del.id).unwrap();

        let ro = Database::open_read_only(&db_path).unwrap();
        assert_eq!(ro.count_memories().unwrap(), 1);
        assert_eq!(ro.search_memories("WAL", Some(&proj.id), 10).unwrap().len(), 1);
        drop(ro);

        let report = db.compact(&db_path, None).unwrap();
        assert_eq!(report.tombstones_scrubbed, 1);
        assert_eq!(report.tombstones_purged, 0);
        assert!(report.size_after > 0);

        // Tombstone remains with deleted_at preserved and payload scrubbed
        let tombstone = db.get_memory(&to_del.id).unwrap().unwrap();
        assert!(tombstone.deleted_at.is_some());
        assert_eq!(tombstone.content, "");
        assert_eq!(tombstone.tags, "");

        // Active memory still searchable via FTS5 after rebuild
        assert_eq!(db.search_memories("WAL", Some(&proj.id), 10).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
