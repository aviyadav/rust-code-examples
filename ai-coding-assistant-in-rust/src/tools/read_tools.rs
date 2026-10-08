//! Read-only repository tools.
//!
//! These are how the model asks for context instead of receiving a dump of the
//! repository. Every result is bounded, workspace-confined, and observable in
//! the trace.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    object_schema, optional_str, optional_usize, require_str, Tool, ToolContext, ToolDefinition,
    ToolOutcome,
};
use crate::error::{RaiError, Result};
use crate::index;
use crate::util::{relative_to, truncate_line};

/// List workspace files from the local index.
pub struct ListFiles;

#[async_trait]
impl Tool for ListFiles {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "list_files",
            "List workspace files known to the local index. Use this to discover layout before reading files.",
            object_schema(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Only list files under this workspace-relative directory."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Simple pattern matched against the path, e.g. `*.rs` or `src/*.toml`."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of paths to return (default 100)."
                    }
                }),
                &[],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let prefix = optional_str(&args, "path");
        let pattern = optional_str(&args, "glob");
        let limit = optional_usize(&args, "limit").unwrap_or(100).clamp(1, 1000);

        let mut paths: Vec<&crate::index::IndexEntry> = ctx
            .index
            .entries
            .iter()
            .filter(|entry| match &prefix {
                Some(p) => entry.path.starts_with(p.trim_end_matches('/')),
                None => true,
            })
            .filter(|entry| match &pattern {
                Some(p) => glob_match(p, &entry.path),
                None => true,
            })
            .collect();
        paths.sort_by(|a, b| a.path.cmp(&b.path));

        let total = paths.len();
        let shown: Vec<&crate::index::IndexEntry> = paths.drain(..).take(limit).collect();

        let mut text = String::new();
        text.push_str(&format!(
            "{} file(s) matched, showing {}\n",
            total,
            shown.len()
        ));
        for entry in &shown {
            text.push_str(&format!(
                "{}\t{}\t{} bytes\n",
                entry.path, entry.lang, entry.bytes
            ));
        }

        Ok(ToolOutcome::new(format!("listed {total} path(s)"), text)
            .with_data(json!({ "total": total, "shown": shown.len() })))
    }
}

/// Read a text file inside the workspace, optionally sliced to a line range.
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "read_file",
            "Read a UTF-8 text file inside the workspace. Prefer a line range for large files.",
            object_schema(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative path, e.g. `src/main.rs`."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "First line to return (1-based)."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Last line to return (1-based, inclusive)."
                    }
                }),
                &["path"],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let raw = require_str(&args, "path", "read_file")?;
        let path = ctx.resolve(&raw)?;
        if path.is_dir() {
            return Err(RaiError::InvalidArguments {
                tool: "read_file".into(),
                problem: format!("`{raw}` is a directory; use list_files"),
            });
        }

        let start = optional_usize(&args, "start_line");
        let end = optional_usize(&args, "end_line");
        let full = index::read_text_file(&path, ctx.config.workspace.max_file_bytes)?;
        let relative = relative_to(&ctx.root, &path);
        let total_lines = full.lines().count();
        let (body, from, to) = index::slice_lines(&full, start, end);

        let text = format!("path: {relative}\nlines: {from}-{to} of {total_lines}\n---\n{body}");
        Ok(
            ToolOutcome::new(format!("read {relative} lines {from}-{to}"), text).with_data(json!({
                "path": relative,
                "from": from,
                "to": to,
                "total_lines": total_lines,
            })),
        )
    }
}

/// Match a path against a simple `*`/`?` pattern.
///
/// Deliberately small: a full glob engine is unnecessary for tool arguments,
/// and this keeps match behaviour obvious.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return true;
    }
    // A bare pattern with no separator matches the file name as well as the
    // whole path, so `*.rs` behaves the way people expect.
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        if wildcard(pattern.as_bytes(), name.as_bytes()) {
            return true;
        }
    }
    wildcard(pattern.as_bytes(), path.as_bytes())
}

fn wildcard(pattern: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Bound a tool result so one call cannot flood the context window.
pub fn bounded(text: String, max_bytes: usize) -> (String, bool) {
    let (mut text, truncated) = crate::util::truncate_bytes(&text, max_bytes);
    if truncated {
        text.push_str("\n[output truncated by the rai runtime]");
    }
    (text, truncated)
}

/// Render a `path:line: text` line for search and grep style output.
pub fn hit_line(path: &str, line: usize, text: &str) -> String {
    format!("{path}:{line}: {}", truncate_line(text.trim_end(), 300))
}

/// Search repository text with ripgrep when available.
pub struct SearchText;

#[async_trait]
impl Tool for SearchText {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "search_text",
            "Search workspace text. Returns `path:line: text` matches. Use this before reading files you have not located yet.",
            object_schema(
                json!({
                    "query": {
                        "type": "string",
                        "description": "Regular expression to search for."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Restrict to paths matching this pattern, e.g. `src/*.rs`."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum matches to return (default from config, hard cap 500)."
                    }
                }),
                &["query"],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let query = require_str(&args, "query", "search_text")?;
        let glob = optional_str(&args, "glob");
        let limit = optional_usize(&args, "max_results")
            .unwrap_or(ctx.config.search.max_results)
            .clamp(1, 500);

        let outcome = index::search_text(
            &ctx.root,
            &query,
            glob.as_deref(),
            limit,
            ctx.config.search.prefer_ripgrep,
        )
        .await;

        let mut text = format!(
            "engine: {}\nmatch_count: {}\n",
            outcome.engine,
            outcome.hits.len()
        );
        if let Some(error) = &outcome.error {
            text.push_str(&format!("search_error: {error}\n"));
        }
        text.push_str("---\n");
        for hit in &outcome.hits {
            text.push_str(&hit_line(&hit.path, hit.line, &hit.text));
            text.push('\n');
        }
        if outcome.truncated {
            text.push_str("[more matches exist; narrow the query or glob]\n");
        }

        let (text, truncated) = bounded(text, 32 * 1024);
        Ok(ToolOutcome::new(
            format!("{} match(es) for `{query}`", outcome.hits.len()),
            text,
        )
        .with_data(json!({
            "engine": outcome.engine,
            "matches": outcome.hits.len(),
            "truncated": outcome.truncated,
        }))
        .truncated_if(truncated || outcome.truncated))
    }
}

/// List symbol-like declarations from the local index.
pub struct ListSymbols;

#[async_trait]
impl Tool for ListSymbols {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "list_symbols",
            "List declarations (functions, types, classes) either for one file or matching a name across the workspace.",
            object_schema(
                json!({
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative file to inspect."
                    },
                    "query": {
                        "type": "string",
                        "description": "Substring to match against symbol names across the workspace."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum symbols to return (default 100)."
                    }
                }),
                &[],
            ),
            super::RiskClass::ReadOnly,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let limit = optional_usize(&args, "limit").unwrap_or(100).clamp(1, 1000);
        let path = optional_str(&args, "path");
        let query = optional_str(&args, "query");

        if path.is_none() && query.is_none() {
            return Err(RaiError::InvalidArguments {
                tool: "list_symbols".into(),
                problem: "provide `path` (one file) or `query` (a name pattern)".into(),
            });
        }

        let mut lines: Vec<String> = Vec::new();
        if let Some(raw) = &path {
            let relative = relative_to(&ctx.root, &ctx.resolve(raw)?);
            let entry =
                ctx.index
                    .entry_for(&relative)
                    .ok_or_else(|| RaiError::InvalidArguments {
                        tool: "list_symbols".into(),
                        problem: format!("`{relative}` is not in the index"),
                    })?;
            for symbol in entry.symbols.iter().take(limit) {
                lines.push(format!(
                    "{}:{}: {} {}",
                    entry.path, symbol.line, symbol.kind, symbol.name
                ));
            }
            if lines.is_empty() {
                lines.push(format!("{}: no symbols detected", entry.path));
            }
        } else if let Some(query) = &query {
            for (path, symbol) in ctx.index.find_symbols(query, limit) {
                lines.push(format!(
                    "{path}:{}: {} {}",
                    symbol.line, symbol.kind, symbol.name
                ));
            }
            if lines.is_empty() {
                lines.push(format!("no symbols matched `{query}`"));
            }
        }

        let text = lines.join("\n");
        Ok(
            ToolOutcome::new(format!("{} symbol(s)", lines.len()), text).with_data(json!({
                "count": lines.len(),
                "path": path,
                "query": query,
            })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_file_names_and_paths() {
        assert!(glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "tests/main.rs"));
        assert!(glob_match("**", "anything/at/all.txt"));
        assert!(glob_match("main.?s", "src/main.rs"));
        assert!(!glob_match("*.py", "src/main.rs"));
        assert!(glob_match("", "anything"));
    }

    #[test]
    fn bounded_marks_truncation() {
        let (text, truncated) = bounded("x".repeat(100), 10);
        assert!(truncated);
        assert!(text.contains("truncated"));
    }

    #[test]
    fn bounded_passes_short_text_through() {
        let (text, truncated) = bounded("short".into(), 100);
        assert!(!truncated);
        assert_eq!(text, "short");
    }

    #[test]
    fn hit_line_is_single_line_and_bounded() {
        let line = hit_line("a.rs", 12, &format!("{}\nmore", "z".repeat(400)));
        assert!(!line.contains('\n'));
        assert!(line.len() < 340);
        assert!(line.starts_with("a.rs:12:"));
    }
}
