//! Patch parsing and application.
//!
//! Patch-based edits are reviewable, auditable, and rejectable. Whole-file
//! rewrites are not. Every write into the workspace goes through this module.
//!
//! The parser understands unified diffs (`git diff` output, including a
//! `diff --git` preamble and `\ No newline at end of file` markers) and is
//! tolerant of the two most common model mistakes: wrong hunk line numbers and
//! stripped trailing context spaces.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{RaiError, Result};
use crate::util::{is_probably_binary, relative_to, safe_join, truncate_line};

/// How far the applier will search for a hunk whose line numbers have drifted.
const SEARCH_WINDOW: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

impl HunkLine {
    pub fn text(&self) -> &str {
        match self {
            HunkLine::Context(t) | HunkLine::Remove(t) | HunkLine::Add(t) => t,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub heading: String,
    pub lines: Vec<HunkLine>,
}

impl Hunk {
    fn added(&self) -> u64 {
        self.lines
            .iter()
            .filter(|l| matches!(l, HunkLine::Add(_)))
            .count() as u64
    }

    fn removed(&self) -> u64 {
        self.lines
            .iter()
            .filter(|l| matches!(l, HunkLine::Remove(_)))
            .count() as u64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilePatch {
    /// Path from the `---` line (`None` when the file is being created).
    pub old_path: Option<String>,
    /// Path from the `+++` line (`None` when the file is being deleted).
    pub new_path: Option<String>,
    pub hunks: Vec<Hunk>,
    /// Set when the diff ends with `\ No newline at end of file`.
    pub new_missing_final_newline: bool,
}

impl FilePatch {
    /// The path that will be written.
    pub fn target(&self) -> Option<&str> {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .filter(|p| !p.is_empty())
    }

    pub fn is_create(&self) -> bool {
        self.old_path.is_none()
    }

    pub fn is_delete(&self) -> bool {
        self.new_path.is_none()
    }

    pub fn added(&self) -> u64 {
        self.hunks.iter().map(|h| h.added()).sum()
    }

    pub fn removed(&self) -> u64 {
        self.hunks.iter().map(|h| h.removed()).sum()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    pub files: Vec<FilePatch>,
    /// Number of lenient repairs made while parsing (empty context lines).
    pub repairs: usize,
    /// Byte length of the source text the patch was parsed from.
    pub source_len: usize,
}

impl Patch {
    /// Total lines added across the patch.
    pub fn added(&self) -> u64 {
        self.files.iter().map(|f| f.added()).sum()
    }

    /// Total lines removed across the patch.
    pub fn removed(&self) -> u64 {
        self.files.iter().map(|f| f.removed()).sum()
    }

    /// Parse a unified diff.
    pub fn parse(text: &str) -> Result<Patch> {
        let normalized = text.replace("\r\n", "\n");
        let mut lines: Vec<&str> = normalized.split('\n').collect();
        // A diff ending in a newline would otherwise add a stray empty line to
        // the last hunk.
        if normalized.ends_with('\n') {
            lines.pop();
        }
        let mut files: Vec<FilePatch> = Vec::new();
        let mut repairs = 0usize;
        let mut i = 0usize;

        while i < lines.len() {
            let line = lines[i];

            // A `--- path` header is only a header when a `+++ path` follows.
            if let Some(old) = header_path(line, "---") {
                if i + 1 < lines.len() && lines[i + 1].starts_with("+++") {
                    let new = header_path(lines[i + 1], "+++").unwrap_or_default();
                    files.push(FilePatch {
                        old_path: norm_side(&old),
                        new_path: norm_side(&new),
                        hunks: Vec::new(),
                        new_missing_final_newline: false,
                    });
                    i += 2;
                    continue;
                }
            }

            if line.starts_with("@@") {
                let Some(file) = files.last_mut() else {
                    return Err(RaiError::Patch(format!(
                        "hunk header `{}` appears before any `---`/`+++` file header",
                        truncate_line(line, 80)
                    )));
                };
                let (hunk, consumed, repaired) = parse_hunk(&lines[i..])?;
                repairs += repaired;
                file.hunks.push(hunk);
                i += consumed;
                continue;
            }

            // A bare empty line inside a hunk is a stripped context line.
            if line.is_empty() && files.last().map(|f| !f.hunks.is_empty()).unwrap_or(false) {
                repairs += 1;
                if let Some(file) = files.last_mut() {
                    if let Some(hunk) = file.hunks.last_mut() {
                        hunk.lines.push(HunkLine::Context(String::new()));
                    }
                }
                i += 1;
                continue;
            }

            i += 1;
        }

        // Drop file headers that carry no hunks (pure rename metadata, etc).
        files.retain(|f| !f.hunks.is_empty());

        if files.is_empty() {
            return Err(RaiError::Patch(
                "no unified diff hunks found; expected `--- a/path`, `+++ b/path`, and `@@` headers"
                    .to_string(),
            ));
        }

        Ok(Patch {
            files,
            repairs,
            source_len: text.len(),
        })
    }

    /// Extract a diff from free-form model text.
    ///
    /// Prefers a fenced block tagged `diff`/`patch`, then falls back to the
    /// first unified-diff-looking region in the text.
    pub fn extract(text: &str) -> Option<String> {
        let normalized = text.replace("\r\n", "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();

        // 1. Fenced block.
        let mut i = 0;
        while i < lines.len() {
            let trimmed = lines[i].trim_start();
            if let Some(info) = trimmed
                .strip_prefix("```")
                .or_else(|| trimmed.strip_prefix("~~~"))
            {
                let tag = info.trim().to_ascii_lowercase();
                let mut body = Vec::new();
                let mut j = i + 1;
                while j < lines.len() {
                    let close = lines[j].trim_start();
                    if close.starts_with("```") || close.starts_with("~~~") {
                        break;
                    }
                    body.push(lines[j]);
                    j += 1;
                }
                let joined = body.join("\n");
                let looks_like_diff =
                    joined.contains("@@ ") || (joined.contains("+++ ") && joined.contains("--- "));
                if (tag.contains("diff") || tag.contains("patch") || tag.is_empty())
                    && looks_like_diff
                {
                    return Some(joined);
                }
                i = j;
                continue;
            }
            i += 1;
        }

        // 2. Bare diff region.
        let start = lines.iter().position(|l| {
            l.starts_with("diff --git ") || (l.starts_with("--- ") && l.contains('/'))
        })?;
        let body = lines[start..].join("\n");
        if body.contains("@@ ") {
            Some(body)
        } else {
            None
        }
    }
}

/// Parse `--- path` / `+++ path` into a path string.
fn header_path(line: &str, marker: &str) -> Option<String> {
    let rest = line.strip_prefix(marker)?;
    // `--- a/x\t2024-01-01 10:00:00` - timestamps are separated by a tab.
    let rest = rest.split('\t').next().unwrap_or(rest);
    Some(rest.trim().to_string())
}

/// Normalize one side of a file header into `None` (deleted/created) or a path.
fn norm_side(raw: &str) -> Option<String> {
    let cleaned = raw.trim();
    if cleaned.is_empty() || cleaned == "/dev/null" {
        return None;
    }
    let cleaned = cleaned
        .strip_prefix("a/")
        .or_else(|| cleaned.strip_prefix("b/"))
        .unwrap_or(cleaned);
    let cleaned = cleaned.trim_matches('"');
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.replace('\\', "/"))
    }
}

/// Strip a `NNN | ` gutter from a hunk line, if one is present.
///
/// Models frequently paste the line-numbered view of a file straight into a
/// patch. That is a formatting mistake, not a different intent, so the parser
/// repairs it instead of failing the hunk. Returns `(content, was_gutted)`.
fn strip_line_number_gutter(line: &str) -> (&str, bool) {
    let trimmed = line.trim_start();
    let digits = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits == 0 || digits >= trimmed.len() {
        return (line, false);
    }
    match trimmed[digits..].strip_prefix(" | ") {
        Some(content) => (content, true),
        None => (line, false),
    }
}

/// Parse one hunk starting at `lines[0]` (the `@@` header).
fn parse_hunk(lines: &[&str]) -> Result<(Hunk, usize, usize)> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$").expect("valid regex")
    });

    let header = lines[0];
    let caps = re
        .captures(header)
        .ok_or_else(|| RaiError::Patch(format!("malformed hunk header: {header}")))?;

    let num = |i: usize| -> usize {
        caps.get(i)
            .map(|m| m.as_str().parse::<usize>().unwrap_or(0))
            .unwrap_or(0)
    };

    let mut hunk = Hunk {
        old_start: num(1),
        old_count: {
            let v = num(2);
            if caps.get(2).is_none() {
                1
            } else {
                v
            }
        },
        new_start: num(3),
        new_count: {
            let v = num(4);
            if caps.get(4).is_none() {
                1
            } else {
                v
            }
        },
        heading: caps
            .get(5)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default(),
        lines: Vec::new(),
    };

    let mut i = 1usize;
    let mut repairs = 0usize;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with("@@") || line.starts_with("diff --git ") {
            break;
        }
        if line.starts_with("--- ") && i + 1 < lines.len() && lines[i + 1].starts_with("+++ ") {
            break;
        }
        if line.starts_with('\\') {
            // `\ No newline at end of file`
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix('+') {
            let (content, gutted) = strip_line_number_gutter(rest);
            if gutted {
                repairs += 1;
            }
            hunk.lines.push(HunkLine::Add(content.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            let (content, gutted) = strip_line_number_gutter(rest);
            if gutted {
                repairs += 1;
            }
            hunk.lines.push(HunkLine::Remove(content.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            let (content, gutted) = strip_line_number_gutter(rest);
            if gutted {
                repairs += 1;
            }
            hunk.lines.push(HunkLine::Context(content.to_string()));
        } else if line.is_empty() {
            // A blank line means the leading space was stripped somewhere in
            // transit; treat it as empty context and record the repair.
            hunk.lines.push(HunkLine::Context(String::new()));
            repairs += 1;
        } else {
            break;
        }
        i += 1;
    }

    if hunk.lines.is_empty() {
        return Err(RaiError::Patch(format!(
            "hunk `{}` contains no lines",
            truncate_line(header, 80)
        )));
    }

    Ok((hunk, i, repairs))
}

/// Options controlling patch application.
#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub root: PathBuf,
    pub allow_writes: bool,
    pub max_file_bytes: u64,
    pub dry_run: bool,
}

/// What happened to one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedFile {
    pub path: String,
    pub action: FileAction,
    pub added: u64,
    pub removed: u64,
    /// `Some(true)` when the file already had uncommitted changes.
    pub preexisting_changes: Option<bool>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    Created,
    Modified,
    Deleted,
}

/// Apply every file in the patch.
///
/// Either all files apply or the call fails: the workspace is left untouched on
/// error. Writes land atomically (temp file + rename).
pub fn apply(patch: &Patch, opts: &ApplyOptions) -> Result<Vec<AppliedFile>> {
    if !opts.allow_writes {
        return Err(RaiError::PolicyDenied {
            tool: "apply_patch".into(),
            reason: "workspace.allow_writes is false in configuration".into(),
        });
    }

    // Validate and compute every new file body before touching the filesystem.
    let mut planned: Vec<(PathBuf, String, FileAction, u64, u64, String)> = Vec::new();

    for file in &patch.files {
        let Some(raw) = file.target() else {
            return Err(RaiError::Patch("file entry has no target path".into()));
        };
        let abs = safe_join(&opts.root, raw)?;
        let rel = relative_to(&opts.root, &abs);

        let was_present = abs.exists();
        let existing = if was_present {
            Some(read_text(&abs, opts.max_file_bytes)?)
        } else {
            None
        };

        if file.is_delete() {
            let Some(body) = existing.as_deref() else {
                return Err(RaiError::Patch(format!(
                    "cannot delete `{rel}`: file does not exist"
                )));
            };
            let lines = split_lines(body);
            let (_, added, removed) = apply_hunks(&lines, &file.hunks)
                .map_err(|e| RaiError::Patch(format!("{rel}: {e}")))?;
            planned.push((abs, String::new(), FileAction::Deleted, added, removed, rel));
            continue;
        }

        let body = existing.unwrap_or_default();
        if file.is_create() && !body.is_empty() {
            return Err(RaiError::Patch(format!(
                "cannot create `{rel}`: file already exists"
            )));
        }
        let lines = split_lines(&body);
        let (new_lines, added, removed) =
            apply_hunks(&lines, &file.hunks).map_err(|e| RaiError::Patch(format!("{rel}: {e}")))?;

        let mut text = new_lines.join("\n");
        if !text.is_empty() && !file.new_missing_final_newline {
            text.push('\n');
        }
        if text.contains('\0') {
            return Err(RaiError::Patch(format!(
                "refusing to write `{rel}`: result contains NUL bytes"
            )));
        }
        if text.len() as u64 > opts.max_file_bytes {
            return Err(RaiError::Patch(format!(
                "refusing to write `{rel}`: {} bytes exceeds the {} byte limit",
                text.len(),
                opts.max_file_bytes
            )));
        }

        let action = if !was_present {
            FileAction::Created
        } else {
            FileAction::Modified
        };
        planned.push((abs, text, action, added, removed, rel));
    }

    let mut applied = Vec::new();
    if !opts.dry_run {
        for (abs, text, action, ..) in &planned {
            match action {
                FileAction::Deleted => {
                    std::fs::remove_file(abs)?;
                }
                _ => write_atomic(abs, text)?,
            }
        }
    }

    for (_abs, text, action, added, removed, rel) in planned {
        applied.push(AppliedFile {
            path: rel,
            action,
            added,
            removed,
            preexisting_changes: None,
            bytes: text.len() as u64,
        });
    }

    Ok(applied)
}

/// Number of lines a hunk will replace, derived from the hunk body itself.
fn apply_hunks(lines: &[String], hunks: &[Hunk]) -> Result<(Vec<String>, u64, u64)> {
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut added = 0u64;
    let mut removed = 0u64;

    for hunk in hunks {
        let pattern: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|l| !matches!(l, HunkLine::Add(_)))
            .map(|l| l.text())
            .collect();
        let expected = hunk.old_start.saturating_sub(1);

        let Some(pos) = find_anchor(lines, &pattern, expected, cursor) else {
            return Err(RaiError::Patch(format!(
                "hunk @@ -{},{} +{},{} @@ (\"{}\") did not match the file contents",
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
                truncate_line(&hunk.heading, 60)
            )));
        };

        if pos < cursor {
            return Err(RaiError::Patch(
                "hunks are out of order; re-generate the diff against the current file".into(),
            ));
        }

        out.extend(lines[cursor..pos].iter().cloned());
        for line in &hunk.lines {
            match line {
                HunkLine::Context(text) => out.push(text.clone()),
                HunkLine::Add(text) => {
                    out.push(text.clone());
                    added += 1;
                }
                HunkLine::Remove(_) => {
                    removed += 1;
                }
            }
        }
        cursor = pos + pattern.len();
    }

    out.extend(lines[cursor..].iter().cloned());
    Ok((out, added, removed))
}

/// Locate `pattern` near `expected`, scanning outward so that drifted line
/// numbers still apply.
fn find_anchor(lines: &[String], pattern: &[&str], expected: usize, min: usize) -> Option<usize> {
    if pattern.is_empty() {
        return Some(expected.min(lines.len()).max(min));
    }
    let matches_at = |pos: usize| -> bool {
        if pos < min || pos + pattern.len() > lines.len() {
            return false;
        }
        pattern
            .iter()
            .enumerate()
            .all(|(i, want)| lines[pos + i] == *want)
    };

    if matches_at(expected) {
        return Some(expected);
    }
    let max_pos = lines.len().saturating_sub(pattern.len());
    for delta in 1..=SEARCH_WINDOW {
        let back = expected.checked_sub(delta);
        if let Some(p) = back {
            if matches_at(p) {
                return Some(p);
            }
        }
        let forward = expected + delta;
        if forward <= max_pos && matches_at(forward) {
            return Some(forward);
        }
        if back.is_none() && forward > max_pos {
            break;
        }
    }
    None
}

/// Split file text into lines without terminators.
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<String> = text.split('\n').map(|l| l.to_string()).collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn read_text(path: &Path, max_file_bytes: u64) -> Result<String> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_file_bytes {
        return Err(RaiError::FileTooLarge {
            path: path.display().to_string(),
            size: meta.len(),
            limit: max_file_bytes,
        });
    }
    let bytes = std::fs::read(path)?;
    if is_probably_binary(&bytes) {
        return Err(RaiError::NotText {
            path: path.display().to_string(),
        });
    }
    String::from_utf8(bytes).map_err(|_| RaiError::NotText {
        path: path.display().to_string(),
    })
}

/// Write via a sibling temp file so a crash cannot truncate the original.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.rai-tmp-{}",
        path.file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into()),
        crate::util::short_id("")
    ));
    std::fs::write(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_diff() -> String {
        [
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -1,4 +1,5 @@",
            " pub fn one() {}",
            "-pub fn two() {}",
            "+pub fn two() -> u32 { 2 }",
            "+pub fn three() {}",
            " pub fn four() {}",
            " pub fn five() {}",
            "",
        ]
        .join("\n")
    }

    #[test]
    fn parses_a_simple_diff() {
        let patch = Patch::parse(&sample_diff()).unwrap();
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.files[0].target(), Some("src/lib.rs"));
        assert_eq!(patch.added(), 2);
        assert_eq!(patch.removed(), 1);
    }

    #[test]
    fn applies_and_reports_stats() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn one() {}\npub fn two() {}\npub fn four() {}\npub fn five() {}\n",
        )
        .unwrap();

        let patch = Patch::parse(&sample_diff()).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        let applied = apply(&patch, &opts).unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].added, 2);
        assert_eq!(applied[0].removed, 1);

        let body = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(body.contains("pub fn two() -> u32 { 2 }"));
        assert!(body.contains("pub fn three() {}"));
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn one() {}\npub fn two() {}\npub fn four() {}\npub fn five() {}\n",
        )
        .unwrap();
        let patch = Patch::parse(&sample_diff()).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: true,
        };
        apply(&patch, &opts).unwrap();
        let body = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(!body.contains("three"));
    }

    #[test]
    fn tolerates_drifted_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let body = (0..60)
            .map(|i| format!("line {i}"))
            .chain(["target".to_string()])
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(dir.path().join("src/big.rs"), &body).unwrap();

        // The hunk pretends `target` lives at line 5, not line 61.
        let diff = [
            "--- a/src/big.rs",
            "+++ b/src/big.rs",
            "@@ -5,1 +5,2 @@",
            " target",
            "+inserted",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&diff).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        apply(&patch, &opts).unwrap();
        let out = std::fs::read_to_string(dir.path().join("src/big.rs")).unwrap();
        assert!(out.contains("target\ninserted\n"));
    }

    #[test]
    fn creates_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let diff = [
            "--- /dev/null",
            "+++ b/docs/new.md",
            "@@ -0,0 +1,2 @@",
            "+hello",
            "+world",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&diff).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        let applied = apply(&patch, &opts).unwrap();
        assert_eq!(applied[0].action, FileAction::Created);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("docs/new.md")).unwrap(),
            "hello\nworld\n"
        );
    }

    #[test]
    fn rejects_paths_outside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let diff = [
            "--- a/../../etc/passwd",
            "+++ b/../../etc/passwd",
            "@@ -1,1 +1,1 @@",
            "-x",
            "+y",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&diff).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        let err = apply(&patch, &opts).unwrap_err();
        assert_eq!(err.kind(), "path_escape");
    }

    #[test]
    fn refuses_writes_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let patch = Patch::parse(&sample_diff()).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: false,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        let err = apply(&patch, &opts).unwrap_err();
        assert_eq!(err.kind(), "policy_denied");
    }

    #[test]
    fn reports_unmatched_hunks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "completely different\n").unwrap();
        let patch = Patch::parse(&sample_diff()).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        let err = apply(&patch, &opts).unwrap_err();
        assert_eq!(err.kind(), "patch");
    }

    #[test]
    fn deletion_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gone.txt"), "a\nb\n").unwrap();
        let diff = [
            "--- a/gone.txt",
            "+++ /dev/null",
            "@@ -1,2 +0,0 @@",
            "-a",
            "-b",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&diff).unwrap();
        let opts = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        apply(&patch, &opts).unwrap();
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[test]
    fn extracts_diff_from_a_fenced_block() {
        let text = format!(
            "Here is the change:\n\n```diff\n{}\n```\nDone.",
            sample_diff()
        );
        let extracted = Patch::extract(&text).unwrap();
        assert!(Patch::parse(&extracted).is_ok());
    }

    #[test]
    fn extracts_bare_diff() {
        let text = format!("Sure.\n{}\n", sample_diff());
        let extracted = Patch::extract(&text).unwrap();
        assert!(extracted.starts_with("--- a/src/lib.rs"));
    }

    #[test]
    fn no_diff_present_returns_none() {
        assert!(Patch::extract("just prose, no changes").is_none());
    }

    #[test]
    fn tolerates_stripped_context_line() {
        let text = [
            "--- a/f.txt",
            "+++ b/f.txt",
            "@@ -1,3 +1,3 @@",
            " one",
            "",
            "-three",
            "+THREE",
            "",
        ]
        .join("\n");
        let patch = Patch::parse(&text).unwrap();
        assert!(patch.repairs >= 1);
        assert_eq!(patch.files[0].hunks[0].lines.len(), 4);
    }

    /// Models often paste the line-numbered file view into a patch. The parser
    /// repairs that instead of failing the hunk.
    #[test]
    fn tolerates_a_line_number_gutter_in_hunk_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "fn main() {\n    let x = 1;\n}\n",
        )
        .unwrap();

        let diff = [
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -1,3 +1,3 @@",
            "    1 | fn main() {",
            "-    2 |     let x = 1;",
            "+    2 |     let x = 2;",
            "    3 | }",
            "",
        ]
        .join("\n");

        let patch = Patch::parse(&diff).expect("parses");
        assert!(patch.repairs >= 3, "gutter removal is counted");
        let options = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        apply(&patch, &options).expect("applies after repair");
        assert!(std::fs::read_to_string(dir.path().join("src/lib.rs"))
            .unwrap()
            .contains("let x = 2;"));
    }
}
