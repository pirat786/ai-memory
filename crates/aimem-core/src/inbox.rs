use crate::db::{data_dir, Database, DbError, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const MAX_REQUEST_BYTES: u64 = 128 * 1024;
const MAX_CONTENT_BYTES: usize = 32 * 1024;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InboxSettings {
    pub directory: Option<PathBuf>,
    pub projects: Vec<String>,
}

impl InboxSettings {
    fn path() -> PathBuf {
        data_dir().join("inbox.json")
    }

    pub fn load() -> Result<Self> {
        match fs::read(Self::path()) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err.into()),
        }
    }

    pub fn save(directory: Option<PathBuf>, projects: Vec<String>) -> Result<()> {
        let directory = directory.map(|path| validate_directory(&path)).transpose()?;
        let mut projects = projects
            .into_iter()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>();
        projects.sort_unstable();
        projects.dedup();

        if projects.iter().any(|id| !valid_project(id)) {
            return Err(invalid("The inbox accepts project IDs only, excluding global."));
        }
        if directory.is_some() && projects.is_empty() {
            return Err(invalid("Select at least one project ID for the inbox."));
        }

        let path = Self::path();
        fs::create_dir_all(data_dir())?;
        write_atomic(&path, &serde_json::to_vec_pretty(&Self { directory, projects })?)
    }
}

/// All agents and the desktop app share this project-scoped request format.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxEntry {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub content: String,
    pub tags: String,
    pub source_agent: String,
}

impl InboxEntry {
    pub fn new(project_id: String, kind: String, content: String, tags: String, agent: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            project_id,
            kind,
            content,
            tags,
            source_agent: agent,
        }
    }

    fn validate(&self) -> Result<()> {
        if !valid_project(&self.project_id) {
            return Err(invalid("Only project-scoped memories can be queued."));
        }
        if Uuid::parse_str(&self.id).map(|id| id.to_string()).ok().as_ref() != Some(&self.id)
            || self.content.trim().is_empty()
            || self.content.len() > MAX_CONTENT_BYTES
            || self.kind.len() > 64
            || self.tags.len() > 1024
            || self.source_agent.trim().is_empty()
            || self.source_agent.len() > 128
        {
            return Err(invalid("Invalid inbox request ID, text, kind, tags, or agent."));
        }
        Ok(())
    }

    pub fn enqueue(&self, directory: &Path) -> Result<()> {
        self.validate()?;
        let directory = validate_directory(directory)?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > MAX_REQUEST_BYTES {
            return Err(invalid("Inbox request exceeds the size limit."));
        }
        write_atomic(&directory.join(format!("{}.json", self.id)), &bytes)
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub imported: usize,
    pub duplicates: usize,
    pub skipped: usize,
    pub invalid: usize,
    pub pending: usize,
}

pub fn import_configured() -> Result<ImportReport> {
    let settings = InboxSettings::load()?;
    let mut database = None;
    import(&settings, |entry| {
        if database.is_none() {
            database = Some(Database::open_default()?);
        }
        database.as_mut().unwrap().import_inbox_entry(entry)
    })
}

fn import(
    settings: &InboxSettings,
    mut accept: impl FnMut(&InboxEntry) -> Result<bool>,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let Some(directory) = &settings.directory else { return Ok(report) };
    for file in fs::read_dir(validate_directory(directory)?)? {
        let file = file?;
        let name = file.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".json")) else { continue };
        if Uuid::parse_str(id).map(|uuid| uuid.to_string()).ok().as_deref() != Some(id) {
            continue;
        }
        let path = file.path();
        let entry = match read_entry(&path, id) {
            Ok(entry) => entry,
            Err(DbError::InvalidInput(_) | DbError::Json(_)) => {
                report.invalid += 1;
                continue;
            }
            Err(DbError::Io(err)) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                report.pending += 1;
                continue;
            }
        };
        if !settings.projects.contains(&entry.project_id) {
            report.skipped += 1;
            continue;
        }
        if accept(&entry)? {
            report.imported += 1;
        } else {
            report.duplicates += 1;
        }
        if let Err(err) = fs::remove_file(path) {
            if err.kind() != io::ErrorKind::NotFound {
                report.pending += 1;
            }
        }
    }
    Ok(report)
}

fn read_entry(path: &Path, id: &str) -> Result<InboxEntry> {
    let metadata = fs::symlink_metadata(path)?;
    if !plain_file(&metadata) {
        return Err(invalid("Inbox request must be a regular file."));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if !plain_file(&file.metadata()?) {
        return Err(invalid("Inbox request must be a regular file."));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("Inbox request exceeds the size limit."));
    }
    let entry: InboxEntry = serde_json::from_slice(&bytes)?;
    entry.validate()?;
    if entry.id != id {
        return Err(invalid("Inbox filename must match the request ID."));
    }
    Ok(entry)
}

fn is_indirect(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn plain_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && !is_indirect(metadata)
}

fn validate_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("Use an absolute inbox directory path."));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_indirect(&metadata) {
        return Err(invalid("Choose a regular directory for the inbox."));
    }
    Ok(path.canonicalize()?)
}

fn valid_project(id: &str) -> bool {
    !id.is_empty() && id.trim() == id && !matches!(id, "global" | "all") && id.len() <= 256
        && id.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && !id.chars().any(char::is_control)
}

fn invalid(message: &str) -> DbError {
    DbError::InvalidInput(message.to_owned())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file: File = OpenOptions::new().write(true).create_new(true).open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    Ok(result?)
}

#[cfg(test)]
mod tests;
