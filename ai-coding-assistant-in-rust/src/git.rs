//! Git helpers.
//!
//! Git work is quick but synchronous; every call is pushed onto a blocking pool
//! so it never stalls the async runtime.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{RaiError, Result};
use crate::util::truncate_bytes;

/// One entry of `git status --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// Two-character porcelain status, e.g. ` M`, `??`, `A `.
    pub status: String,
    pub path: String,
}

impl ChangedFile {
    pub fn is_untracked(&self) -> bool {
        self.status.starts_with("??")
    }

    /// Whether the file has content that is not in `HEAD`.
    pub fn is_modified(&self) -> bool {
        !self.status.starts_with("??")
    }
}

/// Branch/HEAD/dirty summary of a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitInfo {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub changed: Vec<ChangedFile>,
}

impl GitInfo {
    pub fn dirty(&self) -> bool {
        self.changed.iter().any(|c| c.is_modified())
    }
}

/// Run `git` in `root` on a blocking thread.
pub async fn git(root: &Path, args: &[&str]) -> Result<(i32, String, String)> {
    let root = root.to_path_buf();
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    tokio::task::spawn_blocking(move || git_blocking(&root, &args))
        .await
        .map_err(|e| RaiError::Io(std::io::Error::other(e.to_string())))?
}

/// Blocking `git` invocation. Returns `(exit_code, stdout, stderr)`.
pub fn git_blocking(root: &Path, args: &[String]) -> Result<(i32, String, String)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|e| RaiError::CommandSpawn {
            command: format!("git {}", args.join(" ")),
            problem: e.to_string(),
        })?;
    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Collect branch, HEAD, and working-tree state.
pub async fn info(root: &Path) -> GitInfo {
    let mut info = GitInfo::default();
    let Ok((code, out, _)) = git(root, &["rev-parse", "--is-inside-work-tree"]).await else {
        return info;
    };
    if code != 0 || !out.trim().eq_ignore_ascii_case("true") {
        return info;
    }
    info.is_repo = true;

    if let Ok((0, out, _)) = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        info.branch = Some(out.trim().to_string());
    }
    if let Ok((0, out, _)) = git(root, &["rev-parse", "--short", "HEAD"]).await {
        info.head = Some(out.trim().to_string());
    }
    if let Ok((0, out, _)) = git(root, &["status", "--porcelain"]).await {
        info.changed = parse_porcelain(&out);
    }
    info
}

/// Parse `git status --porcelain` output.
pub fn parse_porcelain(text: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    for line in text.lines() {
        if line.len() < 4 {
            continue;
        }
        let (status, rest) = line.split_at(2);
        let path = rest.trim_start();
        // Renames are reported as `old -> new`; keep the destination.
        let path = match path.rsplit_once(" -> ") {
            Some((_, new)) => new,
            None => path,
        };
        files.push(ChangedFile {
            status: status.to_string(),
            path: unquote(path),
        });
    }
    files
}

fn unquote(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\\\"", "\"")
    } else {
        trimmed.to_string()
    }
}

/// Whether a single path has uncommitted modifications.
///
/// Untracked files are reported as `false` because they have no committed
/// baseline to diverge from. `None` means "not a git repository".
pub async fn path_is_modified(root: &Path, relative: &str) -> Option<bool> {
    let info = info(root).await;
    if !info.is_repo {
        return None;
    }
    let normalized = relative.replace('\\', "/");
    Some(
        info.changed
            .iter()
            .any(|c| c.is_modified() && c.path.replace('\\', "/") == normalized),
    )
}

/// `git diff` output for the worktree (and optionally the index).
pub async fn diff(
    root: &Path,
    staged: bool,
    path: Option<&str>,
    max_bytes: usize,
) -> Result<(String, bool)> {
    let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff"];
    if staged {
        args.push("--cached");
    }
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    let (code, out, err) = git(root, &args).await?;
    if code != 0 && out.trim().is_empty() {
        return Err(RaiError::Mcp(format!("git diff failed: {}", err.trim())));
    }
    let (text, truncated) = truncate_bytes(&out, max_bytes);
    Ok((text, truncated))
}

/// Path of the repository root-relative `.git` directory, if any.
pub async fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let (code, out, _) = git(cwd, &["rev-parse", "--show-toplevel"]).await.ok()?;
    if code != 0 {
        return None;
    }
    let path = out.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_status() {
        let files = parse_porcelain(" M src/main.rs\n?? new.txt\nA  added.rs\n");
        assert_eq!(files.len(), 3);
        assert!(files[0].is_modified());
        assert!(files[1].is_untracked());
        assert_eq!(files[2].path, "added.rs");
    }

    #[test]
    fn parses_renames_to_destination() {
        let files = parse_porcelain("R  old.rs -> new.rs\n");
        assert_eq!(files[0].path, "new.rs");
    }

    #[test]
    fn untracked_is_not_modified() {
        let files = parse_porcelain("?? scratch.txt\n");
        assert!(!files[0].is_modified());
        assert!(files[0].is_untracked());
    }
}
