use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct DetectedProject {
    pub id: String,
    pub name: String,
    pub git_remote: Option<String>,
    pub root_path: PathBuf,
}

pub fn detect_project(start_dir: Option<&Path>) -> DetectedProject {
    let cwd = start_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let git_root = run_git(&cwd, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
    let root_path = git_root.unwrap_or_else(|| cwd.clone());

    let name = root_path
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("workspace")
        .to_string();

    let raw_remote = run_git(&root_path, &["config", "--get", "remote.origin.url"]);
    if let Some(ref remote) = raw_remote {
        if let Some(norm) = normalize_git_remote(remote) {
            return DetectedProject {
                id: norm,
                name,
                git_remote: Some(remote.clone()),
                root_path,
            };
        }
    }

    DetectedProject {
        id: format!("local/{}", name.to_lowercase()),
        name,
        git_remote: raw_remote,
        root_path,
    }
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub fn normalize_git_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    if trimmed.is_empty() {
        return None;
    }

    // Handle SSH style: git@github.com:user/repo
    if let Some(rest) = trimmed.strip_prefix("git@") {
        let normalized = rest.replacen(':', "/", 1).to_lowercase();
        return Some(normalized);
    }

    // Handle https://user@github.com/user/repo or ssh://git@host/repo
    if let Some(idx) = trimmed.find("://") {
        let after_scheme = &trimmed[idx + 3..];
        let without_auth = after_scheme
            .split_once('@')
            .map(|(_, right)| right)
            .unwrap_or(after_scheme);
        return Some(without_auth.to_lowercase());
    }

    Some(trimmed.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ssh_and_https_remotes_identically() {
        assert_eq!(
            normalize_git_remote("https://github.com/User/MyRepo.git").as_deref(),
            Some("github.com/user/myrepo")
        );
        assert_eq!(
            normalize_git_remote("git@github.com:User/MyRepo.git").as_deref(),
            Some("github.com/user/myrepo")
        );
    }
}
