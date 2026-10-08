//! Deterministic, offline providers.
//!
//! `LocalModel` is a heuristic planner: it retrieves repository context through
//! the same tools a real model would use, then answers from that context. It is
//! honest about what it is — it ranks and quotes, it does not reason.
//!
//! `ScriptedModel` replays a fixed response list so the whole agent loop, policy
//! engine, patching, and sandbox can be exercised deterministically.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::json;

use super::{Message, ModelClient, ModelRequest, ModelResponse, Role, ToolCall};
use crate::error::{RaiError, Result};
use crate::patch::Patch;
use crate::tools::Mode;

/// Words that carry no retrieval signal in a repository query.
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "what",
    "where",
    "when",
    "which",
    "how",
    "why",
    "does",
    "did",
    "was",
    "are",
    "our",
    "you",
    "your",
    "into",
    "than",
    "then",
    "them",
    "they",
    "there",
    "here",
    "have",
    "has",
    "had",
    "not",
    "but",
    "can",
    "could",
    "should",
    "would",
    "about",
    "please",
    "explain",
    "show",
    "tell",
    "give",
    "make",
    "made",
    "add",
    "added",
    "using",
    "use",
    "used",
    "does",
    "code",
    "file",
    "files",
    "function",
    "functions",
    "work",
    "works",
];

/// An offline planner that uses the tool surface instead of a model.
pub struct LocalModel {
    mode: Mode,
    step: AtomicUsize,
}

impl LocalModel {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            step: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ModelClient for LocalModel {
    fn provider(&self) -> &'static str {
        "local"
    }

    fn model(&self) -> &str {
        "local-heuristic"
    }

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse> {
        let step = self.step.fetch_add(1, Ordering::SeqCst);
        let task = last_user_text(&request);
        let tool_results: Vec<&Message> = request
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .collect();

        let response = if tool_results.is_empty() && step == 0 {
            self.plan(&task)
        } else {
            self.answer(&task, &tool_results)
        };

        Ok(ModelResponse {
            text: response.0,
            tool_calls: response.1,
            usage: None,
            provider: "local".to_string(),
            model: "local-heuristic".to_string(),
        })
    }
}

impl LocalModel {
    /// First turn: ask for the context this mode needs.
    fn plan(&self, task: &str) -> (String, Vec<ToolCall>) {
        let keywords = keywords(task);
        let mut calls: Vec<ToolCall> = Vec::new();

        match self.mode {
            Mode::Edit => {
                let paths = path_tokens(task);
                for (index, path) in paths.iter().take(3).enumerate() {
                    calls.push(ToolCall::new(
                        format!("local-read-{index}"),
                        "read_file",
                        json!({ "path": path }),
                    ));
                }
                if calls.is_empty() {
                    calls.push(ToolCall::new(
                        "local-search",
                        "search_text",
                        json!({ "query": search_query(&keywords), "max_results": 20 }),
                    ));
                }
            }
            Mode::Review => {
                calls.push(ToolCall::new("local-status", "git_status", json!({})));
                calls.push(ToolCall::new("local-diff", "summarize_diff", json!({})));
            }
            Mode::Ask | Mode::Run => {
                calls.push(ToolCall::new(
                    "local-search",
                    "search_text",
                    json!({ "query": search_query(&keywords), "max_results": 30 }),
                ));
                calls.push(ToolCall::new(
                    "local-files",
                    "list_files",
                    json!({ "limit": 60 }),
                ));
            }
        }

        (
            format!(
                "Offline planner: gathering context for `{}`.",
                crate::util::truncate_line(task, 80)
            ),
            calls,
        )
    }

    /// Later turns: answer from the tool results.
    fn answer(&self, task: &str, tool_results: &[&Message]) -> (String, Vec<ToolCall>) {
        match self.mode {
            Mode::Ask | Mode::Run => (summarize_ask(task, tool_results), Vec::new()),
            Mode::Review => (summarize_review(tool_results), Vec::new()),
            Mode::Edit => edit_from_task(task, tool_results),
        }
    }
}

/// Extract the last user message text.
fn last_user_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.text.clone())
        .unwrap_or_default()
}

/// Rank query keywords: identifiers first, then longer words.
pub fn keywords(task: &str) -> Vec<String> {
    let mut words: Vec<String> = task
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .filter(|w| w.len() >= 3)
        .map(str::to_string)
        .filter(|w| {
            let lower = w.to_ascii_lowercase();
            !STOPWORDS.contains(&lower.as_str())
        })
        .collect();
    words.sort();
    words.dedup();
    words.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    words.truncate(3);
    words
}

/// Tokens that look like repository paths.
pub fn path_tokens(task: &str) -> Vec<String> {
    let mut paths: Vec<String> = task
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| {
                !(c.is_alphanumeric() || c == '/' || c == '.' || c == '_' || c == '-')
            })
        })
        .filter(|token| {
            let looks_like_path = token.contains('/')
                || [
                    ".rs", ".toml", ".json", ".md", ".js", ".ts", ".py", ".go", ".java", ".cs",
                    ".yaml", ".yml", ".txt",
                ]
                .iter()
                .any(|ext| token.ends_with(ext));
            looks_like_path && !token.starts_with("http")
        })
        .map(str::to_string)
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// Regex alternation of keywords, or a permissive fallback.
pub fn alternation(keywords: &[String]) -> String {
    if keywords.is_empty() {
        // No usable keywords: the caller substitutes a language-shaped query.
        return String::new();
    }
    keywords
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|")
}

/// Search query for the planner: keywords, or a language-shaped fallback.
pub fn search_query(keywords: &[String]) -> String {
    let query = alternation(keywords);
    if query.is_empty() {
        "fn|def|class|function".to_string()
    } else {
        query
    }
}

/// Build the offline answer for `ask`.
fn summarize_ask(task: &str, tool_results: &[&Message]) -> String {
    let mut out = String::new();
    out.push_str("Answer from the local heuristic provider (no LLM configured).\n");
    out.push_str("It retrieved and ranked repository matches; it does not reason about them.\n\n");
    out.push_str(&format!(
        "query: {}\n",
        crate::util::truncate_line(task, 120)
    ));

    for result in tool_results {
        let text = result.text.trim_end();
        if text.is_empty() {
            continue;
        }
        let label = if text.contains("engine:") {
            "search"
        } else if text.contains("file(s) matched") {
            "files"
        } else {
            "context"
        };
        out.push_str(&format!("\n--- {label} ---\n"));
        let (bounded, truncated) = crate::util::truncate_bytes(text, 4000);
        out.push_str(&bounded);
        if truncated {
            out.push_str("\n[truncated]");
        }
        out.push('\n');
    }

    out.push_str(
        "\nSet [model].provider = \"openai\" with a real endpoint for synthesized answers.\n",
    );
    out
}

/// Build the offline answer for `review` using the deterministic risk scan.
fn summarize_review(tool_results: &[&Message]) -> String {
    let mut out = String::new();
    out.push_str("Review from the local deterministic scan (no LLM configured).\n\n");
    for result in tool_results {
        let text = result.text.trim_end();
        if text.is_empty() {
            continue;
        }
        let (bounded, _) = crate::util::truncate_bytes(text, 6000);
        out.push_str(&bounded);
        out.push_str("\n\n");
    }
    out.push_str(
        "Set [model].provider = \"openai\" to add narrative review on top of these facts.\n",
    );
    out
}

/// Turn an explicit edit directive into an `apply_patch` call.
///
/// The local provider cannot invent code, so it accepts three explicit forms:
///
/// 1. an embedded unified diff,
/// 2. `replace in <path>: "<old>" -> "<new>"`,
/// 3. `create file <path>: <<<content>>>`.
///
/// Anything else is refused with an explanation instead of a guess.
fn edit_from_task(task: &str, tool_results: &[&Message]) -> (String, Vec<ToolCall>) {
    // If the runtime already refused this work, explain instead of re-asking.
    if let Some(reason) = last_refusal(tool_results) {
        return (
            format!(
                "Offline planner: the runtime refused the change, so nothing was written.\n\
                 {reason}\n\
                 Re-run with --yes, or add the risk class to [policy].auto_approve, then repeat the request."
            ),
            Vec::new(),
        );
    }

    // The write already happened; re-applying the same diff would fail.
    if let Some(summary) = applied_result(tool_results) {
        return (
            format!("Offline planner: the patch was already applied.\n{summary}"),
            Vec::new(),
        );
    }

    if let Some(diff) = Patch::extract(task) {
        if Patch::parse(&diff).is_ok() {
            return (
                "Offline planner: applying the unified diff from the request.".to_string(),
                vec![ToolCall::new(
                    "local-patch",
                    "apply_patch",
                    json!({ "patch": diff }),
                )],
            );
        }
    }

    if let Some((path, content)) = parse_create_directive(task) {
        let diff = build_create_diff(&path, &content);
        return (
            format!("Offline planner: creating {path}."),
            vec![ToolCall::new(
                "local-create",
                "apply_patch",
                json!({ "patch": diff }),
            )],
        );
    }

    if let Some((path, old, new)) = parse_replace_directive(task) {
        let Some(file) = read_result_for(tool_results, &path) else {
            return (
                format!(
                    "Offline planner: `{path}` was not read, so the replacement cannot be located.\n\
                     Name the file with its extension so it can be resolved first."
                ),
                Vec::new(),
            );
        };
        return match build_replace_diff(&path, &file, &old, &new) {
            Ok(diff) => (
                format!("Offline planner: replacing {old:?} with {new:?} in {path}."),
                vec![ToolCall::new(
                    "local-replace",
                    "apply_patch",
                    json!({ "patch": diff }),
                )],
            ),
            Err(reason) => (
                format!("Offline planner could not build the patch: {reason}"),
                Vec::new(),
            ),
        };
    }

    let mut out = String::new();
    out.push_str("Offline planner: this provider cannot synthesize code changes.\n\n");
    out.push_str("Configure a real model provider to edit by description:\n");
    out.push_str(
        "  [model]\n  provider = \"openai\"\n  base_url = \"https://api.openai.com/v1\"\n",
    );
    out.push_str("  api_key_env = \"OPENAI_API_KEY\"\n\n");
    out.push_str("Or state the change explicitly in one of these forms:\n");
    out.push_str("  1. include a unified diff (a ```diff block) in the task\n");
    out.push_str("  2. replace in src/lib.rs: \"old text\" -> \"new text\"\n");
    out.push_str("  3. create file notes/todo.md: <<<first line\nsecond line>>>\n");
    if !tool_results.is_empty() {
        out.push_str("\nContext that was gathered:\n");
        for result in tool_results.iter().take(3) {
            let (bounded, _) = crate::util::truncate_bytes(result.text.trim_end(), 1200);
            out.push_str(&bounded);
            out.push('\n');
        }
    }
    (out, Vec::new())
}

/// `replace in <path>: "<old>" -> "<new>"`.
fn parse_replace_directive(task: &str) -> Option<(String, String, String)> {
    let re = regex::Regex::new(
        r#"(?is)replace\s+(?:in\s+)?([A-Za-z0-9_./\\-]+\.[A-Za-z0-9_]+)\s*:\s*"((?:[^"\\]|\\.)*)"\s*(?:->|=>|with)\s*"((?:[^"\\]|\\.)*)""#,
    )
    .ok()?;
    let captures = re.captures(task)?;
    Some((
        captures.get(1)?.as_str().trim().to_string(),
        unescape(captures.get(2)?.as_str()),
        unescape(captures.get(3)?.as_str()),
    ))
}

/// `create file <path>: <<<content>>>`.
fn parse_create_directive(task: &str) -> Option<(String, String)> {
    let re = regex::Regex::new(
        r#"(?is)create\s+(?:file\s+)?([A-Za-z0-9_./\\-]+\.[A-Za-z0-9_]+)\s*:\s*<<<(.*?)>>>"#,
    )
    .ok()?;
    let captures = re.captures(task)?;
    Some((
        captures.get(1)?.as_str().trim().to_string(),
        captures.get(2)?.as_str().trim_matches('\n').to_string(),
    ))
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Recover file content from a `read_file` tool result.
fn read_result_for(tool_results: &[&Message], path: &str) -> Option<String> {
    let wanted = path.replace('\\', "/");
    let wanted = wanted.trim_start_matches("./");
    for result in tool_results {
        let Some(rest) = result.text.strip_prefix("path: ") else {
            continue;
        };
        let (header, body) = match rest.split_once("\n---\n") {
            Some(parts) => parts,
            None => continue,
        };
        let actual = header.lines().next().unwrap_or("").trim();
        if actual == wanted || actual.ends_with(&format!("/{wanted}")) {
            return Some(strip_line_numbers(body));
        }
    }
    None
}

/// A successful `apply_patch` result, if the write already happened.
///
/// The prefix matches `tools::patch_tool`'s outcome text; both sides agree on
/// it so the planner can tell "already applied" from "not yet attempted".
fn applied_result(tool_results: &[&Message]) -> Option<String> {
    tool_results
        .iter()
        .map(|message| message.text.trim_start())
        .find(|text| {
            text.starts_with("applied patch(es)") || text.starts_with("validated patch(es)")
        })
        .map(|text| text.lines().take(4).collect::<Vec<_>>().join("\n"))
}

/// The runtime's refusal text from the most recent tool results, if any.
fn last_refusal(tool_results: &[&Message]) -> Option<String> {
    tool_results
        .iter()
        .rev()
        .find(|message| crate::agent::is_refusal(&message.text))
        .map(|message| message.text.clone())
}

/// Recover raw file text from a `read_file` result body.
///
/// `read_file` renders `   12 | content` so a model can talk about line
/// numbers; a diff must not contain that gutter, so it is removed here.
fn strip_line_numbers(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.lines() {
        let content = match line.split_once(" | ") {
            Some((prefix, rest))
                if !prefix.is_empty() && prefix.trim().chars().all(|c| c.is_ascii_digit()) =>
            {
                rest
            }
            _ => line,
        };
        out.push_str(content);
        out.push('\n');
    }
    out
}

/// Build a creation diff for a new file.
fn build_create_diff(path: &str, content: &str) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut diff = format!(
        "--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in &lines {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

/// Build a minimal replacement diff around the first occurrence of `old`.
///
/// Offsets do not need to be exact: the patch applier anchors on content, not
/// on line numbers.
fn build_replace_diff(
    path: &str,
    content: &str,
    old: &str,
    new: &str,
) -> std::result::Result<String, String> {
    if old.is_empty() {
        return Err("the text to replace is empty".to_string());
    }
    if old == new {
        return Err("the replacement is identical to the original".to_string());
    }
    if !content.contains(old) {
        return Err(format!("the text to replace was not found in {path}"));
    }

    // Work on whole lines so the hunk is valid even when `old` covers only part
    // of a line: replace, then keep the changed line window.
    let updated = content.replacen(old, new, 1);
    let before: Vec<&str> = content.lines().collect();
    let after: Vec<&str> = updated.lines().collect();

    let mut prefix = 0usize;
    while prefix < before.len() && prefix < after.len() && before[prefix] == after[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < before.len().saturating_sub(prefix)
        && suffix < after.len().saturating_sub(prefix)
        && before[before.len() - 1 - suffix] == after[after.len() - 1 - suffix]
    {
        suffix += 1;
    }

    const CONTEXT: usize = 3;
    let window = prefix.saturating_sub(CONTEXT);
    let tail = CONTEXT.min(suffix);

    let head = &before[window..prefix];
    let old_changed = &before[prefix..before.len() - suffix];
    let new_changed = &after[prefix..after.len() - suffix];
    let tail_lines = &before[before.len() - suffix..before.len() - suffix + tail];

    let mut diff = format!("--- a/{path}\n+++ b/{path}\n");
    diff.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        window + 1,
        head.len() + old_changed.len() + tail_lines.len(),
        window + 1,
        head.len() + new_changed.len() + tail_lines.len(),
    ));
    for line in head {
        diff.push(' ');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in old_changed {
        diff.push('-');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in new_changed {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    for line in tail_lines {
        diff.push(' ');
        diff.push_str(line);
        diff.push('\n');
    }
    Ok(diff)
}

/// One scripted model response.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ScriptedResponse {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ScriptedCall>,
}

/// One scripted tool call.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ScriptedCall {
    pub name: String,
    #[serde(default = "empty_object")]
    pub arguments: serde_json::Value,
}

fn empty_object() -> serde_json::Value {
    serde_json::json!({})
}

/// Replays a fixed list of responses.
///
/// This is a test and demo provider: it makes the whole loop (policy, approvals,
/// tool execution, patching, budgets, session log) reproducible without a
/// network or a model.
#[derive(Debug)]
pub struct ScriptedModel {
    responses: Vec<ScriptedResponse>,
    source: PathBuf,
    step: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ScriptFile {
    Wrapped { responses: Vec<ScriptedResponse> },
    Plain(Vec<ScriptedResponse>),
}

impl ScriptedModel {
    /// Load a script from disk.
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| RaiError::Config(format!("cannot read script {}: {e}", path.display())))?;
        Self::from_json(&text, path)
    }

    /// Parse a script from text.
    pub fn from_json(text: &str, source: &std::path::Path) -> Result<Self> {
        let file: ScriptFile = serde_json::from_str(text).map_err(|e| {
            RaiError::Config(format!("cannot parse script {}: {e}", source.display()))
        })?;
        let responses = match file {
            ScriptFile::Wrapped { responses } => responses,
            ScriptFile::Plain(responses) => responses,
        };
        if responses.is_empty() {
            return Err(RaiError::Config(format!(
                "script {} contains no responses",
                source.display()
            )));
        }
        Ok(Self {
            responses,
            source: source.to_path_buf(),
            step: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Number of responses consumed so far.
    pub fn steps(&self) -> usize {
        self.step.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelClient for ScriptedModel {
    fn provider(&self) -> &'static str {
        "scripted"
    }

    fn model(&self) -> &str {
        "scripted"
    }

    async fn respond(&self, _request: ModelRequest) -> Result<ModelResponse> {
        let index = self.step.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let Some(scripted) = self.responses.get(index) else {
            return Err(RaiError::Model(format!(
                "script {} ran out of responses after {} turn(s)",
                self.source.display(),
                index
            )));
        };
        let calls = scripted
            .tool_calls
            .iter()
            .enumerate()
            .map(|(offset, call)| {
                ToolCall::new(
                    format!("script-{index}-{offset}"),
                    call.name.clone(),
                    call.arguments.clone(),
                )
            })
            .collect();
        Ok(ModelResponse {
            text: scripted.text.clone(),
            tool_calls: calls,
            usage: None,
            provider: "scripted".to_string(),
            model: "scripted".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Patch;

    #[test]
    fn keywords_skip_stopwords_and_short_tokens() {
        let words = keywords("how does the policy engine evaluate a tool call in src/policy.rs");
        assert!(words
            .iter()
            .any(|w| w == "policy" || w == "engine" || w == "evaluate"));
        assert!(!words.iter().any(|w| w == "the" || w == "a" || w == "in"));
    }

    #[test]
    fn alternation_builds_a_regex() {
        let query = alternation(&["alpha".into(), "beta".into()]);
        assert_eq!(query, "alpha|beta");
        assert_eq!(alternation(&[]), "");
    }

    #[test]
    fn parses_replace_directive() {
        let (path, old, new) =
            parse_replace_directive(r#"replace in src/lib.rs: "let x = 1;" -> "let x = 2;""#)
                .unwrap();
        assert_eq!(path, "src/lib.rs");
        assert_eq!(old, "let x = 1;");
        assert_eq!(new, "let x = 2;");
    }

    #[test]
    fn parses_create_directive() {
        let (path, content) =
            parse_create_directive("create file notes/todo.md: <<<line one\nline two>>>").unwrap();
        assert_eq!(path, "notes/todo.md");
        assert_eq!(content, "line one\nline two");
    }

    #[test]
    fn replace_diff_is_a_valid_patch() {
        let content = "fn main() {\n    let x = 1;\n    println!(\"{}\", x);\n}\n";
        let diff = build_replace_diff("src/main.rs", content, "let x = 1;", "let x = 2;").unwrap();
        let patch = Patch::parse(&diff).expect("the generated diff parses");
        assert_eq!(patch.files.len(), 1);
        assert_eq!(patch.added(), 1);
        assert_eq!(patch.removed(), 1);
        assert!(diff.contains("-    let x = 1;"));
        assert!(diff.contains("+    let x = 2;"));
    }

    #[test]
    fn replace_diff_rejects_missing_text() {
        let error = build_replace_diff("a.rs", "hello\n", "goodbye", "hi").unwrap_err();
        assert!(error.contains("not found"));
    }

    #[test]
    fn create_diff_is_a_valid_patch() {
        let diff = build_create_diff("notes/todo.md", "first\nsecond");
        let patch = Patch::parse(&diff).expect("the generated diff parses");
        assert!(patch.files[0].is_create());
        assert_eq!(patch.added(), 2);
    }

    #[test]
    fn recovers_file_content_from_read_results() {
        let message = Message::tool_result(
            "c1",
            "path: src/lib.rs\nlines: 1-2 of 2\n---\nline one\nline two\n",
        );
        let content = read_result_for(&[&message], "src/lib.rs").unwrap();
        assert!(content.contains("line one"));
        assert!(read_result_for(&[&message], "other.rs").is_none());
    }

    #[tokio::test]
    async fn scripted_model_replays_in_order() {
        let script = r#"{"responses": [
            {"text": "", "tool_calls": [{"name": "read_file", "arguments": {"path": "a.rs"}}]},
            {"text": "done"}
        ]}"#;
        let model = ScriptedModel::from_json(script, std::path::Path::new("<test>")).unwrap();
        let request = ModelRequest::new(vec![Message::user("go")]);

        let first = model.respond(request.clone()).await.unwrap();
        assert_eq!(first.tool_calls.len(), 1);
        assert_eq!(first.tool_calls[0].name, "read_file");
        assert_eq!(first.tool_calls[0].arguments["path"], "a.rs");

        let second = model.respond(request.clone()).await.unwrap();
        assert!(second.is_final());
        assert_eq!(second.text, "done");

        let error = model.respond(request).await.unwrap_err();
        assert!(error.to_string().contains("ran out of responses"));
    }

    #[test]
    fn script_accepts_a_bare_array() {
        let model =
            ScriptedModel::from_json(r#"[{"text": "hi"}]"#, std::path::Path::new("<t>")).unwrap();
        assert_eq!(model.steps(), 0);
    }

    #[test]
    fn empty_script_is_rejected() {
        let error = ScriptedModel::from_json(r#"{"responses": []}"#, std::path::Path::new("<t>"))
            .unwrap_err();
        assert!(error.to_string().contains("no responses"));
    }

    /// A generated diff must actually apply, not merely parse.
    #[test]
    fn replace_diff_applies_to_the_original_content() {
        use crate::patch::{apply, ApplyOptions, Patch};

        let content = "pub struct Engine {\n    pub retries: u32,\n}\n\nimpl Engine {\n    pub fn new() -> Self {\n        let limit = 3;\n        Self { retries: limit }\n    }\n}\n\npub fn describe(engine: &Engine) -> String {\n    format!(\"retries={}\", engine.retries)\n}\n";
        let diff = build_replace_diff("src/lib.rs", content, "let limit = 3;", "let limit = 5;")
            .expect("diff builds");
        eprintln!("--- generated diff ---\n{diff}--- end ---");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), content).unwrap();

        let patch = Patch::parse(&diff).expect("the generated diff parses");
        let options = ApplyOptions {
            root: dir.path().to_path_buf(),
            allow_writes: true,
            max_file_bytes: 1_000_000,
            dry_run: false,
        };
        let applied = apply(&patch, &options).expect("the generated diff applies");
        assert_eq!(applied[0].added, 1);
        assert_eq!(applied[0].removed, 1);

        let updated = std::fs::read_to_string(dir.path().join("src/lib.rs")).unwrap();
        assert!(updated.contains("let limit = 5;"));
        assert!(!updated.contains("let limit = 3;"));
        assert_eq!(updated.lines().count(), content.lines().count());
    }

    /// The planner reads files through `read_file`, which numbers lines.
    #[test]
    fn read_results_are_unnumbered_before_diffing() {
        let body = "path: src/lib.rs\nlines: 1-2 of 2\n---\n    1 | a\n    2 | b\n";
        let message = Message::tool_result("c1", body);
        let content = read_result_for(&[&message], "src/lib.rs").expect("found");
        assert_eq!(content, "a\nb\n");
    }

    #[test]
    fn a_plain_read_result_is_left_alone() {
        let body = "path: src/lib.rs\nlines: 1-1 of 1\n---\nplain text\n";
        let message = Message::tool_result("c1", body);
        let content = read_result_for(&[&message], "src/lib.rs").expect("found");
        assert_eq!(content, "plain text\n");
    }
}
