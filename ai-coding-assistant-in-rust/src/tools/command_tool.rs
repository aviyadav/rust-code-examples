//! Command execution tools.
//!
//! Every command goes through the sandbox: explicit argv, controlled working
//! directory, filtered environment, timeout, output caps, redaction, and a
//! transcript entry. Policy is re-checked here on the resolved argv so an MCP
//! caller cannot bypass `[commands]` by omitting arguments.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    object_schema, optional_bool, optional_str, optional_usize, require_string_array, Tool,
    ToolContext, ToolDefinition, ToolOutcome,
};
use crate::error::{RaiError, Result};
use crate::events::Event;
use crate::policy::Decision;
use crate::sandbox::{self, CommandSpec};

/// Run `<program> <args...>` inside the workspace.
pub struct RunCommand;

/// Infer the project test command from manifest files.
///
/// Deliberately conservative: it recognises a few manifests and otherwise
/// reports nothing so the caller can be explicit instead of guessing.
pub fn infer_test_command(root: &Path) -> Option<Vec<String>> {
    let has = |name: &str| root.join(name).exists();
    if has("Cargo.toml") {
        return Some(vec!["cargo".into(), "test".into()]);
    }
    if has("package.json") {
        return Some(vec!["npm".into(), "test".into()]);
    }
    if has("pyproject.toml") || has("pytest.ini") || has("tox.ini") {
        return Some(vec!["python".into(), "-m".into(), "pytest".into()]);
    }
    if has("go.mod") {
        return Some(vec!["go".into(), "test".into(), "./...".into()]);
    }
    if has("Makefile") {
        return Some(vec!["make".into(), "test".into()]);
    }
    None
}

/// Infer the project verification commands shown to a model and to users.
pub fn infer_verification_commands(root: &Path) -> Vec<Vec<String>> {
    if root.join("Cargo.toml").exists() {
        // Cheapest signal first: does it still compile? Formatting is not a
        // correctness signal, so it is deliberately absent here.
        return vec![
            vec!["cargo".into(), "check".into()],
            vec!["cargo".into(), "test".into()],
        ];
    }
    if root.join("package.json").exists() {
        return vec![vec!["npm".into(), "test".into()]];
    }
    if root.join("pyproject.toml").exists() {
        return vec![vec!["python".into(), "-m".into(), "pytest".into()]];
    }
    if root.join("go.mod").exists() {
        return vec![vec!["go".into(), "test".into(), "./...".into()]];
    }
    Vec::new()
}

#[async_trait]
impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "run_command",
            "Run a command inside the workspace. Structured argv only. Commands not in [commands].auto_allow require approval, and [commands].deny is always refused.",
            object_schema(
                json!({
                    "argv": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Program and arguments, e.g. [\"cargo\", \"test\"]."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Workspace-relative working directory. Defaults to the workspace root."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Override the configured timeout (capped at 3600)."
                    },
                    "shell": {
                        "type": "boolean",
                        "description": "Interpret argv through the platform shell. Requires [commands].allow_shell = true."
                    }
                }),
                &["argv"],
            ),
            super::RiskClass::Command,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, call_id: &str) -> Result<ToolOutcome> {
        let argv = require_string_array(&args, "argv", "run_command")?;
        let cwd = optional_str(&args, "cwd")
            .map(|raw| ctx.resolve(&raw))
            .transpose()?;
        let shell = optional_bool(&args, "shell").unwrap_or(false);
        let timeout = optional_usize(&args, "timeout_seconds")
            .map(|s| s as u64)
            .unwrap_or(ctx.config.commands.timeout_seconds);
        run_sandboxed(ctx, call_id, argv, cwd, shell, timeout).await
    }
}

/// Shared execution path for `run_command` and `run_tests`.
async fn run_sandboxed(
    ctx: &ToolContext,
    call_id: &str,
    argv: Vec<String>,
    cwd: Option<std::path::PathBuf>,
    shell: bool,
    timeout_seconds: u64,
) -> Result<ToolOutcome> {
    if let Decision::Denied { reason } = ctx.policy.command_decision(&argv) {
        return Err(RaiError::PolicyDenied {
            tool: "run_command".into(),
            reason,
        });
    }
    if let Decision::Denied { reason } = ctx.policy.check_shell(shell) {
        return Err(RaiError::PolicyDenied {
            tool: "run_command".into(),
            reason,
        });
    }

    let cwd = match cwd {
        Some(path) => {
            if !path.starts_with(&ctx.root) {
                return Err(RaiError::PathEscape {
                    path: crate::util::slash(&path),
                    root: crate::util::slash(&ctx.root),
                });
            }
            path
        }
        None => ctx.root.clone(),
    };
    if !cwd.is_dir() {
        return Err(RaiError::InvalidArguments {
            tool: "run_command".into(),
            problem: format!(
                "working directory does not exist: {}",
                crate::util::slash(&cwd)
            ),
        });
    }

    let max_output = ctx
        .config
        .commands
        .max_output_bytes
        .min(ctx.config.budgets.max_command_output_bytes);

    let spec = CommandSpec {
        argv: argv.clone(),
        cwd,
        timeout: Duration::from_secs(timeout_seconds.clamp(1, 3600)),
        max_output_bytes: max_output,
        allowed_env: ctx.config.commands.allowed_env.clone(),
        shell,
        stdin: None,
    };

    let call_id_owned = call_id.to_string();
    let outcome = sandbox::run(&spec, &ctx.cancel, &ctx.redactor, |kind, chunk| {
        ctx.emit(Event::ToolStream {
            call_id: call_id_owned.clone(),
            stream: kind.as_str().to_string(),
            chunk: chunk.to_string(),
        });
    })
    .await?;

    ctx.record_command(&outcome);
    ctx.emit(Event::CommandResult {
        argv: argv.clone(),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        duration_ms: outcome.duration_ms,
        stdout_bytes: outcome.stdout_bytes,
        stderr_bytes: outcome.stderr_bytes,
        truncated: outcome.truncated,
        redactions: outcome.redactions,
    });

    let mut text = format!(
        "command: {}\nstatus: {}\n",
        argv.join(" "),
        outcome.summary()
    );
    if outcome.redactions > 0 {
        text.push_str(&format!(
            "redactions: {} secret(s) masked in the output\n",
            outcome.redactions
        ));
    }
    text.push_str("---\n");
    text.push_str(&outcome.combined());
    if outcome.truncated {
        text.push_str("\n[output truncated by the rai runtime]");
    }

    let ok = outcome.success();
    let summary = format!(
        "{} -> {}",
        argv.join(" "),
        match (outcome.timed_out, outcome.exit_code) {
            (true, _) => "timed out".to_string(),
            (_, Some(code)) => format!("exit {code}"),
            (_, None) => "no exit code".to_string(),
        }
    );

    Ok(ToolOutcome::new(summary, text)
        .with_data(json!({
            "argv": argv,
            "exit_code": outcome.exit_code,
            "timed_out": outcome.timed_out,
            "cancelled": outcome.cancelled,
            "success": ok,
            "duration_ms": outcome.duration_ms,
            "truncated": outcome.truncated,
            "redactions": outcome.redactions,
        }))
        .truncated_if(outcome.truncated))
}

/// Run the project's test command through the sandbox.
///
/// `argv` is optional: when omitted the runtime infers a test command from the
/// repository (Cargo, npm, pytest, go). Whatever argv is used, policy is
/// evaluated on the *resolved* command, never on the model's intent.
pub struct RunTests;

#[async_trait]
impl Tool for RunTests {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "run_tests",
            "Run the project test command. Prefer this over composing a test command by hand; pass `argv` only when the inferred command is wrong.",
            object_schema(
                json!({
                    "argv": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Explicit command, e.g. [\"cargo\", \"test\", \"--lib\"]. Omit to use the inferred project command."
                    },
                    "package": {
                        "type": "string",
                        "description": "Limit to one workspace package when the test runner supports it."
                    },
                    "extra_args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Additional arguments appended to the command."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Override the configured command timeout."
                    }
                }),
                &[],
            ),
            super::RiskClass::Command,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, call_id: &str) -> Result<ToolOutcome> {
        let extra = args
            .get("extra_args")
            .and_then(|v| v.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();

        let mut argv: Vec<String> = match args.get("argv").and_then(|v| v.as_array()) {
            Some(_) => super::require_string_array(&args, "argv", "run_tests")?,
            None => infer_test_command(&ctx.root).ok_or_else(|| RaiError::InvalidArguments {
                tool: "run_tests".into(),
                problem:
                    "no test command could be inferred for this repository; pass `argv` explicitly"
                        .into(),
            })?,
        };

        if let Some(package) = super::optional_str(&args, "package") {
            if argv.first().map(String::as_str) == Some("cargo") {
                argv.push("-p".into());
                argv.push(package);
            }
        }
        argv.extend(extra);

        let timeout = super::optional_usize(&args, "timeout_seconds")
            .map(|v| v as u64)
            .unwrap_or(ctx.config.commands.timeout_seconds);

        run_sandboxed(ctx, call_id, argv, None, false, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn infers_cargo_test_for_rust_projects() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        assert_eq!(
            infer_test_command(dir.path()).unwrap(),
            vec!["cargo".to_string(), "test".to_string()]
        );
    }

    #[test]
    fn infers_node_and_python_and_go() {
        let node = TempDir::new().unwrap();
        fs::write(node.path().join("package.json"), "{}").unwrap();
        assert_eq!(
            infer_test_command(node.path()).unwrap(),
            vec!["npm".to_string(), "test".to_string()]
        );

        let python = TempDir::new().unwrap();
        fs::write(python.path().join("pytest.ini"), "").unwrap();
        let inferred = infer_test_command(python.path()).unwrap();
        assert!(
            inferred.iter().any(|a| a.contains("pytest")),
            "{inferred:?}"
        );

        let go = TempDir::new().unwrap();
        fs::write(go.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(
            infer_test_command(go.path()).unwrap(),
            vec!["go".to_string(), "test".to_string(), "./...".to_string()]
        );
    }

    #[test]
    fn unknown_project_yields_no_inference() {
        let dir = TempDir::new().unwrap();
        assert!(infer_test_command(dir.path()).is_none());
    }

    #[test]
    fn verification_commands_are_ordered_cheapest_first() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let commands = infer_verification_commands(dir.path());
        let first = commands.first().cloned().unwrap_or_default();
        assert_eq!(first, vec!["cargo".to_string(), "check".to_string()]);
        assert!(commands.iter().any(|c| c.join(" ") == "cargo test"));
    }
}
