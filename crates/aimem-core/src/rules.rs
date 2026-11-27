use std::path::{Path, PathBuf};

const START_MARKER: &str = "<!-- AI-MEMORY-START -->";
const END_MARKER: &str = "<!-- AI-MEMORY-END -->";
const CURSOR_FRONTMATTER: &str = "---\ndescription: Unified cross-agent AI memory (aimem)\nalwaysApply: true\n---";

const RULE_BLOCK: &str = r#"<!-- AI-MEMORY-START -->
## Unified AI Memory (`aimem`)

You have access to a shared cross-agent memory CLI (`aimem`) synced across the user's machines.
- **At the start of a task** in any project, run `aimem context` to load global user preferences and project-specific memory.
- **When searching past decisions or bugs**, run `aimem search "<query>"`.
- **Before your final reply**, check whether the conversation produced a durable fact worth reusing in a future session: an agreed project decision, a confirmed architectural constraint, a verified bug cause or fix, or an explicitly stated lasting preference. Save new facts with `aimem add` without waiting for the user to ask, including when you only discussed the work and made no file changes.
  - Project fact: `aimem add "<fact>" --kind <decision|arch|bug|note> --agent <your-agent-name>`
  - Lasting cross-project preference: `aimem add "<preference>" --global --kind pref --agent <your-agent-name>`
  - Keep each fact to 1–2 sentences. Use `aimem search "<keywords>"` when unsure if it is already saved. Skip duplicates, tentative ideas, temporary details, transcripts, and secrets.
  - If SQLite is inaccessible, use the configured inbox for project facts. Treat `Queued` as pending import, and report a write failure if the fact was neither saved nor queued.
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
        home.join(".cursor").join("rules").join("aimem.mdc"),
    ];

    let mut updated = Vec::new();
    for path in targets {
        upsert_rule_file(&path)?;
        updated.push(path);
    }
    if let Ok(settings_path) = install_claude_settings() {
        updated.push(settings_path);
    }
    Ok(updated)
}

pub fn install_claude_settings() -> std::io::Result<PathBuf> {
    let path = home_dir().join(".claude").join("settings.json");
    upsert_claude_settings(&path)?;
    Ok(path)
}

pub fn upsert_claude_settings(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut root: serde_json::Value = match std::fs::read_to_string(path) {
        Ok(raw) if !raw.trim().is_empty() => serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        _ => serde_json::json!({}),
    };

    let obj = root
        .as_object_mut()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "settings.json root must be an object"))?;

    // 1. permissions.allow += "Bash(aimem:*)"
    let perms = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(perms_obj) = perms.as_object_mut() {
        let allow = perms_obj
            .entry("allow")
            .or_insert_with(|| serde_json::json!([]));
        if let Some(arr) = allow.as_array_mut() {
            let perm = "Bash(aimem:*)";
            if !arr.iter().any(|v| v.as_str() == Some(perm)) {
                arr.push(serde_json::Value::String(perm.to_string()));
            }
        }
    }

    // 2. hooks.SessionStart += { hooks: [{ type: "command", command: "aimem context" }] }
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(hooks_obj) = hooks.as_object_mut() {
        let session_start = hooks_obj
            .entry("SessionStart")
            .or_insert_with(|| serde_json::json!([]));
        if let Some(arr) = session_start.as_array_mut() {
            let already_present = arr.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|inner| {
                        inner.iter().any(|item| {
                            item.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|cmd| cmd.contains("aimem context"))
                        })
                    })
                    .unwrap_or(false)
            });
            if !already_present {
                arr.push(serde_json::json!({
                    "hooks": [
                        {
                            "type": "command",
                            "command": "aimem context"
                        }
                    ]
                }));
            }
        }
    }

    let formatted = serde_json::to_string_pretty(&root)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, format!("{formatted}\n"))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
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

    let is_mdc = path.extension().and_then(|e| e.to_str()) == Some("mdc");
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let base = if is_mdc && existing.trim().is_empty() {
        CURSOR_FRONTMATTER.to_string()
    } else {
        existing
    };
    let next = if let (Some(start), Some(end)) = (base.find(START_MARKER), base.find(END_MARKER)) {
        if end >= start {
            let end_idx = end + END_MARKER.len();
            format!("{}{}{}", &base[..start], RULE_BLOCK, &base[end_idx..])
        } else {
            append_block(&base)
        }
    } else {
        append_block(&base)
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

pub fn ensure_aimem_in_path() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Some(dir) = exe.parent() else {
        return false;
    };
    #[cfg(target_os = "windows")]
    {
        if !dir.join("aimem.exe").is_file() {
            return false;
        }
        let dir_str = dir.to_string_lossy().to_string();
        let current_path = std::env::var("PATH").unwrap_or_default();
        if current_path
            .split(';')
            .any(|p| p.trim().eq_ignore_ascii_case(&dir_str))
        {
            return true;
        }
        // Read User PATH via reg.exe (much faster than spawning powershell.exe)
        let existing_user_path = std::process::Command::new("reg")
            .args(["query", r"HKCU\Environment", "/v", "Path"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| {
                let text = String::from_utf8_lossy(&o.stdout);
                text.lines()
                    .find(|l| l.contains("REG_"))
                    .and_then(|l| l.split_once("REG_EXPAND_SZ").or_else(|| l.split_once("REG_SZ")))
                    .map(|(_, val)| val.trim().to_string())
            })
            .unwrap_or_default();

        if existing_user_path
            .split(';')
            .any(|p| p.trim().eq_ignore_ascii_case(&dir_str))
        {
            return true;
        }

        let next_path = if existing_user_path.trim().is_empty() {
            dir_str
        } else {
            format!("{};{}", existing_user_path.trim_end_matches(';'), dir_str)
        };
        let status = std::process::Command::new("reg")
            .args([
                "add",
                r"HKCU\Environment",
                "/v",
                "Path",
                "/t",
                "REG_EXPAND_SZ",
                "/d",
                &next_path,
                "/f",
            ])
            .status();
        return status.map(|s| s.success()).unwrap_or(false);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = dir;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_rule_file_preserves_user_content_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("aimem_rules_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("CLAUDE.md");

        std::fs::write(&file, "# My Rules\nKeep it simple.\n").unwrap();
        upsert_rule_file(&file).unwrap();
        let first = std::fs::read_to_string(&file).unwrap();
        assert!(first.contains("# My Rules\nKeep it simple."));
        assert!(first.contains(START_MARKER));
        assert!(first.contains(END_MARKER));

        // Second run must not duplicate the block
        upsert_rule_file(&file).unwrap();
        let second = std::fs::read_to_string(&file).unwrap();
        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cursor_mdc_gets_frontmatter_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("aimem_mdc_test_{}", uuid::Uuid::new_v4()));
        let file = dir.join("rules").join("aimem.mdc");

        upsert_rule_file(&file).unwrap();
        let first = std::fs::read_to_string(&file).unwrap();
        assert!(first.starts_with("---\ndescription:"));
        assert!(first.contains("alwaysApply: true"));
        assert!(first.contains(START_MARKER));

        upsert_rule_file(&file).unwrap();
        let second = std::fs::read_to_string(&file).unwrap();
        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_claude_settings_is_idempotent_and_preserves_existing() {
        let dir = std::env::temp_dir().join(format!("aimem_claude_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("settings.json");

        std::fs::write(
            &file,
            r#"{"permissions":{"allow":["Bash(find:*)"]},"theme":"dark"}"#,
        )
        .unwrap();

        upsert_claude_settings(&file).unwrap();
        let first = std::fs::read_to_string(&file).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(parsed["theme"], "dark");
        let allow = parsed["permissions"]["allow"].as_array().unwrap();
        assert!(allow.iter().any(|v| v == "Bash(find:*)"));
        assert!(allow.iter().any(|v| v == "Bash(aimem:*)"));
        assert_eq!(parsed["hooks"]["SessionStart"].as_array().unwrap().len(), 1);

        upsert_claude_settings(&file).unwrap();
        let second = std::fs::read_to_string(&file).unwrap();
        assert_eq!(first, second);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
