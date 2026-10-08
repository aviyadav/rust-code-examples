//! Small shared helpers: ids, time, byte budgets, path confinement, language
//! detection, and cooperative cancellation.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{RaiError, Result};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// RFC 3339 timestamp with millisecond precision (UTC).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Monotonic-ish short unique id, used for sessions and tool call ids.
pub fn short_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{:08x}{:04x}", millis & 0xffff_ffff, n as u16)
}

/// Current unix time in seconds.
pub fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Human readable byte size.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Truncate to a byte budget on a character boundary, reporting whether
/// truncation happened.
pub fn truncate_bytes(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_string();
    out.push_str(&format!(
        "\n... [truncated, {} omitted]",
        human_bytes((text.len() - end) as u64)
    ));
    (out, true)
}

/// Truncate a single line for display.
pub fn truncate_line(text: &str, max: usize) -> String {
    let single = text.replace('\n', "\\n");
    if single.chars().count() <= max {
        single
    } else {
        let head: String = single.chars().take(max.saturating_sub(1)).collect();
        format!("{head}...")
    }
}

/// Heuristic binary detection: NUL byte, or a high share of control bytes.
pub fn is_probably_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.contains(&0) {
        return true;
    }
    if sample.is_empty() {
        return false;
    }
    let control = sample
        .iter()
        .filter(|b| **b < 0x09 || (**b > 0x0d && **b < 0x20))
        .count();
    control * 100 / sample.len() > 10
}

/// Detect a language id from the file extension (best effort).
pub fn detect_language(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" | "tsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "scala" => "scala",
        "sh" | "bash" | "zsh" => "shell",
        "ps1" => "powershell",
        "sql" => "sql",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "proto" => "protobuf",
        _ => return None,
    };
    Some(lang.to_string())
}

/// Normalize a path for display: forward slashes, no verbatim prefix.
pub fn slash(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.strip_prefix("//?/").map(|x| x.to_string()).unwrap_or(s)
}

/// Lexically normalize a path without touching the filesystem.
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve a workspace-relative path, refusing anything that escapes the root.
///
/// This is the single choke point for "where can the model write". It rejects
/// absolute paths, `..` traversal, NUL bytes, and symlinks that point outside
/// the workspace.
pub fn safe_join(root: &Path, raw: &str) -> Result<PathBuf> {
    if raw.trim().is_empty() {
        return Err(RaiError::AbsolutePath("<empty path>".to_string()));
    }
    if raw.contains('\0') {
        return Err(RaiError::AbsolutePath(raw.replace('\0', "\\0")));
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(RaiError::AbsolutePath(raw.to_string()));
    }
    for comp in candidate.components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RaiError::PathEscape {
                    path: raw.to_string(),
                    root: slash(root),
                });
            }
            _ => {}
        }
    }

    let normalized_root = normalize(root);
    let joined = normalize(&normalized_root.join(candidate));
    if !joined.starts_with(&normalized_root) {
        return Err(RaiError::PathEscape {
            path: raw.to_string(),
            root: slash(root),
        });
    }

    // If the target already exists, follow symlinks before trusting it.
    if let Ok(real) = joined.canonicalize() {
        let real_root = normalized_root
            .canonicalize()
            .unwrap_or_else(|_| normalized_root.clone());
        if !real.starts_with(&real_root) {
            return Err(RaiError::PathEscape {
                path: raw.to_string(),
                root: slash(root),
            });
        }
    }

    Ok(joined)
}

/// Make a path relative to the workspace root, for display.
pub fn relative_to(root: &Path, path: &Path) -> String {
    let norm_root = normalize(root);
    let norm_path = normalize(path);
    match norm_path.strip_prefix(&norm_root) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => slash(rel),
        Err(_) => slash(path),
    }
}

/// Run a blocking closure on the blocking pool.
///
/// Repository walks, hashing, and process spawning for metadata must not occupy
/// async worker threads.
pub async fn blocking<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(inner) => inner,
        Err(join) => Err(RaiError::Io(std::io::Error::other(format!(
            "blocking task failed: {join}"
        )))),
    }
}

/// Cooperative cancellation shared across the agent loop, tools, and children.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    inner: Arc<CancelState>,
}

#[derive(Debug, Default)]
struct CancelState {
    flag: AtomicU64,
    notify: tokio::sync::Notify,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation and wake every waiter.
    pub fn cancel(&self) {
        self.inner.flag.store(1, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::SeqCst) == 1
    }

    /// Resolve once cancellation is requested.
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.inner.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// Return `Cancelled` if a cancellation was requested.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(RaiError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Install a Ctrl-C handler that trips the supplied flag.
pub fn install_ctrlc(cancel: Cancel) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel.cancel();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escape_attempts() {
        let root = Path::new("/workspace");
        assert!(safe_join(root, "../etc/passwd").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "a/../../b").is_err());
        assert!(safe_join(root, "C:\\Windows").is_err());
        assert!(safe_join(root, "src/main.rs").is_ok());
        assert!(safe_join(root, "./src/./lib.rs").is_ok());
    }

    #[test]
    fn truncates_on_char_boundary() {
        let text = "héllo wörld";
        let (out, truncated) = truncate_bytes(text, 4);
        assert!(truncated);
        assert!(out.starts_with("hél"));
    }

    #[test]
    fn detects_binary() {
        assert!(is_probably_binary(b"\x00\x01\x02"));
        assert!(!is_probably_binary(b"fn main() {}"));
    }

    #[test]
    fn detects_language() {
        assert_eq!(
            detect_language(Path::new("src/main.rs")).as_deref(),
            Some("rust")
        );
        assert_eq!(
            detect_language(Path::new("app.py")).as_deref(),
            Some("python")
        );
        assert_eq!(detect_language(Path::new("LICENSE")), None);
    }

    #[test]
    fn cancellation_is_observable() {
        let cancel = Cancel::new();
        assert!(!cancel.is_cancelled());
        assert!(cancel.check().is_ok());
        cancel.cancel();
        assert!(cancel.is_cancelled());
        assert!(cancel.check().is_err());
    }
}
