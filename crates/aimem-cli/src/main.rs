mod add;

use aimem_core::db::{data_dir, load_config, save_config, Database, Memory, Project};
use aimem_core::importer::import_all;
use aimem_core::inbox::{
    export_mirror, import_configured, load_mirror, InboxSettings, MemoryMirror, MIRROR_FILENAME,
};
use aimem_core::project::detect_project;
use aimem_core::rules::{home_dir, install_global_rules, install_local_rules};
use aimem_core::sync::{authenticate_google, sync_with_drive};
use clap::{Parser, Subcommand};

const MAX_CONTEXT_ENTRY_CHARS: usize = 700;

#[derive(Parser)]
#[command(name = "aimem", version, about = "Unified cross-agent AI memory without MCP")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print global + current project memory context (for AI agents)
    Context {
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
        #[arg(long, default_value_t = 12000)]
        max_chars: usize,
        #[arg(long)]
        json: bool,
    },
    /// Add a new memory entry
    Add(add::AddArgs),
    /// Full-text search across memories (FTS5)
    Search {
        query: String,
        #[arg(long)]
        project_only: bool,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// List projects or memories
    List {
        #[arg(long)]
        projects: bool,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Edit an existing memory entry (supports ID prefix)
    Edit {
        id: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        tags: Option<String>,
    },
    /// Delete a memory entry by ID (supports ID prefix)
    Rm { id: String },
    /// Authenticate with Google Drive (drive.appdata)
    Auth {
        #[arg(long)]
        client_id: Option<String>,
        #[arg(long)]
        client_secret: Option<String>,
    },
    /// Sync local database with Google Drive
    Sync,
    /// Install instructions into Claude Code and Antigravity global/local rules
    InstallRules {
        #[arg(long)]
        local: bool,
    },
    /// Import existing memories from ~/.claude and project folders
    Import {
        #[arg(long)]
        projects_dir: Option<std::path::PathBuf>,
    },
    /// Purge soft-deleted memories older than N days
    Gc {
        #[arg(long, default_value_t = 90)]
        days: i64,
    },
    /// Scrub deleted payloads, optimize FTS5, and VACUUM database
    Compact {
        #[arg(long)]
        purge_deleted_days: Option<i64>,
    },
    /// Inspect or import the shared agent inbox
    Inbox {
        #[command(subcommand)]
        action: InboxAction,
    },
    /// Check binary path, DB access, inbox, rules, and agent permissions
    Doctor,
    /// Show database path and sync status
    Status,
}

#[derive(Subcommand)]
enum InboxAction {
    /// Show configured inbox directory and pending files
    Status,
    /// Import pending inbox requests into SQLite
    Import,
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}… _(truncated; use aimem search)_")
    } else {
        prefix
    }
}

enum ReadStore {
    Db(Database),
    Mirror(MemoryMirror, std::path::PathBuf),
}

impl ReadStore {
    fn get_project(&self, id: &str) -> Result<Option<Project>, String> {
        match self {
            Self::Db(db) => db.get_project(id).map_err(|e| e.to_string()),
            Self::Mirror(m, _) => Ok(m.get_project(id)),
        }
    }

    fn list_projects(&self) -> Result<Vec<Project>, String> {
        match self {
            Self::Db(db) => db.list_projects().map_err(|e| e.to_string()),
            Self::Mirror(m, _) => Ok(m.list_projects()),
        }
    }

    fn list_projects_with_counts(&self) -> Result<Vec<(Project, usize)>, String> {
        match self {
            Self::Db(db) => db.list_projects_with_counts().map_err(|e| e.to_string()),
            Self::Mirror(m, _) => Ok(m.list_projects_with_counts()),
        }
    }

    fn count_memories(&self) -> Result<usize, String> {
        match self {
            Self::Db(db) => db.count_memories().map_err(|e| e.to_string()),
            Self::Mirror(m, _) => Ok(m.count_memories()),
        }
    }

    fn list_memories(&self, project_id: Option<&str>, limit: usize) -> Result<Vec<Memory>, String> {
        match self {
            Self::Db(db) => db.list_memories(project_id, limit).map_err(|e| e.to_string()),
            Self::Mirror(m, _) => Ok(m.list_memories(project_id, limit)),
        }
    }

    fn search_memories(
        &self,
        query: &str,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Memory>, String> {
        match self {
            Self::Db(db) => db
                .search_memories(query, project_id, limit)
                .map_err(|e| e.to_string()),
            Self::Mirror(m, _) => Ok(m.search_memories(query, project_id, limit)),
        }
    }

    fn source_label(&self) -> String {
        match self {
            Self::Db(_) => data_dir().display().to_string(),
            Self::Mirror(_, dir) => format!("mirror ({})", dir.join(MIRROR_FILENAME).display()),
        }
    }
}

fn configured_inbox_dir() -> Option<std::path::PathBuf> {
    std::env::var("AIMEM_INBOX_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| InboxSettings::load().ok().and_then(|s| s.directory))
}

fn open_read_store() -> Result<ReadStore, String> {
    let db_path = data_dir().join("memory.db");
    let db_err = if db_path.exists() {
        match Database::open_read_only(&db_path) {
            Ok(db) => return Ok(ReadStore::Db(db)),
            Err(err) => Some(err.to_string()),
        }
    } else {
        None
    };

    if let Some(dir) = configured_inbox_dir() {
        if let Ok(mirror) = load_mirror(&dir) {
            return Ok(ReadStore::Mirror(mirror, dir));
        }
    }

    match db_err {
        Some(err) => Err(err),
        None => Database::open_default()
            .map(ReadStore::Db)
            .map_err(|e| e.to_string()),
    }
}

fn refresh_mirror_if_configured(db: &Database) {
    if let Some(dir) = configured_inbox_dir() {
        let _ = export_mirror(db, &dir);
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn count_pending_inbox_files(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| {
                    let p = e.path();
                    p.extension().and_then(|s| s.to_str()) == Some("json")
                        && p.file_name().and_then(|s| s.to_str()) != Some(MIRROR_FILENAME)
                })
                .count()
        })
        .unwrap_or(0)
}

fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli.command) {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

fn run(cmd: Commands) -> Result<(), String> {
    match cmd {
        Commands::Context {
            project,
            limit,
            max_chars,
            json,
        } => {
            let store = open_read_store()?;
            let detected = detect_project(None);
            let pid = project.unwrap_or_else(|| detected.id.clone());
            let proj = store
                .get_project(&pid)?
                .unwrap_or_else(|| Project {
                    id: pid.clone(),
                    name: detected.name.clone(),
                    git_remote: detected.git_remote.clone(),
                    summary: String::new(),
                    created_at: String::new(),
                    updated_at: String::new(),
                });

            let global_items = store.list_memories(Some("global"), limit)?;
            let project_items = if pid == "global" {
                Vec::new()
            } else {
                store.list_memories(Some(&pid), limit)?
            };

            if json {
                let out = serde_json::json!({
                    "project": proj,
                    "global": global_items,
                    "memories": project_items,
                });
                println!("{}", serde_json::to_string_pretty(&out).unwrap());
                return Ok(());
            }

            let mut buf = String::new();
            buf.push_str("# AI Memory Context\n");
            buf.push_str(&format!("Project: **{}** (`{}`)\n", proj.name, proj.id));
            if !proj.summary.is_empty() {
                buf.push_str(&format!("Summary: {}\n", proj.summary));
            }
            buf.push('\n');

            let mut used_chars = buf.chars().count();
            let mut omitted = 0usize;
            let global_budget = if max_chars > 0 {
                (max_chars * 35) / 100
            } else {
                usize::MAX
            };
            let mut global_used = 0usize;

            if !global_items.is_empty() {
                let header = "## Global Preferences & Rules\n";
                buf.push_str(header);
                used_chars += header.chars().count();
                for m in &global_items {
                    let snippet = truncate_chars(&m.content, MAX_CONTEXT_ENTRY_CHARS);
                    let line = format!(
                        "- [{}] {} _(id: {}, by {})_\n",
                        m.kind,
                        snippet,
                        short_id(&m.id),
                        m.source_agent
                    );
                    let line_chars = line.chars().count();
                    if (max_chars > 0 && used_chars + line_chars > max_chars)
                        || (max_chars > 0 && global_used + line_chars > global_budget)
                    {
                        omitted += 1;
                        continue;
                    }
                    used_chars += line_chars;
                    global_used += line_chars;
                    buf.push_str(&line);
                }
                buf.push('\n');
                used_chars += 1;
            }

            let proj_header = "## Project Memory\n";
            buf.push_str(proj_header);
            used_chars += proj_header.chars().count();
            if project_items.is_empty() {
                buf.push_str("_(no project memories yet)_\n");
            } else {
                for m in &project_items {
                    let tags = if m.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [tags: {}]", m.tags)
                    };
                    let snippet = truncate_chars(&m.content, MAX_CONTEXT_ENTRY_CHARS);
                    let line = format!(
                        "- [{}] {}{} _(id: {}, by {})_\n",
                        m.kind,
                        snippet,
                        tags,
                        short_id(&m.id),
                        m.source_agent
                    );
                    let line_chars = line.chars().count();
                    if max_chars > 0 && used_chars + line_chars > max_chars {
                        omitted += 1;
                        continue;
                    }
                    used_chars += line_chars;
                    buf.push_str(&line);
                }
            }
            if omitted > 0 {
                buf.push_str(&format!(
                    "\n_({omitted} more entries omitted due to --max-chars={max_chars}; use `aimem search \"<query>\"` for details)_\n"
                ));
            }
            print!("{buf}");
        }
        Commands::Add(args) => add::run(args).map_err(|err| err.to_string())?,
        Commands::Search {
            query,
            project_only,
            limit,
            json,
        } => {
            let store = open_read_store()?;
            let pid = if project_only {
                Some(detect_project(None).id)
            } else {
                None
            };
            let items = store.search_memories(&query, pid.as_deref(), limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&items).unwrap());
                return Ok(());
            }
            if items.is_empty() {
                println!("No matches for \"{query}\".");
                return Ok(());
            }
            for m in items {
                println!("[{}] ({}) {} — {}", m.kind, m.project_id, short_id(&m.id), m.content);
            }
        }
        Commands::List {
            projects,
            project,
            limit,
        } => {
            let store = open_read_store()?;
            if projects {
                for (p, count) in store.list_projects_with_counts()? {
                    println!("{} — {} ({count} memories)", p.id, p.name);
                }
                return Ok(());
            }
            let pid = project.unwrap_or_else(|| detect_project(None).id);
            let items = store.list_memories(Some(&pid), limit)?;
            for m in items {
                println!("[{}] {} — {}", m.kind, m.id, m.content);
            }
        }
        Commands::Edit {
            id,
            content,
            kind,
            tags,
        } => {
            if content.is_none() && kind.is_none() && tags.is_none() {
                return Err("Provide at least one of --content, --kind, or --tags.".to_string());
            }
            let db = Database::open_default().map_err(|e| e.to_string())?;
            match db
                .update_memory_by_prefix(&id, kind.as_deref(), content.as_deref(), tags.as_deref())
                .map_err(|e| e.to_string())?
            {
                Some(m) => println!("Updated [{}] {} — {}", m.kind, short_id(&m.id), m.content),
                None => println!("Not found: {id}"),
            }
        }
        Commands::Rm { id } => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            match db.soft_delete_memory_by_prefix(&id).map_err(|e| e.to_string())? {
                Some(full_id) => println!("Deleted {full_id}"),
                None => println!("Not found: {id}"),
            }
        }
        Commands::Auth {
            client_id,
            client_secret,
        } => {
            let mut cfg = load_config();
            if let Some(id) = client_id {
                cfg.google_client_id = Some(id);
            }
            if let Some(sec) = client_secret {
                cfg.google_client_secret = Some(sec);
            }
            save_config(&cfg).map_err(|e| e.to_string())?;
            authenticate_google(&mut cfg).map_err(|e| e.to_string())?;
            println!("Google Drive authenticated successfully.");
        }
        Commands::Sync => {
            let mut db = Database::open_default().map_err(|e| e.to_string())?;
            let mut cfg = load_config();
            let report = sync_with_drive(&mut db, &mut cfg).map_err(|e| e.to_string())?;
            println!(
                "Synced at {}: +{} added, ~{} updated, {} total memories uploaded.",
                report.synced_at,
                report.merged.memories_added,
                report.merged.memories_updated,
                report.uploaded_memories
            );
        }
        Commands::InstallRules { local } => {
            let global_files = install_global_rules().map_err(|e| e.to_string())?;
            println!("Updated global agent rules:");
            for f in global_files {
                println!("  - {}", f.display());
            }
            if local {
                let detected = detect_project(None);
                let local_files = install_local_rules(&detected.root_path).map_err(|e| e.to_string())?;
                println!("Updated local project rules:");
                for f in local_files {
                    println!("  - {}", f.display());
                }
            }
        }
        Commands::Import { projects_dir } => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let report = import_all(&db, projects_dir.as_deref())?;
            println!(
                "Scanned {} files: {} imported, {} duplicates skipped ({} projects).",
                report.files_scanned,
                report.memories_imported,
                report.memories_skipped_duplicate,
                report.projects_touched
            );
        }
        Commands::Gc { days } => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let removed = db.gc_deleted(days).map_err(|e| e.to_string())?;
            println!("Purged {removed} deleted memories older than {days} days.");
        }
        Commands::Compact { purge_deleted_days } => {
            let mut db = Database::open_default().map_err(|e| e.to_string())?;
            let db_path = data_dir().join("memory.db");
            let report = db
                .compact(&db_path, purge_deleted_days)
                .map_err(|e| e.to_string())?;
            refresh_mirror_if_configured(&db);
            println!(
                "Compacted {}: {} -> {} (scrubbed {} tombstones, purged {}).",
                db_path.display(),
                format_bytes(report.size_before),
                format_bytes(report.size_after),
                report.tombstones_scrubbed,
                report.tombstones_purged
            );
        }
        Commands::Inbox { action } => match action {
            InboxAction::Status => {
                let settings = InboxSettings::load().unwrap_or_default();
                let env_dir = std::env::var("AIMEM_INBOX_DIR").ok().filter(|s| !s.trim().is_empty());
                println!(
                    "Inbox dir (config): {}",
                    settings
                        .directory
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "not configured".into())
                );
                println!(
                    "AIMEM_INBOX_DIR:    {}",
                    env_dir.as_deref().unwrap_or("not set")
                );
                println!("Allow all projects: {}", settings.allow_all_projects);
                println!("Allowed projects:   {}", settings.projects.len());
                let active_dir = env_dir
                    .map(std::path::PathBuf::from)
                    .or(settings.directory);
                let pending = active_dir
                    .as_deref()
                    .map(count_pending_inbox_files)
                    .unwrap_or(0);
                println!("Pending requests:   {pending}");
                let mirror_info = active_dir
                    .as_ref()
                    .map(|d| d.join(MIRROR_FILENAME))
                    .map(|p| match std::fs::metadata(&p) {
                        Ok(m) => format!("{} ({})", p.display(), format_bytes(m.len())),
                        Err(_) => "missing".to_string(),
                    })
                    .unwrap_or_else(|| "not configured".to_string());
                println!("Read mirror:        {mirror_info}");
            }
            InboxAction::Import => {
                let report = import_configured().map_err(|e| e.to_string())?;
                if report.imported > 0 {
                    if let Ok(db) = Database::open_read_only_default() {
                        refresh_mirror_if_configured(&db);
                    }
                }
                println!(
                    "Inbox import: {} imported, {} duplicates, {} skipped, {} invalid, {} pending.",
                    report.imported,
                    report.duplicates,
                    report.skipped,
                    report.invalid,
                    report.pending
                );
            }
        },
        Commands::Doctor => {
            run_doctor();
        }
        Commands::Status => {
            let store = open_read_store()?;
            let cfg = load_config();
            let detected = detect_project(None);
            let projects = store.list_projects()?;
            let active_memories = store.count_memories()?;
            let db_size = std::fs::metadata(data_dir().join("memory.db"))
                .map(|m| format_bytes(m.len()))
                .unwrap_or_else(|_| "via mirror".into());
            let inbox = InboxSettings::load().ok();
            let inbox_dir = configured_inbox_dir();
            let inbox_summary = match inbox_dir {
                Some(ref dir) => {
                    let pending = count_pending_inbox_files(dir);
                    let allow_all = inbox.as_ref().is_some_and(|s| s.allow_all_projects);
                    let proj_cnt = inbox.as_ref().map(|s| s.projects.len()).unwrap_or(0);
                    let mirror_ok = dir.join(MIRROR_FILENAME).is_file();
                    format!(
                        "{} (allow_all={allow_all}, projects={proj_cnt}, pending={pending}, mirror={mirror_ok})",
                        dir.display()
                    )
                }
                None => "not configured".to_string(),
            };
            println!("Data source:      {}", store.source_label());
            println!("Database size:    {db_size}");
            println!("Current project:  {} ({})", detected.name, detected.id);
            println!("Projects:         {}", projects.len());
            println!("Active memories:  {}", active_memories);
            println!("Inbox:            {inbox_summary}");
            println!("Google connected: {}", cfg.google_refresh_token.is_some());
            println!(
                "Last sync:        {}",
                cfg.last_sync_at.as_deref().unwrap_or("never")
            );
        }
    }
    Ok(())
}

fn run_doctor() {
    if let Ok(exe) = std::env::current_exe() {
        let exe_str = exe.display().to_string();
        let in_target = exe_str.contains(r"\target\") || exe_str.contains("/target/");
        let tag = if in_target { "WARN" } else { "OK  " };
        println!("[{tag}] binary: {exe_str}{}", if in_target { " (in target/; copy to stable bin)" } else { "" });
    }

    let db_path = data_dir().join("memory.db");
    match Database::open_read_only(&db_path) {
        Ok(db) => {
            let size = std::fs::metadata(&db_path).map(|m| format_bytes(m.len())).unwrap_or_default();
            let count = db.count_memories().unwrap_or(0);
            println!("[OK  ] db read: {} ({size}, {count} active)", db_path.display());
        }
        Err(e) => println!("[FAIL] db read: {} ({e})", db_path.display()),
    }
    match Database::open_default() {
        Ok(_) => println!("[OK  ] db write: accessible"),
        Err(e) => println!("[WARN] db write: {e}"),
    }

    let env_inbox = std::env::var("AIMEM_INBOX_DIR").ok().filter(|s| !s.trim().is_empty());
    let settings = InboxSettings::load().ok();
    let inbox_dir = env_inbox
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(|| settings.as_ref().and_then(|s| s.directory.clone()));
    match inbox_dir {
        Some(dir) => {
            let exists = dir.is_dir();
            let pending = count_pending_inbox_files(&dir);
            let allow_all = settings.as_ref().is_some_and(|s| s.allow_all_projects);
            let proj_cnt = settings.as_ref().map(|s| s.projects.len()).unwrap_or(0);
            let mirror_ok = dir.join(MIRROR_FILENAME).is_file();
            let tag = if exists && env_inbox.is_some() && mirror_ok { "OK  " } else { "WARN" };
            println!(
                "[{tag}] inbox: {} (exists={exists}, env_set={}, allow_all={allow_all}, projects={proj_cnt}, mirror={mirror_ok}, pending={pending})",
                dir.display(),
                env_inbox.is_some()
            );
        }
        None => println!("[WARN] inbox: not configured (no AIMEM_INBOX_DIR or inbox.json)"),
    }

    let home = home_dir();
    let rule_files = [
        ("Claude", home.join(".claude").join("CLAUDE.md")),
        ("Antigravity AGENTS", home.join(".gemini").join("antigravity").join("AGENTS.md")),
        ("Gemini", home.join(".gemini").join("GEMINI.md")),
        ("Codex", home.join(".codex").join("AGENTS.md")),
        ("Cursor", home.join(".cursor").join("rules").join("aimem.mdc")),
    ];
    for (name, path) in rule_files {
        let has_block = std::fs::read_to_string(&path)
            .map(|c| c.contains("<!-- AI-MEMORY-START -->"))
            .unwrap_or(false);
        let tag = if has_block { "OK  " } else { "WARN" };
        println!("[{tag}] rule {name}: {}", path.display());
    }

    let claude_settings = home.join(".claude").join("settings.json");
    let (has_perm, has_hook) = std::fs::read_to_string(&claude_settings)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|v| {
            let perm = v["permissions"]["allow"]
                .as_array()
                .is_some_and(|a| a.iter().any(|x| x.as_str() == Some("Bash(aimem:*)")));
            let hook = v["hooks"]["SessionStart"]
                .as_array()
                .is_some_and(|a| {
                    serde_json::to_string(a).is_ok_and(|s| s.contains("aimem context"))
                });
            (perm, hook)
        })
        .unwrap_or((false, false));
    let tag = if has_perm && has_hook { "OK  " } else { "WARN" };
    println!("[{tag}] claude settings: allow_aimem={has_perm}, session_start_hook={has_hook}");
}
