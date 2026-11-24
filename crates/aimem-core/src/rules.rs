use std::path::{Path, PathBuf};

const START_MARKER: &str = "<!-- AI-MEMORY-START -->";
const END_MARKER: &str = "<!-- AI-MEMORY-END -->";

const RULE_BLOCK: &str = r#"<!-- AI-MEMORY-START -->
## Unified AI Memory (`aimem`)

You have access to a shared cross-agent memory CLI (`aimem`) synced across the user's machines.
- **At the start of a task** in any project, run `aimem context` to load global user preferences and project-specific memory.
- **When searching past decisions or bugs**, run `aimem search "<query>"`.
- **When you learn important architectural decisions, non-obvious bug fixes, or user preferences**, save them concisely:
  - Project-specific: `aimem add "<fact>" --kind decision --agent <antigravity|claude|cursor>`
  - Global preference: `aimem add "<fact>" --global --kind pref --agent <antigravity|claude|cursor>`
  - Valid `--kind` values: `decision`, `arch`, `bug`, `pref`, `note`.
<!-- AI-MEMORY-END -->"#;

pub fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn install_global_rules() -> std::io::Result<Vec<PathBuf>> {
    let home = home_dir();
    let targets = vec![
        home.join(".claude").join("CLAUDE.md"),
        home.join(".gemini").join("antigravity").join("AGENTS.md"),
        home.join(".gemini").join("GEMINI.md"),
        home.join(".codex").join("AGENTS.md"),
    ];

    let mut updated = Vec::new();
    for path in targets {
        upsert_rule_file(&path)?;
        updated.push(path);
    }
    Ok(updated)
}

pub fn install_local_rules(project_root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let targets = vec![
        project_root.join("AGENTS.md"),
        project_root.join("CLAUDE.md"),
    ];
    let mut updated = Vec::new();
    for path in targets {
        upsert_rule_file(&path)?;
        updated.push(path);
    }
    Ok(updated)
}

fn upsert_rule_file(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let next = if let (Some(start), Some(end)) = (existing.find(START_MARKER), existing.find(END_MARKER)) {
        if end >= start {
            let end_idx = end + END_MARKER.len();
            format!("{}{}{}", &existing[..start], RULE_BLOCK, &existing[end_idx..])
        } else {
            append_block(&existing)
        }
    } else {
        append_block(&existing)
    };

    std::fs::write(path, next)
}

fn append_block(existing: &str) -> String {
    let trimmed = existing.trim_end();
    if trimmed.is_empty() {
        format!("{RULE_BLOCK}\n")
    } else {
        format!("{trimmed}\n\n{RULE_BLOCK}\n")
    }
}
