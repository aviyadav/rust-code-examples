//! Repository context: indexed, not dumped.
//!
//! The assistant never sends the whole repository to a model. It keeps a
//! lightweight local index (paths, languages, symbols, size, mtime, git state)
//! and lets the model ask for context through tools. Index building and
//! searching are CPU-heavy, so they run on a blocking pool rather than on the
//! async runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::error::{RaiError, Result};
use crate::util::{detect_language, now_rfc3339, relative_to, slash};

/// Directories that never belong in a context index.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".rai",
    "target",
    "node_modules",
    "dist",
    "build",
    "out",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".gradle",
    ".terraform",
    "vendor",
];

/// A symbol-like declaration found by text extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
}

/// One indexed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Workspace-relative path with forward slashes.
    pub path: String,
    pub lang: String,
    pub bytes: u64,
    pub lines: usize,
    /// Modification time in unix seconds.
    pub modified: u64,
    pub symbols: Vec<Symbol>,
}

/// How the index was built.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexOptions {
    pub excludes: Vec<String>,
    pub max_files: usize,
    /// Files larger than this are listed but not scanned for symbols.
    pub max_scan_bytes: u64,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            excludes: DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            max_files: 20_000,
            max_scan_bytes: 512 * 1024,
        }
    }
}

/// A repository index snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoIndex {
    pub root: String,
    pub built_at: String,
    pub entries: Vec<IndexEntry>,
    /// True when the walk stopped early at `max_files`.
    pub truncated: bool,
    /// Number of files skipped because they were binary or undecodable.
    pub skipped_binary: usize,
}

impl RepoIndex {
    /// Build an index by walking the workspace.
    pub fn build(root: &Path, options: &IndexOptions) -> Result<Self> {
        let mut entries: Vec<IndexEntry> = Vec::new();
        let mut truncated = false;
        let mut skipped_binary = 0usize;

        let excludes: Vec<String> = options.excludes.clone();
        let walker = ignore::WalkBuilder::new(root)
            .hidden(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .require_git(false)
            .parents(true)
            .filter_entry(move |entry| {
                if entry.depth() == 0 {
                    return true;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.file_type().is_some_and(|t| t.is_dir()) {
                    return !excludes.contains(&name);
                }
                true
            })
            .build();

        for result in walker {
            let Ok(entry) = result else { continue };
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            if entries.len() >= options.max_files {
                truncated = true;
                break;
            }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let bytes = meta.len();
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let lang = detect_language(path).unwrap_or_else(|| "text".to_string());

            let mut symbols = Vec::new();
            let mut lines = 0usize;
            if bytes <= options.max_scan_bytes {
                if let Ok(raw) = std::fs::read(path) {
                    if crate::util::is_probably_binary(&raw) {
                        skipped_binary += 1;
                    } else if let Ok(text) = String::from_utf8(raw) {
                        lines = text.lines().count();
                        symbols = extract_symbols(&lang, &text);
                    }
                }
            }

            entries.push(IndexEntry {
                path: relative_to(root, path),
                lang,
                bytes,
                lines,
                modified,
                symbols,
            });
        }

        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            root: slash(root),
            built_at: now_rfc3339(),
            entries,
            truncated,
            skipped_binary,
        })
    }

    /// Build the index off the async runtime.
    pub async fn build_async(root: PathBuf, options: IndexOptions) -> Result<Self> {
        tokio::task::spawn_blocking(move || RepoIndex::build(&root, &options))
            .await
            .map_err(|e| RaiError::Io(std::io::Error::other(e.to_string())))?
    }

    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    pub fn symbol_count(&self) -> usize {
        self.entries.iter().map(|e| e.symbols.len()).sum()
    }

    pub fn entry_for(&self, relative: &str) -> Option<&IndexEntry> {
        let normalized = relative.replace('\\', "/");
        self.entries.iter().find(|e| e.path == normalized)
    }

    /// Language histogram, most common first.
    pub fn languages(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for entry in &self.entries {
            *counts.entry(entry.lang.clone()).or_default() += 1;
        }
        let mut pairs: Vec<(String, usize)> = counts.into_iter().collect();
        pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        pairs
    }

    /// Symbols whose name contains `query` (case-insensitive).
    pub fn find_symbols(&self, query: &str, limit: usize) -> Vec<(String, Symbol)> {
        let needle = query.to_ascii_lowercase();
        let mut hits = Vec::new();
        for entry in &self.entries {
            for symbol in &entry.symbols {
                if symbol.name.to_ascii_lowercase().contains(&needle) {
                    hits.push((entry.path.clone(), symbol.clone()));
                    if hits.len() >= limit {
                        return hits;
                    }
                }
            }
        }
        hits
    }

    /// Files whose path contains any of the query tokens, best match first.
    pub fn find_files(&self, query: &str, limit: usize) -> Vec<String> {
        let tokens: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.')
            .filter(|t| t.len() >= 3)
            .map(|t| t.to_ascii_lowercase())
            .collect();
        if tokens.is_empty() {
            return self
                .entries
                .iter()
                .take(limit)
                .map(|e| e.path.clone())
                .collect();
        }
        let mut scored: Vec<(usize, &IndexEntry)> = Vec::new();
        for entry in &self.entries {
            let lower = entry.path.to_ascii_lowercase();
            let score: usize = tokens
                .iter()
                .map(|t| lower.matches(t.as_str()).count())
                .sum();
            if score > 0 {
                scored.push((score, entry));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, e)| e.path.clone())
            .collect()
    }

    /// Compact repository map for a model prompt.
    pub fn summary(&self, max_entries: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} files, {}, {} symbols\n",
            self.file_count(),
            crate::util::human_bytes(self.total_bytes()),
            self.symbol_count()
        ));
        let langs: Vec<String> = self
            .languages()
            .into_iter()
            .take(8)
            .map(|(lang, count)| format!("{lang}:{count}"))
            .collect();
        if !langs.is_empty() {
            out.push_str(&format!("languages: {}\n", langs.join(", ")));
        }
        out.push_str("files:\n");
        for entry in self.entries.iter().take(max_entries) {
            out.push_str(&format!("  {} ({} lines)\n", entry.path, entry.lines));
        }
        if self.entries.len() > max_entries {
            out.push_str(&format!(
                "  ... {} more\n",
                self.entries.len() - max_entries
            ));
        }
        out
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Result of a repository text search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    pub truncated: bool,
    pub engine: String,
    pub files_scanned: usize,
    pub error: Option<String>,
}

/// Search the workspace for `query`.
///
/// Prefers ripgrep when available (fast, ignore-aware) and falls back to an
/// in-process walk so the assistant works on machines without `rg`.
pub async fn search_text(
    root: &Path,
    query: &str,
    glob: Option<&str>,
    max_results: usize,
    prefer_ripgrep: bool,
) -> SearchOutcome {
    if query.trim().is_empty() {
        return SearchOutcome {
            hits: Vec::new(),
            truncated: false,
            engine: "none".into(),
            files_scanned: 0,
            error: Some("empty query".into()),
        };
    }

    if prefer_ripgrep {
        match ripgrep_search(root, query, glob, max_results).await {
            Ok(outcome) => return outcome,
            Err(err) => {
                let mut fallback = fallback_search(root, query, max_results).await;
                fallback.engine = format!("fallback (rg unavailable: {err})");
                return fallback;
            }
        }
    }

    fallback_search(root, query, max_results).await
}

async fn ripgrep_search(
    root: &Path,
    query: &str,
    glob: Option<&str>,
    max_results: usize,
) -> std::result::Result<SearchOutcome, String> {
    let mut args: Vec<String> = vec![
        "--json".into(),
        "--max-count".into(),
        max_results.to_string(),
        "--max-columns".into(),
        "400".into(),
        "-e".into(),
        query.to_string(),
    ];
    if let Some(glob) = glob {
        args.push("--glob".into());
        args.push(glob.to_string());
    }

    let mut command = tokio::process::Command::new("rg");
    command
        .args(&args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let output = command
        .output()
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    // Exit code 2 means rg failed; most commonly an invalid regex.
    if output.status.code() == Some(2) {
        if stderr.contains("regex") {
            return Err(format!("invalid regex: {stderr}"));
        }
        return Err(if stderr.is_empty() {
            "rg exited with an error".to_string()
        } else {
            stderr
        });
    }

    let mut hits = Vec::new();
    let mut files_scanned = 0usize;
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            Some("begin") => files_scanned += 1,
            Some("match") => {
                let data = &value["data"];
                let path = data["path"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let line_number = data["line_number"].as_u64().unwrap_or(0) as usize;
                let text = data["lines"]["text"].as_str().unwrap_or_default();
                hits.push(SearchHit {
                    path: path.replace('\\', "/"),
                    line: line_number,
                    text: text.trim_end_matches(['\n', '\r']).to_string(),
                });
            }
            _ => {}
        }
    }

    let truncated = hits.len() >= max_results;
    Ok(SearchOutcome {
        hits,
        truncated,
        engine: "ripgrep".into(),
        files_scanned,
        error: None,
    })
}

async fn fallback_search(root: &Path, query: &str, max_results: usize) -> SearchOutcome {
    let root_owned = root.to_path_buf();
    let query_owned = query.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        fallback_search_blocking(&root_owned, &query_owned, max_results)
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(err) => SearchOutcome {
            hits: Vec::new(),
            truncated: false,
            engine: "fallback".into(),
            files_scanned: 0,
            error: Some(err.to_string()),
        },
    }
}

fn fallback_search_blocking(root: &Path, query: &str, max_results: usize) -> SearchOutcome {
    let pattern = match RegexBuilder::new(query).case_insensitive(false).build() {
        Ok(re) => re,
        Err(_) => match Regex::new(&regex::escape(query)) {
            Ok(re) => re,
            Err(e) => {
                return SearchOutcome {
                    hits: Vec::new(),
                    truncated: false,
                    engine: "fallback".into(),
                    files_scanned: 0,
                    error: Some(e.to_string()),
                }
            }
        },
    };

    let mut hits = Vec::new();
    let mut files_scanned = 0usize;
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().is_some_and(|t| t.is_dir()) {
                return !DEFAULT_EXCLUDES.contains(&name.as_str());
            }
            true
        })
        .build();

    'outer: for result in walker {
        let Ok(entry) = result else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > 1_048_576 {
            continue;
        }
        let Ok(raw) = std::fs::read(path) else {
            continue;
        };
        if crate::util::is_probably_binary(&raw) {
            continue;
        }
        let Ok(text) = String::from_utf8(raw) else {
            continue;
        };
        files_scanned += 1;
        let relative = relative_to(root, path);
        for (index, line) in text.lines().enumerate() {
            if pattern.is_match(line) {
                hits.push(SearchHit {
                    path: relative.clone(),
                    line: index + 1,
                    text: line.chars().take(400).collect(),
                });
                if hits.len() >= max_results {
                    truncated = true;
                    break 'outer;
                }
            }
        }
    }

    SearchOutcome {
        hits,
        truncated,
        engine: "fallback".into(),
        files_scanned,
        error: None,
    }
}

/// Read a workspace file as text, with line slicing.
pub fn read_text_file(path: &Path, max_bytes: u64) -> Result<String> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > max_bytes {
        return Err(RaiError::FileTooLarge {
            path: slash(path),
            size: meta.len(),
            limit: max_bytes,
        });
    }
    let raw = std::fs::read(path)?;
    if crate::util::is_probably_binary(&raw) {
        return Err(RaiError::NotText { path: slash(path) });
    }
    String::from_utf8(raw).map_err(|_| RaiError::NotText { path: slash(path) })
}

/// Slice text into a `start..=end` line window (1-based, inclusive).
pub fn slice_lines(text: &str, start: Option<usize>, end: Option<usize>) -> (String, usize, usize) {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let from = start.unwrap_or(1).max(1);
    let to = end.unwrap_or(total).min(total).max(from);
    if total == 0 {
        return (String::new(), 0, 0);
    }
    let mut out = String::new();
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        if number < from || number > to {
            continue;
        }
        out.push_str(&format!("{number:>5} | {line}\n"));
    }
    (out, from, to)
}

/// Extract symbol-like declarations for a language, line by line.
pub fn extract_symbols(lang: &str, text: &str) -> Vec<Symbol> {
    let rules = rules_for(lang);
    let mut symbols = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if symbols.len() >= 400 {
            break;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('#') && !trimmed.starts_with("#[") {
            // Cheap comment skip; keeps the scan honest without a parser.
            if trimmed.starts_with("//") {
                continue;
            }
        }
        for rule in rules {
            if let Some(caps) = rule.pattern.captures(line) {
                if let Some(name) = caps.get(1) {
                    let name = name.as_str().trim().trim_end_matches('(');
                    if name.is_empty() {
                        continue;
                    }
                    symbols.push(Symbol {
                        name: name.to_string(),
                        kind: rule.kind.to_string(),
                        line: index + 1,
                    });
                    break;
                }
            }
        }
    }
    symbols
}

struct Rule {
    pattern: Regex,
    kind: &'static str,
}

fn compile(pattern: &str, kind: &'static str) -> Rule {
    Rule {
        pattern: Regex::new(pattern).expect("valid symbol regex"),
        kind,
    }
}

fn rules_for(lang: &str) -> &'static [Rule] {
    static RUST: OnceLock<Vec<Rule>> = OnceLock::new();
    static PY: OnceLock<Vec<Rule>> = OnceLock::new();
    static TS: OnceLock<Vec<Rule>> = OnceLock::new();
    static GO: OnceLock<Vec<Rule>> = OnceLock::new();
    static JAVA: OnceLock<Vec<Rule>> = OnceLock::new();
    static GENERIC: OnceLock<Vec<Rule>> = OnceLock::new();

    match lang {
        "rust" => RUST.get_or_init(|| {
            vec![
                compile(
                    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+\x22[^\x22]*\x22\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)",
                    "fn",
                ),
                compile(
                    r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|union)\s+([A-Za-z_][A-Za-z0-9_]*)",
                    "type",
                ),
                compile(r"^\s*impl(?:<[^>]*>)?\s+([A-Za-z_][A-Za-z0-9_:<>, ]*)", "impl"),
                compile(r"^\s*(?:pub\s+)?(?:const|static)\s+([A-Za-z_][A-Za-z0-9_]*)", "const"),
                compile(r"^\s*(?:pub\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)", "mod"),
                compile(r"macro_rules!\s*([A-Za-z_][A-Za-z0-9_]*)", "macro"),
            ]
        }),
        "python" => PY.get_or_init(|| {
            vec![
                compile(r"^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)", "fn"),
                compile(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)", "class"),
            ]
        }),
        "typescript" | "javascript" => TS.get_or_init(|| {
            vec![
                compile(
                    r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)",
                    "fn",
                ),
                compile(r"^\s*(?:export\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)", "class"),
                compile(r"^\s*(?:export\s+)?(?:interface|type|enum)\s+([A-Za-z_$][A-Za-z0-9_$]*)", "type"),
                compile(r"^\s*(?:export\s+)?const\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*[:=]", "const"),
            ]
        }),
        "go" => GO.get_or_init(|| {
            vec![
                compile(r"^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)", "fn"),
                compile(r"^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)", "type"),
            ]
        }),
        "java" | "csharp" | "kotlin" => JAVA.get_or_init(|| {
            vec![
                compile(
                    r"^\s*(?:\w+\s+)*(?:class|interface|enum|record|struct)\s+([A-Za-z_][A-Za-z0-9_]*)",
                    "type",
                ),
                compile(
                    r"^\s*(?:public|private|protected|internal|static|final|override|virtual|async|\s)+[\w<>\[\],\.]+\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(",
                    "fn",
                ),
            ]
        }),
        _ => GENERIC.get_or_init(|| {
            vec![compile(
                r"^\s*(?:pub\s+)?(?:fn|def|function|class|struct|interface|enum|trait|type|const|func|sub|module)\s+([A-Za-z_][A-Za-z0-9_]*)",
                "symbol",
            )]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub struct Alpha;\npub fn beta() {}\ntrait Gamma {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("README.md"), "# Title\n\nsome words\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/junk.rs"), "pub fn junk() {}\n").unwrap();
        dir
    }

    #[test]
    fn builds_index_and_skips_build_dirs() {
        let dir = workspace();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        let paths: Vec<&str> = index.entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src/lib.rs"));
        assert!(paths.contains(&"README.md"));
        assert!(!paths.iter().any(|p| p.starts_with("target/")));
    }

    #[test]
    fn extracts_rust_symbols() {
        let dir = workspace();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        let entry = index.entry_for("src/lib.rs").unwrap();
        let names: Vec<&str> = entry.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"Gamma"));
    }

    #[test]
    fn detects_language_by_extension() {
        let dir = workspace();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        assert_eq!(index.entry_for("src/lib.rs").unwrap().lang, "rust");
    }

    #[test]
    fn finds_files_by_token() {
        let dir = workspace();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        let hits = index.find_files("lib", 5);
        assert!(hits.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn finds_symbols_by_name() {
        let dir = workspace();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        let hits = index.find_symbols("alph", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.name, "Alpha");
    }

    #[test]
    fn slices_line_windows() {
        let text = "a\nb\nc\nd\n";
        let (out, from, to) = slice_lines(text, Some(2), Some(3));
        assert_eq!((from, to), (2, 3));
        assert!(out.contains("    2 | b"));
        assert!(!out.contains("1 | a"));
    }

    #[test]
    fn index_round_trips_through_disk() {
        let dir = workspace();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        let path = dir.path().join("index.json");
        index.save(&path).unwrap();
        let loaded = RepoIndex::load(&path).unwrap();
        assert_eq!(loaded.file_count(), index.file_count());
    }

    #[tokio::test]
    async fn fallback_search_finds_text() {
        let dir = workspace();
        let outcome = fallback_search(dir.path(), "pub fn beta", 10).await;
        assert!(outcome
            .hits
            .iter()
            .any(|h| h.path == "src/lib.rs" && h.line == 2));
    }

    #[tokio::test]
    async fn search_prefers_ripgrep_when_present() {
        let dir = workspace();
        let outcome = search_text(dir.path(), "beta", None, 10, true).await;
        assert!(outcome.error.is_none());
        assert!(outcome.hits.iter().any(|h| h.path.contains("lib.rs")));
    }

    #[test]
    fn binary_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0u8, 1, 2, 3, 0, 9]).unwrap();
        let index = RepoIndex::build(dir.path(), &IndexOptions::default()).unwrap();
        assert_eq!(index.entry_for("blob.bin").unwrap().symbols.len(), 0);
    }
}
