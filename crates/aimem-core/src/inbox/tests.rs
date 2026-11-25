use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("aimem-inbox-test-{}", Uuid::new_v4()));
        fs::create_dir_all(path.join("requests")).unwrap();
        Self(path)
    }

    fn directory(&self) -> PathBuf {
        self.0.join("requests")
    }

    fn database(&self) -> Database {
        Database::open(&self.0.join("memory.db")).unwrap()
    }

    fn settings(&self) -> InboxSettings {
        InboxSettings {
            directory: Some(self.directory()),
            projects: vec!["local/trusted".into()],
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn request(project: &str, agent: &str) -> InboxEntry {
    InboxEntry::new(project.into(), "decision".into(), "Keep an audit log".into(), "db".into(), agent.into())
}

#[test]
fn accepts_different_agents_and_preserves_metadata() {
    let fixture = Fixture::new();
    let mut db = fixture.database();
    for agent in ["codex", "claude", "cursor", "gemini", "antigravity"] {
        let entry = request("local/trusted", agent);
        entry.enqueue(&fixture.directory()).unwrap();
        assert!(db.get_memory(&entry.id).unwrap().is_none());
    }

    let report = import(&fixture.settings(), |entry| db.import_inbox_entry(entry)).unwrap();
    assert_eq!(report.imported, 5);
    assert_eq!(fs::read_dir(fixture.directory()).unwrap().count(), 0);
    let memories = db.list_memories(Some("local/trusted"), 10).unwrap();
    assert_eq!(memories.len(), 5);
    assert!(memories.iter().all(|memory| memory.kind == "decision" && memory.tags == "db"));
    assert!(memories.iter().any(|memory| memory.source_agent == "gemini"));
}

#[test]
fn ignores_partial_invalid_and_unapproved_files() {
    let fixture = Fixture::new();
    let mut db = fixture.database();
    let directory = fixture.directory();
    request("local/unapproved", "cursor").enqueue(&directory).unwrap();
    assert!(request("global", "codex").enqueue(&directory).is_err());
    fs::write(directory.join(format!("{}.json", Uuid::new_v4())), b"bad json").unwrap();
    fs::write(directory.join("unfinished.tmp"), b"partial write").unwrap();

    let report = import(&fixture.settings(), |entry| db.import_inbox_entry(entry)).unwrap();
    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.invalid, 1);
    assert!(db.get_project("local/unapproved").unwrap().is_none());
    assert_eq!(fs::read_dir(directory).unwrap().count(), 3);
}

#[test]
fn replay_after_deletion_does_not_restore_a_memory() {
    let fixture = Fixture::new();
    let mut db = fixture.database();
    let entry = request("local/trusted", "claude");
    entry.enqueue(&fixture.directory()).unwrap();
    let settings = fixture.settings();
    assert_eq!(import(&settings, |entry| db.import_inbox_entry(entry)).unwrap().imported, 1);
    assert!(db.soft_delete_memory(&entry.id).unwrap());
    // Remove the tombstone to test replay even after garbage collection.
    let raw = rusqlite::Connection::open(fixture.0.join("memory.db")).unwrap();
    raw.execute("DELETE FROM memories WHERE id = ?1", [&entry.id]).unwrap();
    drop(db);
    let mut db = fixture.database();
    entry.enqueue(&fixture.directory()).unwrap();
    assert_eq!(import(&settings, |entry| db.import_inbox_entry(entry)).unwrap().duplicates, 1);
    assert!(db.get_memory(&entry.id).unwrap().is_none());
}

#[test]
fn upgrades_a_v1_database_and_preserves_existing_memory() {
    let fixture = Fixture::new();
    let db = fixture.database();
    let old = db.add_memory("global", "note", "Existing entry", "", "user").unwrap();
    drop(db);
    let path = fixture.0.join("memory.db");
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch("DROP TABLE inbox_receipts; PRAGMA user_version = 1;").unwrap();
    drop(raw);

    let mut db = fixture.database();
    assert_eq!(db.get_memory(&old.id).unwrap().unwrap().content, "Existing entry");
    let entry = request("local/trusted", "codex");
    assert!(db.import_inbox_entry(&entry).unwrap());
    assert!(!db.import_inbox_entry(&entry).unwrap());
}

#[test]
fn ignores_linked_requests() {
    let fixture = Fixture::new();
    let mut db = fixture.database();
    let entry = request("local/trusted", "codex");
    let outside = fixture.0.join("outside.json");
    fs::write(&outside, serde_json::to_vec(&entry).unwrap()).unwrap();
    let link = fixture.directory().join(format!("{}.json", entry.id));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    #[cfg(windows)]
    if let Err(err) = std::os::windows::fs::symlink_file(&outside, &link) {
        if err.raw_os_error() == Some(1314) || err.kind() == io::ErrorKind::PermissionDenied {
            return;
        }
        panic!("{err}");
    }

    let report = import(&fixture.settings(), |entry| db.import_inbox_entry(entry)).unwrap();
    assert_eq!(report.imported, 0);
    assert_eq!(report.invalid, 1);
    assert!(outside.exists());
}
