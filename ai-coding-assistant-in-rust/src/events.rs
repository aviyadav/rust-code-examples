//! The structured event model.
//!
//! Everything the assistant does is expressed as an event. Once events are
//! structured they can be rendered for a terminal, written as JSONL logs, or
//! consumed by a future UI without touching the agent loop.

use std::io::Write;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::render;
use crate::util::now_rfc3339;

/// One event in the run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// High level phase change: reading context, planning, applying, verifying.
    Phase { name: String, detail: String },
    /// Final text for one model message.
    ModelText { text: String },
    /// Incremental streamed model text.
    ModelDelta { text: String },
    /// A tool call is about to execute, after policy evaluation.
    ToolStart {
        call_id: String,
        tool: String,
        risk: String,
        args: Value,
    },
    /// Incremental tool output (streamed stdout/stderr).
    ToolStream {
        call_id: String,
        stream: String,
        chunk: String,
    },
    /// A tool call finished.
    ToolFinish {
        call_id: String,
        tool: String,
        ok: bool,
        duration_ms: u64,
        summary: String,
        retry_safe: bool,
    },
    /// A patch was applied (or dry-run validated).
    PatchApplied {
        dry_run: bool,
        files: Vec<String>,
        added: u64,
        removed: u64,
        dirty_files: Vec<String>,
    },
    /// An approval decision was made.
    Approval {
        call_id: String,
        tool: String,
        risk: String,
        decision: String,
        reason: String,
    },
    /// A sandboxed command finished.
    CommandResult {
        argv: Vec<String>,
        exit_code: Option<i32>,
        timed_out: bool,
        duration_ms: u64,
        stdout_bytes: u64,
        stderr_bytes: u64,
        truncated: bool,
        redactions: usize,
    },
    /// Informational note that is not a failure.
    Notice { message: String },
    /// A failure, with enough detail to debug the next session.
    Error {
        message: String,
        kind: String,
        retry_safe: bool,
    },
    /// The run finished.
    Done {
        mode: String,
        ok: bool,
        tool_calls: u64,
        files_changed: u64,
        commands_run: u64,
        duration_ms: u64,
    },
}

impl Event {
    /// Short label used by the human renderer.
    pub fn label(&self) -> &'static str {
        match self {
            Event::Phase { .. } => "phase",
            Event::ModelText { .. } => "model",
            Event::ModelDelta { .. } => "delta",
            Event::ToolStart { .. } => "tool",
            Event::ToolStream { .. } => "stream",
            Event::ToolFinish { .. } => "tool",
            Event::PatchApplied { .. } => "patch",
            Event::Approval { .. } => "approval",
            Event::CommandResult { .. } => "command",
            Event::Notice { .. } => "notice",
            Event::Error { .. } => "error",
            Event::Done { .. } => "done",
        }
    }
}

/// An event with envelope metadata, as persisted to the session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub ts: String,
    pub session: String,
    #[serde(flatten)]
    pub event: Event,
}

/// How events are presented to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Human readable terminal output.
    Text,
    /// One JSON object per line (machine consumable).
    Json,
}

/// Where events go: terminal, session log, or both.
///
/// The emitter is shared across concurrently running tasks, so writes are
/// serialized behind a mutex to keep lines intact.
#[derive(Clone)]
pub struct Emitter {
    mode: OutputMode,
    quiet: bool,
    session: String,
    out: Arc<Mutex<Box<dyn Write + Send>>>,
    log: Arc<Mutex<Option<std::fs::File>>>,
    /// Session transcript to mirror durable events into, if any.
    mirror: Option<(Arc<crate::session::SessionStore>, String)>,
}

impl Emitter {
    /// Create an emitter writing to stdout, optionally also to a JSONL log.
    pub fn new(mode: OutputMode, quiet: bool, session: impl Into<String>) -> Self {
        Self {
            mode,
            quiet,
            session: session.into(),
            out: Arc::new(Mutex::new(Box::new(std::io::stdout()))),
            log: Arc::new(Mutex::new(None)),
            mirror: None,
        }
    }

    /// Mirror durable events into a session transcript.
    ///
    /// Used by the CLI and the MCP server so `rai sessions show` reflects what
    /// actually happened, not just the conversation.
    pub fn mirror_into(mut self, store: Arc<crate::session::SessionStore>) -> Self {
        self.mirror = Some((store, self.session.clone()));
        self
    }

    /// Create an emitter that only writes to an in-memory buffer (tests).
    pub fn silent(session: impl Into<String>) -> Self {
        Self {
            mode: OutputMode::Text,
            quiet: true,
            session: session.into(),
            out: Arc::new(Mutex::new(Box::new(Vec::<u8>::new()))),
            log: Arc::new(Mutex::new(None)),
            mirror: None,
        }
    }

    /// Attach a JSONL session log.
    pub fn with_log(mut self, file: std::fs::File) -> Self {
        self.log = Arc::new(Mutex::new(Some(file)));
        self
    }

    /// Attach a JSONL log file, creating parent directories as needed.
    ///
    /// Used by long-running surfaces (the MCP server) that have no terminal.
    pub fn with_log_file(self, path: &std::path::Path) -> Self {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(file) => self.with_log(file),
            Err(_) => self,
        }
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn mode(&self) -> OutputMode {
        self.mode
    }

    pub fn quiet(&self) -> bool {
        self.quiet
    }

    /// Emit an event to the terminal and the log.
    pub fn emit(&self, event: Event) {
        self.log_event(&event);
        // The session transcript is the durable work log: it is written even
        // when the terminal is quiet, and it excludes high-volume stream noise.
        if let Some((store, id)) = &self.mirror {
            if !matches!(event, Event::ModelDelta { .. } | Event::ToolStream { .. }) {
                let _ = store.append_event(id, &event);
            }
        }
        if self.quiet {
            return;
        }
        match self.mode {
            OutputMode::Json => {
                let record = EventRecord {
                    ts: now_rfc3339(),
                    session: self.session.clone(),
                    event,
                };
                if let Ok(line) = serde_json::to_string(&record) {
                    self.write_line(&line);
                }
            }
            OutputMode::Text => {
                if let Some(text) = render::render_human(&event) {
                    self.write_line(&text);
                }
            }
        }
    }

    /// Write raw text with no formatting (used for streamed model output).
    pub fn emit_stream_text(&self, text: &str) {
        if self.quiet || self.mode == OutputMode::Json {
            return;
        }
        if let Ok(mut out) = self.out.lock() {
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
        }
    }

    fn log_event(&self, event: &Event) {
        let Ok(mut guard) = self.log.lock() else {
            return;
        };
        let Some(file) = guard.as_mut() else {
            return;
        };
        let record = EventRecord {
            ts: now_rfc3339(),
            session: self.session.clone(),
            event: event.clone(),
        };
        if let Ok(line) = serde_json::to_string(&record) {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }

    pub(crate) fn write_line(&self, line: &str) {
        if let Ok(mut out) = self.out.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("mode", &self.mode)
            .field("quiet", &self.quiet)
            .field("session", &self.session)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_with_a_stable_tag() {
        let event = Event::Phase {
            name: "planning".into(),
            detail: "planning edit".into(),
        };
        let json: Value = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "phase");
        assert_eq!(json["name"], "planning");
    }

    #[test]
    fn tool_finish_round_trips() {
        let event = Event::ToolFinish {
            call_id: "c1".into(),
            tool: "read_file".into(),
            ok: true,
            duration_ms: 3,
            summary: "read 12 lines".into(),
            retry_safe: true,
        };
        let text = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&text).unwrap();
        assert_eq!(back.label(), "tool");
    }

    #[test]
    fn silent_emitter_writes_nothing_visible() {
        let emitter = Emitter::silent("s1");
        emitter.emit(Event::Notice {
            message: "hello".into(),
        });
        assert_eq!(emitter.session(), "s1");
    }
}
