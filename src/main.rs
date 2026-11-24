mod db;
mod project;
mod rules;
mod sync;
mod ui;

use clap::{Parser, Subcommand};
use db::{data_dir, load_config, save_config, Database};
use project::detect_project;
use rules::{install_global_rules, install_local_rules};
use sync::{authenticate_google, sync_with_drive};

#[derive(Parser)]
#[command(name = "aimem", version, about = "Unified cross-agent AI memory without MCP")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Print global + current project memory context (for AI agents)
    Context {
        /// Optional project ID override
        #[arg(long)]
        project: Option<String>,
        /// Max memories per section
        #[arg(long, default_value_t = 30)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Add a new memory entry
    Add {
        /// Fact, decision, bug fix, or preference to remember
        content: String,
        /// Memory kind: decision | arch | bug | pref | note
        #[arg(long, default_value = "note")]
        kind: String,
        /// Comma-separated tags
        #[arg(long, default_value = "")]
        tags: String,
        /// Source agent (antigravity, claude, cursor, user)
        #[arg(long, default_value = "user")]
        agent: String,
        /// Store in global memory instead of current project
        #[arg(long)]
        global: bool,
        /// Explicit project ID override
        #[arg(long)]
        project: Option<String>,
    },
    /// Full-text search across memories (FTS5)
    Search {
        /// Search query
        query: String,
        /// Restrict to current project (+ global)
        #[arg(long)]
        project_only: bool,
        /// Max results
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List projects or memories
    List {
        /// Show all projects instead of memories
        #[arg(long)]
        projects: bool,
        /// Filter by project ID
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Delete a memory entry by ID
    Rm {
        id: String,
    },
    /// Launch local Web UI
    Ui {
        #[arg(long, default_value_t = 4747)]
        port: u16,
        /// Do not auto-open browser
        #[arg(long)]
        no_open: bool,
    },
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
        /// Also write AGENTS.md and CLAUDE.md in the current project
        #[arg(long)]
        local: bool,
    },
    /// Show database path and sync status
    Status,
}

fn main() {
    let cli = Cli::parse();
    let cmd = cli.command.unwrap_or(Commands::Ui {
        port: 4747,
        no_open: false,
    });

    if let Err(err) = run(cmd) {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

fn run(cmd: Commands) -> Result<(), String> {
    match cmd {
        Commands::Context { project, limit, json } => {
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

            println!("# AI Memory Context");
            println!("Project: **{}** (`{}`)", proj.name, proj.id);
            if !proj.summary.is_empty() {
                println!("Summary: {}", proj.summary);
            }
            println!();

            if !global_items.is_empty() {
                println!("## Global Preferences & Rules");
                for m in &global_items {
                    println!("- [{}] {} _(id: {}, by {})_", m.kind, m.content, &m.id[..8], m.source_agent);
                }
                println!();
            }

            println!("## Project Memory");
            if project_items.is_empty() {
                println!("_(no project memories yet)_");
            } else {
                for m in &project_items {
                    let tags = if m.tags.is_empty() {
                        String::new()
                    } else {
                        format!(" [tags: {}]", m.tags)
                    };
                    println!(
                        "- [{}] {}{} _(id: {}, by {})_",
                        m.kind, m.content, tags, &m.id[..8], m.source_agent
                    );
                }
            }
        }
        Commands::Add {
            content,
            kind,
            tags,
            agent,
            global,
            project,
        } => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let pid = if global {
                "global".to_string()
            } else if let Some(p) = project {
                db.ensure_project(&p, &p, None).map_err(|e| e.to_string())?;
                p
            } else {
                let detected = detect_project(None);
                db.ensure_project(&detected.id, &detected.name, detected.git_remote.as_deref())
                    .map_err(|e| e.to_string())?;
                detected.id
            };
            let mem = db
                .add_memory(&pid, &kind, &content, &tags, &agent)
                .map_err(|e| e.to_string())?;
            println!("Saved [{}] {} -> {} ({})", mem.kind, mem.id, mem.project_id, mem.content);
        }
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
                println!("[{}] ({}) {} — {}", m.kind, m.project_id, &m.id[..8], m.content);
            }
        }
        Commands::List {
            projects,
            project,
            limit,
        } => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            if projects {
                for p in db.list_projects().map_err(|e| e.to_string())? {
                    let count = db.list_memories(Some(&p.id), 10000).map_err(|e| e.to_string())?.len();
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
        Commands::Rm { id } => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            if db.soft_delete_memory(&id).map_err(|e| e.to_string())? {
                println!("Deleted {id}");
            } else {
                println!("Not found: {id}");
            }
        }
        Commands::Ui { port, no_open } => {
            ui::serve(port, !no_open)?;
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
        Commands::Status => {
            let db = Database::open_default().map_err(|e| e.to_string())?;
            let cfg = load_config();
            let detected = detect_project(None);
            let projects = db.list_projects().map_err(|e| e.to_string())?;
            let memories = db.list_memories(None, 10000).map_err(|e| e.to_string())?;
            println!("Data dir:         {}", data_dir().display());
            println!("Current project:  {} ({})", detected.name, detected.id);
            println!("Projects:         {}", projects.len());
            println!("Active memories:  {}", memories.len());
            println!("Google connected: {}", cfg.google_refresh_token.is_some());
            println!(
                "Last sync:        {}",
                cfg.last_sync_at.as_deref().unwrap_or("never")
            );
        }
    }
    Ok(())
}
