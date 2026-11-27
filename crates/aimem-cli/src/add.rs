use aimem_core::db::{Database, DbError, Memory, Result};
use aimem_core::inbox::{InboxEntry, InboxSettings};
use aimem_core::project::detect_project;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct AddArgs {
    content: String,
    #[arg(long, default_value = "note")]
    kind: String,
    #[arg(long, default_value = "")]
    tags: String,
    #[arg(long, default_value = "user")]
    agent: String,
    #[arg(long)]
    global: bool,
    #[arg(long)]
    project: Option<String>,
    /// Queue the memory immediately instead of opening SQLite
    #[arg(long)]
    queue: bool,
    /// Override the inbox directory (or set AIMEM_INBOX_DIR)
    #[arg(long)]
    inbox: Option<PathBuf>,
}

pub fn run(args: AddArgs) -> Result<()> {
    let detected = detect_project(None);
    let (id, name, remote) = if args.global {
        ("global".into(), "Global Preferences".into(), None)
    } else if let Some(project) = args.project {
        (project.clone(), project, None)
    } else {
        (detected.id, detected.name, detected.git_remote)
    };
    let entry = InboxEntry::new(id, args.kind, args.content, args.tags, args.agent);
    if args.queue {
        return enqueue(entry, inbox_directory(args.inbox)?.ok_or_else(|| {
            DbError::InvalidInput("No inbox is configured. Set it in AI Memory or pass --inbox.".into())
        })?);
    }

    match save(&entry, &name, remote.as_deref()) {
        Ok((memory, created)) => {
            let verb = if created { "Saved" } else { "Already saved" };
            println!("{verb} [{}] {} -> {} ({})", memory.kind, memory.id, memory.project_id, memory.content);
            Ok(())
        }
        Err(err) if err.is_access_failure() && entry.project_id != "global" => {
            if let Some(directory) = inbox_directory(args.inbox)? {
                enqueue(entry, directory)
            } else {
                Err(DbError::InvalidInput(
                    "SQLite is inaccessible and no inbox directory is configured (set AIMEM_INBOX_DIR or pass --inbox).".into(),
                ))
            }
        }
        Err(err) => Err(err),
    }
}

fn save(entry: &InboxEntry, name: &str, remote: Option<&str>) -> Result<(Memory, bool)> {
    let db = Database::open_default()?;
    if entry.project_id != "global" {
        db.ensure_project(&entry.project_id, name, remote)?;
    }
    db.add_entry_dedup(entry)
}

fn inbox_directory(explicit: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let override_dir = explicit.or_else(|| {
        std::env::var_os("AIMEM_INBOX_DIR").filter(|value| !value.is_empty()).map(PathBuf::from)
    });
    match override_dir {
        Some(path) => Ok(Some(path)),
        None => match InboxSettings::load() {
            Ok(settings) => Ok(settings.directory),
            Err(DbError::Io(err)) if err.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
            Err(err) => Err(err),
        },
    }
}

fn enqueue(entry: InboxEntry, directory: PathBuf) -> Result<()> {
    entry.enqueue(&directory)?;
    println!("Queued [{}] {} -> {}", entry.kind, entry.id, entry.project_id);
    Ok(())
}
