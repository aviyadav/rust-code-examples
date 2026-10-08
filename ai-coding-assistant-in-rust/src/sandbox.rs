//! Command execution sandbox.
//!
//! A coding assistant that runs arbitrary commands without guardrails is not
//! acceptable. This runner owns: working directory, environment filtering,
//! timeout, output caps, cancellation, exit-code capture, separated
//! stdout/stderr, secret redaction, and a transcript record.
//!
//! Structured argv execution is the default. Shell interpretation is opt-in and
//! always explicit, because `cargo test` and `rm -rf .` must not look alike.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::error::{RaiError, Result};
use crate::redact::Redactor;
use crate::util::{truncate_bytes, Cancel};

/// Variables that are passed through when present. Everything else is dropped.
pub const SAFE_ENV: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SYSTEMDRIVE",
    "windir",
    "ComSpec",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "USERNAME",
    "LANG",
    "LC_ALL",
    "TERM",
    "SHELL",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "CARGO_HOME",
    "CARGO_TARGET_DIR",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "GOPATH",
    "GOROOT",
    "GOCACHE",
    "JAVA_HOME",
    "DOTNET_ROOT",
    "PYTHONIOENCODING",
    "PYTHONUNBUFFERED",
];

/// Which stream a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Stdout,
    Stderr,
}

impl StreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamKind::Stdout => "stdout",
            StreamKind::Stderr => "stderr",
        }
    }
}

/// A request to execute one command.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Executable followed by its arguments. When `shell` is true the first
    /// element is interpreted by the platform shell.
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    /// Extra environment variables (beyond [`SAFE_ENV`]) allowed through.
    pub allowed_env: Vec<String>,
    pub shell: bool,
    pub stdin: Option<String>,
}

impl CommandSpec {
    pub fn new(argv: Vec<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            argv,
            cwd: cwd.into(),
            timeout: Duration::from_secs(120),
            max_output_bytes: 64 * 1024,
            allowed_env: Vec::new(),
            shell: false,
            stdin: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_output(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }

    pub fn with_env(mut self, allowed: Vec<String>) -> Self {
        self.allowed_env = allowed;
        self
    }
}

/// The outcome of one command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
    pub redactions: usize,
}

impl CommandOutcome {
    pub fn success(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit_code == Some(0)
    }

    /// One-line summary used in tool results and logs.
    pub fn summary(&self) -> String {
        let status = if self.cancelled {
            "cancelled".to_string()
        } else if self.timed_out {
            "timed out".to_string()
        } else {
            match self.exit_code {
                Some(code) => format!("exit {code}"),
                None => "no exit code".to_string(),
            }
        };
        format!(
            "{status} in {}ms (stdout {}, stderr {}{})",
            self.duration_ms,
            self.stdout_bytes,
            self.stderr_bytes,
            if self.truncated { ", truncated" } else { "" }
        )
    }

    /// Combined output, stdout first, as handed back to a model.
    pub fn combined(&self) -> String {
        let mut out = String::new();
        if !self.stdout.is_empty() {
            out.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("--- stderr ---\n");
            out.push_str(&self.stderr);
        }
        out
    }
}

/// True when an environment variable may be passed to a child process.
pub fn env_allowed(name: &str, extra: &[String]) -> bool {
    let upper = name.to_ascii_uppercase();
    // Secret-shaped names never pass through, even if explicitly allowed.
    if crate::redact::is_secret_env_name(&upper) {
        return false;
    }
    SAFE_ENV.iter().any(|s| s.eq_ignore_ascii_case(name))
        || extra.iter().any(|s| s.eq_ignore_ascii_case(name))
}

/// Build the platform shell invocation for a raw command string.
pub fn shell_command(raw: &str) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        ("cmd".to_string(), vec!["/C".to_string(), raw.to_string()])
    }
    #[cfg(not(windows))]
    {
        ("sh".to_string(), vec!["-c".to_string(), raw.to_string()])
    }
}

/// Characters that require an explicit `--shell` acknowledgement.
pub fn has_shell_metacharacters(raw: &str) -> bool {
    raw.chars().any(|c| {
        matches!(
            c,
            '|' | '&' | ';' | '<' | '>' | '$' | '`' | '*' | '?' | '~' | '!' | '\n' | '(' | ')'
        )
    })
}

/// Execute a command with full guardrails.
///
/// `on_chunk` receives streamed, already-redacted output as it arrives.
pub async fn run(
    spec: &CommandSpec,
    cancel: &Cancel,
    redactor: &Redactor,
    mut on_chunk: impl FnMut(StreamKind, &str),
) -> Result<CommandOutcome> {
    if spec.argv.is_empty() {
        return Err(RaiError::CommandSpawn {
            command: String::new(),
            problem: "empty command".to_string(),
        });
    }

    let (program, args) = if spec.shell {
        shell_command(&spec.argv.join(" "))
    } else {
        (spec.argv[0].clone(), spec.argv[1..].to_vec())
    };

    let mut command = Command::new(&program);
    command
        .args(&args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.env_clear();
    for (key, value) in std::env::vars() {
        if env_allowed(&key, &spec.allowed_env) {
            command.env(key, value);
        }
    }
    // Keep child processes non-interactive.
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_PAGER", "cat");
    command.env("CARGO_TERM_COLOR", "never");
    command.env("NO_COLOR", "1");

    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| RaiError::CommandSpawn {
        command: spec.argv.join(" "),
        problem: e.to_string(),
    })?;

    if let Some(input) = spec.stdin.clone() {
        if let Some(mut stdin) = child.stdin.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
    } else {
        drop(child.stdin.take());
    }

    let (tx, mut rx) = mpsc::channel::<(StreamKind, String)>(512);
    if let Some(stdout) = child.stdout.take() {
        spawn_reader(stdout, StreamKind::Stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        spawn_reader(stderr, StreamKind::Stderr, tx.clone());
    }
    drop(tx);

    let deadline = tokio::time::Instant::now() + spec.timeout;
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut stdout_bytes = 0u64;
    let mut stderr_bytes = 0u64;
    let mut truncated = false;
    let mut timed_out = false;
    let mut cancelled = false;

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break;
            }
            _ = tokio::time::sleep_until(deadline) => {
                timed_out = true;
                break;
            }
            message = rx.recv() => {
                match message {
                    Some((kind, line)) => {
                        let (bucket, counter, cap) = match kind {
                            StreamKind::Stdout => (&mut stdout, &mut stdout_bytes, spec.max_output_bytes),
                            StreamKind::Stderr => (&mut stderr, &mut stderr_bytes, spec.max_output_bytes),
                        };
                        let line_bytes = line.len() as u64 + 1;
                        *counter += line_bytes;
                        if bucket.len() < cap {
                            let (text, cut) = truncate_bytes(&line, cap - bucket.len());
                            bucket.push_str(&text);
                            bucket.push('\n');
                            truncated |= cut;
                        } else {
                            truncated = true;
                        }
                        let (safe, _) = redactor.redact(line.trim_end_matches(['\r', '\n']));
                        if !safe.is_empty() {
                            on_chunk(kind, &safe);
                        }
                    }
                    None => break,
                }
            }
        }
    }

    if timed_out || cancelled {
        terminate(&mut child).await;
    }

    let status = child.wait().await.ok();
    let exit_code = status.and_then(|s| s.code());

    // Drain anything the readers buffered before exiting.
    while let Ok((kind, line)) = rx.try_recv() {
        let (bucket, counter) = match kind {
            StreamKind::Stdout => (&mut stdout, &mut stdout_bytes),
            StreamKind::Stderr => (&mut stderr, &mut stderr_bytes),
        };
        *counter += line.len() as u64 + 1;
        if bucket.len() < spec.max_output_bytes {
            let (text, cut) = truncate_bytes(&line, spec.max_output_bytes - bucket.len());
            bucket.push_str(&text);
            bucket.push('\n');
            truncated |= cut;
        } else {
            truncated = true;
        }
    }

    let (stdout, cut_out) = truncate_bytes(&stdout, spec.max_output_bytes);
    let (stderr, cut_err) = truncate_bytes(&stderr, spec.max_output_bytes);
    truncated |= cut_out || cut_err;

    let (stdout, out_redactions) = redactor.redact(&stdout);
    let (stderr, err_redactions) = redactor.redact(&stderr);

    Ok(CommandOutcome {
        argv: spec.argv.clone(),
        exit_code,
        stdout,
        stderr,
        stdout_bytes,
        stderr_bytes,
        truncated,
        timed_out,
        cancelled,
        duration_ms: started.elapsed().as_millis() as u64,
        redactions: out_redactions + err_redactions,
    })
}

fn spawn_reader<R>(reader: R, kind: StreamKind, tx: mpsc::Sender<(StreamKind, String)>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if tx.send((kind, line)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
    });
}

/// Terminate a child and its descendants.
async fn terminate(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
    let _ = child.kill().await;
}

/// True when `argv` matches an allowlist entry.
///
/// An entry matches its own argv (exact) or a prefix of it, so `cargo test`
/// also permits `cargo test --lib`.
pub fn matches_allowlist(argv: &[String], allow_entry: &str) -> bool {
    let entry: Vec<&str> = allow_entry.split_whitespace().collect();
    if entry.is_empty() || entry.len() > argv.len() {
        return false;
    }
    entry
        .iter()
        .zip(argv.iter())
        .all(|(expected, actual)| expected == actual)
}

/// True when `argv` matches any deny entry.
pub fn matches_denylist(argv: &[String], deny: &[String]) -> Option<String> {
    deny.iter()
        .find(|entry| matches_allowlist(argv, entry))
        .cloned()
}

/// Split a single string argument into argv without shell interpretation.
pub fn split_simple(raw: &str) -> Vec<String> {
    raw.split_whitespace().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matches_prefix() {
        let argv = vec!["cargo".to_string(), "test".to_string(), "--lib".to_string()];
        assert!(matches_allowlist(&argv, "cargo test"));
        assert!(!matches_allowlist(&argv, "cargo build"));
        assert!(!matches_allowlist(&argv, "cargo test --bins"));
    }

    #[test]
    fn denylist_reports_the_entry() {
        let argv = vec!["rm".to_string(), "-rf".to_string(), "/".to_string()];
        let deny = vec!["rm -rf".to_string()];
        assert_eq!(matches_denylist(&argv, &deny).as_deref(), Some("rm -rf"));
    }

    #[test]
    fn secret_env_never_passes_through() {
        assert!(!env_allowed("OPENAI_API_KEY", &[]));
        assert!(!env_allowed("MY_TOKEN", &["MY_TOKEN".to_string()]));
        assert!(env_allowed("PATH", &[]));
        assert!(env_allowed("MY_CUSTOM_VAR", &["MY_CUSTOM_VAR".to_string()]));
        assert!(!env_allowed("MY_CUSTOM_VAR", &[]));
    }

    #[test]
    fn detects_shell_metacharacters() {
        assert!(has_shell_metacharacters("cargo test && rm -rf /"));
        assert!(has_shell_metacharacters("ls | wc -l"));
        assert!(!has_shell_metacharacters("cargo test --lib"));
    }

    #[test]
    fn splits_plain_command() {
        assert_eq!(
            split_simple("cargo test --lib"),
            vec!["cargo", "test", "--lib"]
        );
    }
}
