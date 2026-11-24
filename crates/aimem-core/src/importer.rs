use crate::db::{normalize_kind, Database};
use crate::project::detect_project;
use crate::rules::home_dir;
use serde::Serialize;
use std::path::{Path, PathBuf};

const OUR_START: &str = "<!-- AI-MEMORY-START -->";
const OUR_END: &str = "<!-- AI-MEMORY-END -->";

#[derive(Debug, Clone, Serialize, Default)]
pub struct ImportReport {
    pub files_scanned: usize,
    pub projects_touched: usize,
    pub memories_imported: usize,
    pub memories_skipped_duplicate: usize,
}

pub fn import_all(db: &Database, projects_root: Option<&Path>) -> Result<ImportReport, String> {
    let mut report = ImportReport::default();
    let mut touched_projects = std::collections::HashSet::new();

    // 1. Import ~/.claude/CLAUDE.md -> global
    let home = home_dir();
    let global_claude = home.join(".claude").join("CLAUDE.md");
    if global_claude.is_file() {
        import_markdown_file(db, &global_claude, "global", "Global Preferences", None, "claude", "pref", &mut report, &mut touched_projects)?;
    }

    // 2. Import ~/.claude/projects/<encoded-path>/memory/*.md
    let claude_projects = home.join(".claude").join("projects");
    if claude_projects.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&claude_projects) {
            for entry in entries.flatten() {
                let proj_dir = entry.path();
                if !proj_dir.is_dir() {
                    continue;
                }
                let mem_dir = proj_dir.join("memory");
                if !mem_dir.is_dir() {
                    continue;
                }
                let folder_name = proj_dir
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown");
                let (pid, pname, remote) = resolve_claude_project_folder(folder_name);

                if let Ok(files) = std::fs::read_dir(&mem_dir) {
                    for f in files.flatten() {
                        let fpath = f.path();
                        if fpath.extension().and_then(|e| e.to_str()) == Some("md") {
                            let is_index = fpath
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(|n| n.eq_ignore_ascii_case("MEMORY.md"))
                                .unwrap_or(false);
                            let default_kind = if is_index { "note" } else { "arch" };
                            import_markdown_file(
                                db,
                                &fpath,
                                &pid,
                                &pname,
                                remote.as_deref(),
                                "claude",
                                default_kind,
                                &mut report,
                                &mut touched_projects,
                            )?;
                        }
                    }
                }
            }
        }
    }

    // 3. Scan a projects root directory (depth 1) for CLAUDE.md, AGENTS.md, GEMINI.md, .cursorrules
    if let Some(root) = projects_root {
        if root.is_dir() {
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let pdir = entry.path();
                    if !pdir.is_dir() {
                        continue;
                    }
                    let detected = detect_project(Some(&pdir));
                    for (fname, agent) in [
                        ("CLAUDE.md", "claude"),
                        ("AGENTS.md", "antigravity"),
                        ("GEMINI.md", "antigravity"),
                        (".cursorrules", "cursor"),
                    ] {
                        let candidate = pdir.join(fname);
                        if candidate.is_file() {
                            import_markdown_file(
                                db,
                                &candidate,
                                &detected.id,
                                &detected.name,
                                detected.git_remote.as_deref(),
                                agent,
                                "arch",
                                &mut report,
                                &mut touched_projects,
                            )?;
                        }
                    }
                }
            }
        }
    }

    report.projects_touched = touched_projects.len();
    Ok(report)
}

fn resolve_claude_project_folder(encoded: &str) -> (String, String, Option<String>) {
    // Claude encodes "C:\RABOTA\Cods\VAH" as "C--RABOTA-Cods-VAH"
    if let Some(candidate) = decode_windows_claude_path(encoded) {
        if candidate.is_dir() {
            let detected = detect_project(Some(&candidate));
            return (detected.id, detected.name, detected.git_remote);
        }
    }
    let short_name = encoded
        .rsplit('-')
        .find(|s| !s.is_empty())
        .unwrap_or(encoded)
        .to_string();
    (format!("local/{}", short_name.to_lowercase()), short_name, None)
}

fn decode_windows_claude_path(encoded: &str) -> Option<PathBuf> {
    let bytes = encoded.as_bytes();
    if bytes.len() >= 4 && bytes[0].is_ascii_alphabetic() && &bytes[1..3] == b"--" {
        let drive = bytes[0] as char;
        let rest = &encoded[3..];
        let joined = format!("{drive}:\\{}", rest.replace('-', "\\"));
        return Some(PathBuf::from(joined));
    }
    None
}

fn import_markdown_file(
    db: &Database,
    path: &Path,
    project_id: &str,
    project_name: &str,
    git_remote: Option<&str>,
    source_agent: &str,
    default_kind: &str,
    report: &mut ImportReport,
    touched_projects: &mut std::collections::HashSet<String>,
) -> Result<(), String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    report.files_scanned += 1;

    let cleaned = strip_our_block(&raw);
    let (fm_title, fm_type, body) = parse_frontmatter(&cleaned);
    let body_trimmed = body.trim();
    if body_trimmed.is_empty() {
        return Ok(());
    }

    db.ensure_project(project_id, project_name, git_remote)
        .map_err(|e| e.to_string())?;

    let kind = fm_type
        .as_deref()
        .map(|t| match t.to_lowercase().as_str() {
            "user" | "feedback" => "pref".to_string(),
            "project" => "arch".to_string(),
            "reference" => "note".to_string(),
            other => normalize_kind(other),
        })
        .unwrap_or_else(|| default_kind.to_string());

    let file_label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("memory.md");

    let content = if let Some(title) = fm_title {
        format!("[{title}] ({file_label})\n{body_trimmed}")
    } else {
        format!("[{file_label}]\n{body_trimmed}")
    };

    let existing = db
        .list_memories(Some(project_id), 10000)
        .map_err(|e| e.to_string())?;
    if existing.iter().any(|m| m.content.trim() == content.trim()) {
        report.memories_skipped_duplicate += 1;
        return Ok(());
    }

    db.add_memory(project_id, &kind, &content, "imported", source_agent)
        .map_err(|e| e.to_string())?;
    report.memories_imported += 1;
    touched_projects.insert(project_id.to_string());
    Ok(())
}

fn strip_our_block(text: &str) -> String {
    if let (Some(start), Some(end)) = (text.find(OUR_START), text.find(OUR_END)) {
        if end >= start {
            let end_idx = end + OUR_END.len();
            return format!("{}{}", &text[..start], &text[end_idx..]);
        }
    }
    text.to_string()
}

fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>, &str) {
    let trimmed = text.trim_start();
    if let Some(after_open) = trimmed.strip_prefix("---") {
        if let Some(end_idx) = after_open.find("\n---") {
            let fm = &after_open[..end_idx];
            let body = &after_open[end_idx + 4..];
            let mut title = None;
            let mut ftype = None;
            for line in fm.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim().to_lowercase();
                    let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    if key == "name" && !val.is_empty() {
                        title = Some(val);
                    } else if key == "type" && !val.is_empty() {
                        ftype = Some(val);
                    }
                }
            }
            return (title, ftype, body);
        }
    }
    (None, None, text)
}
