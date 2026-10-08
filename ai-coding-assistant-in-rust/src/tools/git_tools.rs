//! Git tools: status, diff, change summary, and failure explanation.
//!
//! `summarize_diff` and `explain_failure` are deterministic on purpose. A model
//! can narrate them, but the factual work log does not depend on a model being
//! reachable, so `rai review` stays useful offline.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    object_schema, optional_bool, optional_str, optional_usize, require_str, Tool, ToolContext,
    ToolDefinition, ToolOutcome,
};
use crate::error::Result;
use crate::git;
use crate::util::human_bytes;

/// `git status` for the workspace.
pub struct GitStatus;

#[async_trait]
impl Tool for GitStatus {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "git_status",
            "Report the repository branch, HEAD, and changed files.",
            object_schema(json!({}), &[]),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(
        &self,
        _args: Value,
        ctx: &ToolContext,
        _call_id: &str,
    ) -> Result<ToolOutcome> {
        let info = git::info(&ctx.root).await;
        if !info.is_repo {
            return Ok(ToolOutcome::new(
                "not a git repository",
                "not a git repository: git tools are unavailable",
            ));
        }
        let mut text = format!(
            "branch: {}\nhead: {}\nchanged: {}\n---\n",
            info.branch.clone().unwrap_or_else(|| "detached".into()),
            info.head.clone().unwrap_or_else(|| "none".into()),
            info.changed.len()
        );
        for file in &info.changed {
            text.push_str(&format!("{} {}\n", file.status, file.path));
        }
        Ok(
            ToolOutcome::new(format!("{} changed file(s)", info.changed.len()), text)
                .with_data(json!({ "dirty": info.dirty(), "changed": info.changed.len() })),
        )
    }
}

/// `git diff` for the worktree or the index.
pub struct GitDiff;

#[async_trait]
impl Tool for GitDiff {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "git_diff",
            "Return the git diff for the working tree, or the staged changes. Use before proposing edits.",
            object_schema(
                json!({
                    "staged": {
                        "type": "boolean",
                        "description": "Diff the index instead of the working tree."
                    },
                    "path": {
                        "type": "string",
                        "description": "Limit the diff to one path."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "Maximum diff bytes to return."
                    }
                }),
                &[],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let staged = optional_bool(&args, "staged").unwrap_or(false);
        let path = optional_str(&args, "path");
        let max_bytes = optional_usize(&args, "max_bytes")
            .unwrap_or(64 * 1024)
            .clamp(1024, 512 * 1024);

        let (diff, truncated) = git::diff(&ctx.root, staged, path.as_deref(), max_bytes).await?;
        if diff.trim().is_empty() {
            return Ok(ToolOutcome::new(
                "no diff",
                "no changes in the requested diff scope",
            ));
        }
        let text = format!(
            "staged: {staged}\nbytes: {}\ntruncated: {truncated}\n---\n{diff}",
            human_bytes(diff.len() as u64)
        );
        Ok(ToolOutcome::new(
            format!("diff of {} bytes", human_bytes(diff.len() as u64)),
            text,
        )
        .with_data(json!({ "staged": staged, "truncated": truncated }))
        .truncated_if(truncated))
    }
}

/// One risk found in a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// `high`, `medium`, `low`, or `info`.
    pub severity: String,
    pub kind: String,
    pub path: Option<String>,
    pub detail: String,
}

impl Finding {
    fn new(severity: &str, kind: &str, path: Option<String>, detail: String) -> Self {
        Self {
            severity: severity.to_string(),
            kind: kind.to_string(),
            path,
            detail,
        }
    }
}

/// Per-file change counts, parsed from a unified diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub path: String,
    pub added: u64,
    pub removed: u64,
}

/// Count added and removed lines per file in a unified diff.
pub fn diff_stats(diff: &str) -> Vec<FileStat> {
    let mut stats: Vec<FileStat> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = rest.trim();
            let path = path
                .strip_prefix("b/")
                .unwrap_or(path)
                .split('\t')
                .next()
                .unwrap_or(path)
                .to_string();
            if path == "/dev/null" {
                continue;
            }
            if !stats.iter().any(|s| s.path == path) {
                stats.push(FileStat {
                    path,
                    added: 0,
                    removed: 0,
                });
            }
        } else if line.starts_with("---") || line.starts_with("+++") {
            continue;
        } else if line.starts_with('+') {
            if let Some(last) = stats.last_mut() {
                last.added += 1;
            }
        } else if line.starts_with('-') {
            if let Some(last) = stats.last_mut() {
                last.removed += 1;
            }
        }
    }
    stats
}

/// Deterministic risk scan over a unified diff.
pub fn scan_diff(diff: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut current: Option<String> = None;
    let mut per_file_removed: std::collections::BTreeMap<String, u64> = Default::default();

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = rest.trim();
            current = Some(
                path.strip_prefix("b/")
                    .unwrap_or(path)
                    .split('\t')
                    .next()
                    .unwrap_or(path)
                    .to_string(),
            );
            continue;
        }
        if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
            continue;
        }
        let Some(path) = current.clone() else {
            continue;
        };

        if let Some(added) = line.strip_prefix('+') {
            let added = added.trim();
            if added.is_empty() {
                continue;
            }
            let lowered = added.to_ascii_lowercase();

            if crate::redact::looks_like_secret(added) {
                findings.push(Finding::new(
                    "high",
                    "possible-secret",
                    Some(path.clone()),
                    format!(
                        "added a value that looks like a credential: {}",
                        crate::util::truncate_line(added, 80)
                    ),
                ));
            }
            if is_env_file(&path) {
                findings.push(Finding::new(
                    "high",
                    "env-file",
                    Some(path.clone()),
                    "changed an environment file; confirm no secrets are committed".into(),
                ));
            }
            if is_ci_file(&path) {
                findings.push(Finding::new(
                    "medium",
                    "ci-change",
                    Some(path.clone()),
                    "changed CI configuration".into(),
                ));
            }
            if is_lockfile(&path) {
                findings.push(Finding::new(
                    "info",
                    "lockfile-change",
                    Some(path.clone()),
                    "dependency lockfile changed".into(),
                ));
            }
            if path.ends_with(".rs") && contains_word(added, "unsafe") {
                findings.push(Finding::new(
                    "medium",
                    "unsafe-added",
                    Some(path.clone()),
                    "added an `unsafe` block".into(),
                ));
            }
            if path.ends_with(".rs")
                && (contains_word(added, "unwrap") || contains_word(added, "expect"))
                && !lowered.contains("test")
            {
                findings.push(Finding::new(
                    "low",
                    "panic-risk",
                    Some(path.clone()),
                    "added a call that can panic".into(),
                ));
            }
            if contains_word(&lowered, "todo") || contains_word(&lowered, "fixme") {
                findings.push(Finding::new(
                    "info",
                    "todo-added",
                    Some(path.clone()),
                    "added a TODO/FIXME marker".into(),
                ));
            }
            if lowered.contains("allow(") && path.ends_with(".rs") {
                findings.push(Finding::new(
                    "low",
                    "lint-suppression",
                    Some(path.clone()),
                    "added an `allow(...)` attribute".into(),
                ));
            }
        } else if line.starts_with('-') {
            *per_file_removed.entry(path.clone()).or_insert(0) += 1;
            let removed = line.strip_prefix('-').unwrap_or(line).trim();
            if is_test_path(&path) && !removed.is_empty() {
                findings.push(Finding::new(
                    "high",
                    "test-removal",
                    Some(path.clone()),
                    format!(
                        "removed a test line: {}",
                        crate::util::truncate_line(removed, 80)
                    ),
                ));
            }
        }
    }

    for (path, removed) in per_file_removed {
        if removed >= 100 {
            findings.push(Finding::new(
                "medium",
                "large-deletion",
                Some(path),
                format!("removed {removed} lines from one file"),
            ));
        }
    }

    findings.sort_by(|a, b| {
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then_with(|| a.path.cmp(&b.path))
    });
    findings.dedup();
    findings
}

fn severity_rank(severity: &str) -> u8 {
    match severity {
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        name,
        "Cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "poetry.lock"
            | "go.sum"
            | "Gemfile.lock"
            | "composer.lock"
    )
}

fn is_ci_file(path: &str) -> bool {
    path.starts_with(".github/workflows/")
        || path.starts_with(".github/actions/")
        || path.contains(".gitlab-ci")
        || path.contains("azure-pipelines")
        || path.starts_with(".circleci/")
}

fn is_env_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.starts_with(".env") || name.ends_with(".env")
}

fn is_test_path(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    lowered.contains("test") || lowered.contains("spec")
}

/// Summarize the current diff: stats plus deterministic risk findings.
pub struct SummarizeDiff;

#[async_trait]
impl Tool for SummarizeDiff {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "summarize_diff",
            "Summarize the current git diff: per-file additions and removals plus a deterministic risk scan.",
            object_schema(
                json!({
                    "staged": {
                        "type": "boolean",
                        "description": "Summarize the index instead of the working tree."
                    }
                }),
                &[],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let staged = optional_bool(&args, "staged").unwrap_or(false);
        let (diff, truncated) = git::diff(&ctx.root, staged, None, 512 * 1024).await?;
        if diff.trim().is_empty() {
            return Ok(ToolOutcome::new(
                "no diff to summarize",
                "no changes in the requested diff scope",
            ));
        }

        let stats = diff_stats(&diff);
        let findings = scan_diff(&diff);
        let added: u64 = stats.iter().map(|s| s.added).sum();
        let removed: u64 = stats.iter().map(|s| s.removed).sum();

        let mut text = format!(
            "files_changed: {}\nlines_added: {added}\nlines_removed: {removed}\nrisk_findings: {}\n---\n",
            stats.len(),
            findings.len()
        );
        for stat in &stats {
            text.push_str(&format!(
                "{} +{} -{}\n",
                stat.path, stat.added, stat.removed
            ));
        }
        if !findings.is_empty() {
            text.push_str("--- findings ---\n");
            for finding in &findings {
                text.push_str(&format!(
                    "[{}] {} {} - {}\n",
                    finding.severity,
                    finding.kind,
                    finding.path.clone().unwrap_or_else(|| "-".into()),
                    finding.detail
                ));
            }
        }

        Ok(ToolOutcome::new(
            format!(
                "{} file(s), +{added} -{removed}, {} finding(s)",
                stats.len(),
                findings.len()
            ),
            text,
        )
        .with_data(json!({
            "files": stats.len(),
            "added": added,
            "removed": removed,
            "findings": findings,
            "truncated": truncated,
        })))
    }
}

/// Turn failed command output into a structured diagnosis.
///
/// This runs locally and deterministically. A model can add narrative on top,
/// but the facts come from the output itself.
pub struct ExplainFailure;

#[async_trait]
impl Tool for ExplainFailure {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "explain_failure",
            "Summarize a failed build or test output: error kind, first error, file locations, and suggested next step.",
            object_schema(
                json!({
                    "output": {
                        "type": "string",
                        "description": "Captured stdout/stderr from the failed command."
                    },
                    "command": {
                        "type": "string",
                        "description": "The command that produced the output, used for suggestions."
                    }
                }),
                &["output"],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let raw = require_str(&args, "output", "explain_failure")?;
        let command = optional_str(&args, "command");
        let output = ctx.redactor.clean(&raw);
        let report = explain_failure(&output);

        let mut text = format!("kind: {}\nsummary: {}\n", report.kind, report.summary);
        if let Some(first) = &report.first_error {
            text.push_str(&format!("first_error: {first}\n"));
        }
        for location in &report.locations {
            text.push_str(&format!("location: {location}\n"));
        }
        for failure in &report.test_failures {
            text.push_str(&format!("failing_test: {failure}\n"));
        }
        if !report.highlights.is_empty() {
            text.push_str("---\n");
            for line in &report.highlights {
                text.push_str(line);
                text.push('\n');
            }
        }
        if !report.suggestions.is_empty() {
            text.push_str("---\nnext steps:\n");
            for suggestion in &report.suggestions {
                text.push_str(&format!("- {suggestion}\n"));
            }
        }
        if let Some(command) = command {
            text.push_str(&format!("command: {command}\n"));
        }

        let data = serde_json::to_value(&report).unwrap_or(Value::Null);
        Ok(ToolOutcome::new(report.summary.clone(), text).with_data(data))
    }
}

/// Structured diagnosis of a failed command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FailureReport {
    /// `compile-error`, `test-failure`, `assertion`, `runtime-error`,
    /// `missing-dependency`, or `unknown`.
    pub kind: String,
    pub summary: String,
    pub first_error: Option<String>,
    /// `path:line` (or `path:line:col`) references found in the output.
    pub locations: Vec<String>,
    /// Failing test names.
    pub test_failures: Vec<String>,
    /// The most informative raw lines.
    pub highlights: Vec<String>,
    pub suggestions: Vec<String>,
}

impl FailureReport {
    fn new(kind: &str, summary: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            summary: summary.into(),
            first_error: None,
            locations: Vec::new(),
            test_failures: Vec::new(),
            highlights: Vec::new(),
            suggestions: Vec::new(),
        }
    }
}

/// Analyse captured output. Offline, deterministic, and safe to unit test.
pub fn explain_failure(output: &str) -> FailureReport {
    let lines: Vec<&str> = output.lines().collect();
    let mut compile = Vec::new();
    let mut test_failures = Vec::new();
    let mut locations = Vec::new();
    let mut assertions = Vec::new();
    let mut runtime = Vec::new();
    let mut missing = Vec::new();
    let mut panics = Vec::new();

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_compile_error(trimmed) {
            compile.push(trimmed.to_string());
        }
        if let Some(name) = failing_test_name(trimmed) {
            test_failures.push(name);
        }
        if let Some(location) = location_of(trimmed) {
            if !locations.contains(&location) {
                locations.push(location);
            }
        }
        if trimmed.starts_with("assertion") || trimmed.contains("assertion failed") {
            assertions.push(trimmed.to_string());
        }
        if trimmed.contains("panicked at") {
            panics.push(trimmed.to_string());
        }
        if trimmed.starts_with("npm ERR!") || trimmed.contains("ModuleNotFoundError") {
            missing.push(trimmed.to_string());
        }
        if trimmed.starts_with("Traceback (most recent call last)") {
            runtime.push(trimmed.to_string());
        }
    }

    let mut report = if let Some(first) = compile.first() {
        let mut report = FailureReport::new(
            "compile-error",
            format!("{} compiler error(s); first: {}", compile.len(), first),
        );
        report.first_error = Some(first.clone());
        report.suggestions.push(
            "Fix the first compiler error before re-running: later errors are often consequences."
                .into(),
        );
        report
            .suggestions
            .push("Prefer the cheapest verification that answers the question, e.g. `cargo check` before `cargo test`.".into());
        report
    } else if !test_failures.is_empty() {
        let mut report = FailureReport::new(
            "test-failure",
            format!("{} failing test(s)", test_failures.len()),
        );
        report
            .suggestions
            .push("Re-run a single failing test to get a focused failure output.".into());
        report
    } else if let Some(first) = panics.first().or_else(|| assertions.first()) {
        let mut report =
            FailureReport::new("runtime-error", format!("panic or assertion: {first}"));
        report.first_error = Some(first.clone());
        report.suggestions.push(
            "Read the assertion values: they usually show the intended versus actual behaviour."
                .into(),
        );
        report
    } else if let Some(first) = missing.first() {
        let mut report =
            FailureReport::new("missing-dependency", format!("missing dependency: {first}"));
        report.first_error = Some(first.clone());
        report
            .suggestions
            .push("Install or declare the dependency before re-running the command.".into());
        report
    } else if !runtime.is_empty() {
        let mut report = FailureReport::new("runtime-error", "uncaught exception");
        report
            .suggestions
            .push("Start from the deepest frame in the traceback that mentions your code.".into());
        report
    } else {
        let mut report = FailureReport::new("unknown", "no recognized error pattern in output");
        report.suggestions.push(
            "Inspect the tail of the output; if it is empty the command may have been killed or timed out.".into(),
        );
        report
    };

    report.test_failures = test_failures;
    report.locations = locations;
    report.highlights = lines
        .iter()
        .map(|l| l.trim_end())
        .filter(|l| {
            is_compile_error(l.trim())
                || l.contains("panicked at")
                || l.starts_with("assertion")
                || l.starts_with("--- FAIL")
                || l.starts_with("error:")
                || l.starts_with("Error:")
                || l.starts_with("npm ERR!")
                || l.contains("ModuleNotFoundError")
                || l.trim_start().starts_with("-->")
        })
        .take(20)
        .map(str::to_string)
        .collect();
    if report.highlights.is_empty() {
        report.highlights = lines
            .iter()
            .rev()
            .take(10)
            .rev()
            .map(|l| l.trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect();
    }
    report
}

fn is_compile_error(line: &str) -> bool {
    if line.starts_with("error[E") || line.starts_with("error:") {
        return true;
    }
    // TypeScript / generic tsc style, but not every line containing "error".
    line.starts_with("src/") && line.contains("error TS")
}

fn failing_test_name(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("test ") {
        if let Some(name) = rest.strip_suffix(" ... FAILED") {
            return Some(name.trim().to_string());
        }
    }
    if let Some(rest) = line.strip_prefix("--- FAIL: ") {
        return Some(rest.split_whitespace().next().unwrap_or(rest).to_string());
    }
    None
}

/// Extract `path:line[:col]` references, preferring paths that look like source.
fn location_of(line: &str) -> Option<String> {
    let candidate = line
        .trim_start_matches("--> ")
        .split_whitespace()
        .find(|token| {
            let token = token.trim_matches(|c: char| c == '(' || c == ')' || c == ',' || c == ';');
            if !token.contains(':') || token.contains("://") {
                return false;
            }
            // Accept both `path:line:col` (rustc) and `path:line:` (go test).
            let mut parts = token.split(':');
            let _path = parts.next();
            parts
                .next()
                .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        })?;
    let cleaned = candidate
        .trim_matches(|c: char| c == '(' || c == ')' || c == ',' || c == ';')
        .trim_end_matches(':');
    Some(cleaned.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO_ERROR: &str = "   Compiling rai v0.1.0\n\
error[E0308]: mismatched types\n\
  --> src/main.rs:42:17\n\
   |\n42 |     let x: u32 = \"text\";\n\
   |            ---   ^^^^^^ expected `u32`, found `&str`\n\
error: aborting due to 1 previous error\n";

    #[test]
    fn explains_compiler_errors_with_locations() {
        let report = explain_failure(CARGO_ERROR);
        assert_eq!(report.kind, "compile-error");
        assert!(report.first_error.unwrap().contains("mismatched types"));
        assert!(report.locations.contains(&"src/main.rs:42:17".to_string()));
        assert!(!report.suggestions.is_empty());
    }

    #[test]
    fn explains_test_failures() {
        let output = "running 3 tests\ntest tests::works ... ok\ntest tests::broken ... FAILED\n\nfailures:\n    tests::broken\n";
        let report = explain_failure(output);
        assert_eq!(report.kind, "test-failure");
        assert_eq!(report.test_failures, vec!["tests::broken".to_string()]);
    }

    #[test]
    fn explains_go_test_failures() {
        let report = explain_failure("--- FAIL: TestLogin (0.00s)\n    auth_test.go:12: got 401");
        assert_eq!(report.kind, "test-failure");
        assert_eq!(report.test_failures, vec!["TestLogin".to_string()]);
        assert!(report.locations.contains(&"auth_test.go:12".to_string()));
    }

    #[test]
    fn explains_python_missing_module() {
        let report = explain_failure("ModuleNotFoundError: No module named 'requests'");
        assert_eq!(report.kind, "missing-dependency");
    }

    #[test]
    fn unknown_output_still_returns_something_useful() {
        let report = explain_failure("just some noise");
        assert_eq!(report.kind, "unknown");
        assert!(!report.highlights.is_empty());
    }

    #[test]
    fn empty_output_is_reported_plainly() {
        let report = explain_failure("");
        assert_eq!(report.kind, "unknown");
        assert!(report.summary.contains("no recognized error pattern"));
    }
}
