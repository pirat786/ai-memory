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
    let default_cods = Path::new(r"C:\RABOTA\Cods");
    let effective_root = projects_root.or_else(|| if default_cods.is_dir() { Some(default_cods) } else { None });

    // 1. Global rule/memory files across Claude, Antigravity/Gemini, Codex, Cursor
    let home = home_dir();
    for (path, agent) in [
        (home.join(".claude").join("CLAUDE.md"), "claude"),
        (home.join(".gemini").join("GEMINI.md"), "antigravity"),
        (home.join(".gemini").join("antigravity").join("AGENTS.md"), "antigravity"),
        (home.join(".codex").join("AGENTS.md"), "codex"),
    ] {
        if path.is_file() {
            import_markdown_file(db, &path, "global", "Global Preferences", None, agent, "pref", &mut report, &mut touched_projects)?;
        }
    }

    let codex_mem = home.join(".codex").join("memories");
    if codex_mem.is_dir() {
        if let Ok(files) = std::fs::read_dir(&codex_mem) {
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) == Some("md") {
                    import_markdown_file(db, &p, "global", "Global Preferences", None, "codex", "note", &mut report, &mut touched_projects)?;
                }
            }
        }
    }

    let cursor_rules = home.join(".cursor").join("rules");
    if cursor_rules.is_dir() {
        if let Ok(files) = std::fs::read_dir(&cursor_rules) {
            for f in files.flatten() {
                let p = f.path();
                let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                if ext == "md" || ext == "mdc" {
                    import_markdown_file(db, &p, "global", "Global Preferences", None, "cursor", "pref", &mut report, &mut touched_projects)?;
                }
            }
        }
    }

    // Antigravity knowledge & brain walkthroughs
    let agy_knowledge = home.join(".gemini").join("antigravity").join("knowledge");
    if agy_knowledge.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&agy_knowledge) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_file() && p.extension().and_then(|x| x.to_str()) == Some("md") {
                    import_markdown_file(db, &p, "global", "Global Preferences", None, "antigravity", "note", &mut report, &mut touched_projects)?;
                } else if p.is_dir() {
                    if let Ok(sub) = std::fs::read_dir(&p) {
                        for sf in sub.flatten() {
                            let sp = sf.path();
                            if sp.is_file() && sp.extension().and_then(|x| x.to_str()) == Some("md") {
                                import_markdown_file(db, &sp, "global", "Global Preferences", None, "antigravity", "note", &mut report, &mut touched_projects)?;
                            }
                        }
                    }
                }
            }
        }
    }

    let agy_brain = home.join(".gemini").join("antigravity").join("brain");
    if agy_brain.is_dir() {
        if let Ok(convs) = std::fs::read_dir(&agy_brain) {
            for c in convs.flatten() {
                let cdir = c.path();
                if !cdir.is_dir() {
                    continue;
                }
                let wt = cdir.join("walkthrough.md");
                if wt.is_file() {
                    import_markdown_file(db, &wt, "global", "Global Preferences", None, "antigravity", "note", &mut report, &mut touched_projects)?;
                }
            }
        }
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

    // 3. Scan a projects root directory (depth 1) for CLAUDE.md, AGENTS.md, GEMINI.md, .cursorrules, README.md, docs/ARCHITECTURE.md
    if let Some(root) = effective_root {
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
                        ("MEMORY.md", "claude"),
                        (".cursorrules", "cursor"),
                        (".github/copilot-instructions.md", "copilot"),
                        ("docs/ARCHITECTURE.md", "project"),
                        ("README.md", "project"),
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
                    let cursor_dir = pdir.join(".cursor").join("rules");
                    if cursor_dir.is_dir() {
                        if let Ok(cfiles) = std::fs::read_dir(&cursor_dir) {
                            for cf in cfiles.flatten() {
                                let cp = cf.path();
                                let ext = cp.extension().and_then(|e| e.to_str()).unwrap_or("");
                                if ext == "md" || ext == "mdc" {
                                    import_markdown_file(
                                        db,
                                        &cp,
                                        &detected.id,
                                        &detected.name,
                                        detected.git_remote.as_deref(),
                                        "cursor",
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
    if bytes.len() < 4 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b"--" {
        return None;
    }
    let drive = bytes[0] as char;
    let mut cur = PathBuf::from(format!("{drive}:\\"));
    let mut rest = encoded[3..].to_string();

    while !rest.is_empty() {
        rest = rest.trim_start_matches('-').to_string();
        if rest.is_empty() {
            break;
        }
        let mut matched: Option<(PathBuf, usize)> = None;
        if let Ok(entries) = std::fs::read_dir(&cur) {
            let mut candidates: Vec<(String, PathBuf)> = entries
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| {
                    let name = e.file_name().to_str()?.to_string();
                    Some((name, e.path()))
                })
                .collect();
            candidates.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));
            let rest_norm: String = rest
                .chars()
                .map(|c| if c == '_' || c == ' ' { '-' } else { c.to_ascii_lowercase() })
                .collect();

            for (name, path) in candidates {
                let name_norm: String = name
                    .chars()
                    .map(|c| if c == '_' || c == ' ' { '-' } else { c.to_ascii_lowercase() })
                    .collect();
                let char_count = name_norm.chars().count();
                if rest_norm == name_norm {
                    matched = Some((path, rest.chars().count()));
                    break;
                }
                if rest_norm.starts_with(&format!("{name_norm}-")) {
                    matched = Some((path, char_count));
                    break;
                }
            }
        }
        if let Some((next_path, consumed_chars)) = matched {
            cur = next_path;
            rest = rest.chars().skip(consumed_chars).collect();
        } else {
            break;
        }
    }

    if cur.as_os_str().len() > 3 && cur.is_dir() {
        Some(cur)
    } else {
        None
    }
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
