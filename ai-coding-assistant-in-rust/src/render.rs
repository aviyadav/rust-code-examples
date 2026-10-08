//! Human-readable rendering of events.
//!
//! The renderer is deliberately plain: one aligned label column, no color
//! dependency, no emoji. Machine consumers use the JSON output mode instead.

use crate::events::Event;
use crate::util::human_bytes;

const LABEL: usize = 11;

fn line(label: &str, body: impl AsRef<str>) -> String {
    format!("{:<LABEL$}{}", format!("[{label}]"), body.as_ref())
}

/// Render one event for a terminal. Returns `None` when the event is already
/// handled by another output path (streamed text).
pub fn render_human(event: &Event) -> Option<String> {
    match event {
        Event::Phase { name, detail } => Some(line(name, detail)),
        Event::ModelText { .. } | Event::ModelDelta { .. } => None,
        Event::ToolStart {
            tool, risk, args, ..
        } => {
            let preview = summarize_args(args);
            if preview.is_empty() {
                Some(line("tool", format!("{tool} ({risk})")))
            } else {
                Some(line("tool", format!("{tool} ({risk}) {preview}")))
            }
        }
        Event::ToolStream { stream, chunk, .. } => {
            let text = chunk.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                None
            } else {
                Some(line(&format!("out:{stream}"), text))
            }
        }
        Event::ToolFinish {
            tool,
            ok,
            duration_ms,
            summary,
            retry_safe,
            ..
        } => {
            let state = if *ok { "ok" } else { "failed" };
            let retry = if *ok || *retry_safe {
                ""
            } else {
                " (not retry-safe)"
            };
            Some(line(
                "tool",
                format!("{tool} {state} in {duration_ms}ms - {summary}{retry}"),
            ))
        }
        Event::PatchApplied {
            dry_run,
            files,
            added,
            removed,
            dirty_files,
        } => {
            let verb = if *dry_run { "would apply" } else { "applied" };
            let dirty = if dirty_files.is_empty() {
                String::new()
            } else {
                format!(" (already modified: {})", dirty_files.join(", "))
            };
            Some(line(
                "patch",
                format!("{verb} {} file(s), +{added} -{removed}{dirty}", files.len()),
            ))
        }
        Event::Approval {
            tool,
            risk,
            decision,
            reason,
            ..
        } => Some(line(
            "approval",
            format!("{tool} ({risk}) -> {decision}: {reason}"),
        )),
        Event::CommandResult {
            argv,
            exit_code,
            timed_out,
            duration_ms,
            stdout_bytes,
            stderr_bytes,
            truncated,
            redactions,
        } => {
            let status = match (timed_out, exit_code) {
                (true, _) => "timed out".to_string(),
                (false, Some(code)) => format!("exit {code}"),
                (false, None) => "no exit code".to_string(),
            };
            let mut extras = Vec::new();
            if *truncated {
                extras.push("output truncated".to_string());
            }
            if *redactions > 0 {
                extras.push(format!("{redactions} redaction(s)"));
            }
            let extra = if extras.is_empty() {
                String::new()
            } else {
                format!("; {}", extras.join(", "))
            };
            Some(line(
                "command",
                format!(
                    "{} -> {status} in {duration_ms}ms (stdout {}, stderr {}){extra}",
                    argv.join(" "),
                    human_bytes(*stdout_bytes),
                    human_bytes(*stderr_bytes)
                ),
            ))
        }
        Event::Notice { message } => Some(line("notice", message)),
        Event::Error { message, kind, .. } => Some(line("error", format!("{message} [{kind}]"))),
        Event::Done {
            mode,
            ok,
            tool_calls,
            files_changed,
            commands_run,
            duration_ms,
        } => {
            let state = if *ok { "complete" } else { "ended with errors" };
            Some(line(
                "done",
                format!(
                    "{mode} {state}: {tool_calls} tool call(s), {files_changed} file(s) changed, {commands_run} command(s), {duration_ms}ms"
                ),
            ))
        }
    }
}

/// Compact one-line preview of tool arguments for the terminal.
fn summarize_args(args: &serde_json::Value) -> String {
    let serde_json::Value::Object(map) = args else {
        return String::new();
    };
    let mut parts = Vec::new();
    for (key, value) in map.iter().take(3) {
        let text = match value {
            serde_json::Value::String(s) => {
                let mut s = s.clone();
                s = s.replace('\n', "\\n");
                crate::util::truncate_line(&s, 60)
            }
            other => crate::util::truncate_line(&other.to_string(), 40),
        };
        parts.push(format!("{key}={text}"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("({})", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn phase_is_aligned() {
        let out = render_human(&Event::Phase {
            name: "plan".into(),
            detail: "planning edit".into(),
        })
        .unwrap();
        assert!(out.starts_with("[plan]"));
        assert!(out.ends_with("planning edit"));
    }

    #[test]
    fn streamed_deltas_are_not_rendered_twice() {
        assert!(render_human(&Event::ModelDelta { text: "hi".into() }).is_none());
    }

    #[test]
    fn patch_line_reports_stats() {
        let out = render_human(&Event::PatchApplied {
            dry_run: false,
            files: vec!["src/main.rs".into()],
            added: 4,
            removed: 2,
            dirty_files: vec![],
        })
        .unwrap();
        assert!(out.contains("+4 -2"));
    }

    #[test]
    fn tool_args_preview_is_bounded() {
        let out = render_human(&Event::ToolStart {
            call_id: "c".into(),
            tool: "read_file".into(),
            risk: "ReadOnly".into(),
            args: json!({"path": "a".repeat(400)}),
        })
        .unwrap();
        assert!(out.len() < 200);
    }
}
