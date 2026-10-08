//! Conversation memory: persisted, resumable, and compacted.
//!
//! Raw logs become too large quickly. Sessions are stored as append-only JSONL
//! records so an interrupted task can be resumed, and compaction keeps the file
//! bounded without discarding the factual work log.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::events::Event;
use crate::util::{now_rfc3339, truncate_bytes};

/// One durable record in a session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum SessionRecord {
    /// Written once at the start of a run.
    Header(SessionHeader),
    /// A conversation message, sufficient to rebuild model context.
    Message(MessageRecord),
    /// A structured event from the run.
    Event {
        ts: String,
        #[serde(flatten)]
        event: Event,
    },
    /// Written when a session file was compacted.
    Compaction {
        ts: String,
        dropped_events: usize,
        truncated_messages: usize,
        note: String,
    },
}

/// Metadata about one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    pub id: String,
    pub ts: String,
    pub mode: String,
    pub task: String,
    pub provider: String,
    pub model: String,
    pub workspace: String,
}

/// A conversation message. Mirrors `model::Message` but is stable on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRecord {
    /// `system`, `user`, `assistant`, or `tool`.
    pub role: String,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<StoredToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// A tool call as recorded in the transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Summary row for `rai sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub ts: String,
    pub mode: String,
    pub task: String,
    pub bytes: u64,
    pub records: usize,
    pub events: usize,
    pub messages: usize,
}

/// Result of compacting a session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionReport {
    pub session: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub dropped_events: usize,
    pub truncated_messages: usize,
    /// Payload size cap that triggered the compaction.
    pub limit_bytes: usize,
}

/// Append-only session store rooted at `<metadata>/sessions`.
#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// Create a store under `<metadata_dir>/sessions`.
    pub fn new(metadata_dir: &Path) -> Self {
        Self {
            dir: metadata_dir.join("sessions"),
        }
    }

    /// Directory holding session files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn ensure_dir(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        Ok(())
    }

    /// Path of one session file.
    pub fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    /// Whether a session file already exists on disk.
    pub fn exists(&self, id: &str) -> bool {
        self.path_for(id).is_file()
    }

    /// Whether a session already carries a header record.
    ///
    /// Used instead of [`Self::exists`] because events can be mirrored into the
    /// transcript before the run has written its header.
    pub fn has_header(&self, id: &str) -> bool {
        matches!(self.header(id), Ok(Some(_)))
    }

    /// Append a single record.
    pub fn append(&self, id: &str, record: &SessionRecord) -> Result<()> {
        self.ensure_dir()?;
        let line = serde_json::to_string(record)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path_for(id))?;
        writeln!(file, "{line}")?;
        file.flush()?;
        Ok(())
    }

    /// Append a conversation message.
    pub fn append_message(&self, id: &str, message: &MessageRecord) -> Result<()> {
        self.append(id, &SessionRecord::Message(message.clone()))
    }

    /// Append an event with a timestamp envelope.
    pub fn append_event(&self, id: &str, event: &Event) -> Result<()> {
        self.append(
            id,
            &SessionRecord::Event {
                ts: now_rfc3339(),
                event: event.clone(),
            },
        )
    }

    /// Load every record, skipping unparseable lines rather than failing the
    /// whole session (a partial transcript is still useful evidence).
    pub fn load(&self, id: &str) -> Result<Vec<SessionRecord>> {
        let path = self.path_for(id);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut records = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<SessionRecord>(line) {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// Header of a session, if it exists.
    pub fn header(&self, id: &str) -> Result<Option<SessionHeader>> {
        Ok(self.load(id)?.into_iter().find_map(|r| match r {
            SessionRecord::Header(h) => Some(h),
            _ => None,
        }))
    }

    /// Conversation messages for resuming a run.
    pub fn messages(&self, id: &str) -> Result<Vec<MessageRecord>> {
        Ok(self
            .load(id)?
            .into_iter()
            .filter_map(|r| match r {
                SessionRecord::Message(m) => Some(m),
                _ => None,
            })
            .collect())
    }

    /// All sessions, newest first.
    pub fn list(&self) -> Result<Vec<SessionSummary>> {
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.into()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let records = self.load(&id).unwrap_or_default();
            let header = records.iter().find_map(|r| match r {
                SessionRecord::Header(h) => Some(h.clone()),
                _ => None,
            });
            let events = records
                .iter()
                .filter(|r| matches!(r, SessionRecord::Event { .. }))
                .count();
            let messages = records
                .iter()
                .filter(|r| matches!(r, SessionRecord::Message(_)))
                .count();
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            out.push(SessionSummary {
                id: id.clone(),
                ts: header
                    .as_ref()
                    .map(|h| h.ts.clone())
                    .unwrap_or_else(|| id.clone()),
                mode: header.as_ref().map(|h| h.mode.clone()).unwrap_or_default(),
                task: header.as_ref().map(|h| h.task.clone()).unwrap_or_default(),
                bytes,
                records: records.len(),
                events,
                messages,
            });
        }
        out.sort_by(|a, b| b.ts.cmp(&a.ts));
        Ok(out)
    }

    /// Most recent session id, if any.
    pub fn latest(&self) -> Result<Option<String>> {
        Ok(self.list()?.into_iter().next().map(|s| s.id))
    }

    /// Whether a session file exceeds the payload cap.
    pub fn needs_compaction(&self, id: &str, limit_bytes: usize) -> bool {
        std::fs::metadata(self.path_for(id))
            .map(|m| m.len() as usize > limit_bytes)
            .unwrap_or(false)
    }

    /// Compact a session in place, preserving the factual work log.
    ///
    /// Streaming deltas and duplicate model text are dropped; long tool results
    /// are truncated. The uncompacted file is kept beside it as `<id>.full.jsonl`.
    pub fn compact(
        &self,
        id: &str,
        limit_bytes: usize,
        keep_message_bytes: usize,
    ) -> Result<CompactionReport> {
        let path = self.path_for(id);
        let before_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let records = self.load(id)?;

        let mut out = Vec::with_capacity(records.len());
        let mut dropped_events = 0usize;
        let mut truncated_messages = 0usize;

        for record in records {
            match record {
                SessionRecord::Event { ts, event } => {
                    if keep_event(&event) {
                        out.push(SessionRecord::Event { ts, event });
                    } else {
                        dropped_events += 1;
                    }
                }
                SessionRecord::Message(mut message) => {
                    let (text, truncated) = truncate_bytes(&message.text, keep_message_bytes);
                    if truncated {
                        truncated_messages += 1;
                        message.text = format!("{text}\n[truncated during compaction]");
                    } else {
                        message.text = text;
                    }
                    out.push(SessionRecord::Message(message));
                }
                other => out.push(other),
            }
        }

        out.push(SessionRecord::Compaction {
            ts: now_rfc3339(),
            dropped_events,
            truncated_messages,
            note: format!("compacted to stay under {limit_bytes} bytes"),
        });

        let mut body = String::new();
        for record in &out {
            body.push_str(&serde_json::to_string(record)?);
            body.push('\n');
        }

        if before_bytes > 0 {
            let backup = self.dir.join(format!("{id}.full.jsonl"));
            let _ = std::fs::copy(&path, &backup);
        }
        let tmp = self.dir.join(format!("{id}.jsonl.tmp"));
        std::fs::write(&tmp, body.as_bytes())?;
        std::fs::rename(&tmp, &path)?;

        Ok(CompactionReport {
            session: id.to_string(),
            before_bytes,
            after_bytes: body.len() as u64,
            dropped_events,
            truncated_messages,
            limit_bytes,
        })
    }

    /// Rebuild model conversation messages from a session transcript.
    ///
    /// Tool results are dropped on resume: they are stale evidence, and the
    /// model can always re-request context through tools.
    pub fn resume_messages(&self, id: &str) -> Result<Vec<MessageRecord>> {
        Ok(self
            .messages(id)?
            .into_iter()
            .filter(|m| m.role != "tool" && (!m.text.is_empty() || !m.tool_calls.is_empty()))
            .collect::<Vec<_>>())
    }
}

/// Events worth keeping in a compacted transcript (the factual work log).
fn keep_event(event: &Event) -> bool {
    matches!(
        event,
        Event::Phase { .. }
            | Event::ToolFinish { .. }
            | Event::PatchApplied { .. }
            | Event::Approval { .. }
            | Event::CommandResult { .. }
            | Event::Notice { .. }
            | Event::Error { .. }
            | Event::Done { .. }
    )
}

/// Aggregate session counts by mode, used by `rai sessions`.
pub fn count_by_mode(summaries: &[SessionSummary]) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    for summary in summaries {
        *map.entry(summary.mode.clone()).or_insert(0) += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;
    use tempfile::TempDir;

    fn store() -> (TempDir, SessionStore) {
        let dir = TempDir::new().unwrap();
        let store = SessionStore::new(dir.path());
        (dir, store)
    }

    fn header(id: &str, ts: &str, mode: &str) -> SessionHeader {
        SessionHeader {
            id: id.into(),
            ts: ts.into(),
            mode: mode.into(),
            task: "t".into(),
            provider: "local".into(),
            model: "local".into(),
            workspace: ".".into(),
        }
    }

    #[test]
    fn appends_and_reloads_records() {
        let (_dir, store) = store();
        store
            .append(
                "s1",
                &SessionRecord::Header(header("s1", &now_rfc3339(), "ask")),
            )
            .unwrap();
        store
            .append_message(
                "s1",
                &MessageRecord {
                    role: "user".into(),
                    text: "hello".into(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            )
            .unwrap();
        store
            .append_event(
                "s1",
                &Event::Phase {
                    name: "context".into(),
                    detail: "reading".into(),
                },
            )
            .unwrap();

        assert_eq!(store.load("s1").unwrap().len(), 3);
        assert_eq!(store.header("s1").unwrap().unwrap().mode, "ask");
        assert_eq!(store.messages("s1").unwrap().len(), 1);
    }

    #[test]
    fn missing_session_loads_empty() {
        let (_dir, store) = store();
        assert!(store.load("nope").unwrap().is_empty());
        assert!(store.header("nope").unwrap().is_none());
    }

    #[test]
    fn compaction_drops_deltas_and_keeps_work_log() {
        let (_dir, store) = store();
        store
            .append_event(
                "s2",
                &Event::ModelDelta {
                    text: "x".repeat(500),
                },
            )
            .unwrap();
        store
            .append_event(
                "s2",
                &Event::PatchApplied {
                    dry_run: false,
                    files: vec!["src/lib.rs".into()],
                    added: 4,
                    removed: 1,
                    dirty_files: vec![],
                },
            )
            .unwrap();
        store
            .append_message(
                "s2",
                &MessageRecord {
                    role: "tool".into(),
                    text: "y".repeat(2000),
                    tool_calls: vec![],
                    tool_call_id: Some("c1".into()),
                },
            )
            .unwrap();

        let report = store.compact("s2", 400, 100).unwrap();
        assert_eq!(report.dropped_events, 1);
        assert_eq!(report.truncated_messages, 1);

        let records = store.load("s2").unwrap();
        let has_patch = records.iter().any(|r| {
            matches!(
                r,
                SessionRecord::Event {
                    event: Event::PatchApplied { .. },
                    ..
                }
            )
        });
        assert!(has_patch, "work log must survive compaction");
        assert!(store.dir().join("s2.full.jsonl").exists());
    }

    #[test]
    fn lists_sessions_newest_first() {
        let (_dir, store) = store();
        for (id, ts) in [
            ("a", "2020-01-01T00:00:00.000Z"),
            ("b", "2026-01-01T00:00:00.000Z"),
        ] {
            store
                .append(id, &SessionRecord::Header(header(id, ts, "ask")))
                .unwrap();
        }
        let list = store.list().unwrap();
        assert_eq!(list[0].id, "b");
        assert_eq!(store.latest().unwrap().unwrap(), "b");
        assert_eq!(count_by_mode(&list)["ask"], 2);
    }

    #[test]
    fn resume_drops_stale_tool_results() {
        let (_dir, store) = store();
        store
            .append_message(
                "s3",
                &MessageRecord {
                    role: "user".into(),
                    text: "do it".into(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            )
            .unwrap();
        store
            .append_message(
                "s3",
                &MessageRecord {
                    role: "tool".into(),
                    text: "big output".into(),
                    tool_calls: vec![],
                    tool_call_id: Some("c1".into()),
                },
            )
            .unwrap();
        let resumed = store.resume_messages("s3").unwrap();
        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].role, "user");
    }
}
