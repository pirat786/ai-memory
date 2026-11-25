mod add;

use aimem_core::db::{data_dir, load_config, save_config, Database};
use aimem_core::importer::import_all;
use aimem_core::project::detect_project;
use aimem_core::rules::{install_global_rules, install_local_rules};
use aimem_core::sync::{authenticate_google, sync_with_drive};
use clap::{Parser, Subcommand};

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
    /// Show database path and sync status
    Status,
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
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
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let detected = detect_project(None);
            let pid = project.unwrap_or_else(|| detected.id.clone());
            let proj = db
                .ensure_project(&pid, &detected.name, detected.git_remote.as_deref())
                .map_err(|e| e.to_string())?;

            let global_items = db.list_memories(Some("global"), limit).map_err(|e| e.to_string())?;
            let project_items = if pid == "global" {
                Vec::new()
            } else {
                db.list_memories(Some(&pid), limit).map_err(|e| e.to_string())?
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

            let mut used_chars = buf.len();
            let mut omitted = 0usize;

            if !global_items.is_empty() {
                buf.push_str("## Global Preferences & Rules\n");
                for m in &global_items {
                    let line = format!(
                        "- [{}] {} _(id: {}, by {})_\n",
                        m.kind,
                        m.content,
                        short_id(&m.id),
                        m.source_agent
                    );
                    if max_chars > 0 && used_chars + line.len() > max_chars {
                        omitted += 1;
                        continue;
                    }
                    used_chars += line.len();
                    buf.push_str(&line);
                }
                buf.push('\n');
            }

            buf.push_str("## Project Memory\n");
            if project_items.is_empty() {
                buf.push_str("_(no project memories yet)_\n");
            } else {
                for m in &project_items {
                    let tags = if m.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [tags: {}]", m.tags)
                    };
                    let line = format!(
                        "- [{}] {}{} _(id: {}, by {})_\n",
                        m.kind,
                        m.content,
                        tags,
                        short_id(&m.id),
                        m.source_agent
                    );
                    if max_chars > 0 && used_chars + line.len() > max_chars {
                        omitted += 1;
                        continue;
                    }
                    used_chars += line.len();
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
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let pid = if project_only {
                Some(detect_project(None).id)
            } else {
                None
            };
            let items = db
                .search_memories(&query, pid.as_deref(), limit)
                .map_err(|e| e.to_string())?;
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
            let db = Database::open_default().map_err(|e| e.to_string())?;
            if projects {
                for (p, count) in db.list_projects_with_counts().map_err(|e| e.to_string())? {
                    println!("{} — {} ({count} memories)", p.id, p.name);
                }
                return Ok(());
            }
            let pid = project.unwrap_or_else(|| detect_project(None).id);
            let items = db.list_memories(Some(&pid), limit).map_err(|e| e.to_string())?;
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
        Commands::Status => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let cfg = load_config();
            let detected = detect_project(None);
            let projects = db.list_projects().map_err(|e| e.to_string())?;
            let active_memories = db.count_memories().map_err(|e| e.to_string())?;
            println!("Data dir:         {}", data_dir().display());
            println!("Current project:  {} ({})", detected.name, detected.id);
            println!("Projects:         {}", projects.len());
            println!("Active memories:  {}", active_memories);
            println!("Google connected: {}", cfg.google_refresh_token.is_some());
            println!(
                "Last sync:        {}",
                cfg.last_sync_at.as_deref().unwrap_or("never")
            );
        }
    }
    Ok(())
}
