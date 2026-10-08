//! Command sandbox behaviour and the `rai run` policy gate.

use std::time::Duration;

use rai::redact::Redactor;
use rai::sandbox::{self, CommandSpec, StreamKind};
use rai::util::Cancel;

mod common;
use common::{cli, stderr, stdout, Workspace};

/// Command that prints to stdout only, cross-platform.
fn version_command() -> Vec<String> {
    vec!["cargo".to_string(), "--version".to_string()]
}

/// Command that fails and prints to stderr only, cross-platform.
fn bad_flag_command() -> Vec<String> {
    vec![
        "cargo".to_string(),
        "--definitely-not-a-cargo-flag".to_string(),
    ]
}

/// Command that prints the child's environment.
fn env_dump_command() -> Vec<String> {
    #[cfg(windows)]
    {
        vec!["cmd".to_string(), "/C".to_string(), "set".to_string()]
    }
    #[cfg(not(windows))]
    {
        vec!["env".to_string()]
    }
}

/// Command that keeps running for longer than a test timeout.
fn slow_command() -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            "ping -n 15 127.0.0.1 > NUL".to_string(),
        ]
    }
    #[cfg(not(windows))]
    {
        vec!["sh".to_string(), "-c".to_string(), "sleep 15".to_string()]
    }
}

fn spec(argv: Vec<String>, workspace: &Workspace) -> CommandSpec {
    CommandSpec::new(argv, workspace.root())
        .with_timeout(Duration::from_secs(60))
        .with_max_output(1024 * 1024)
}

#[test]
fn secret_shaped_environment_names_never_pass_through() {
    // The allowlist cannot be used to leak credentials into a child process.
    assert!(!sandbox::env_allowed("OPENAI_API_KEY", &[]));
    assert!(!sandbox::env_allowed(
        "OPENAI_API_KEY",
        &["OPENAI_API_KEY".to_string()]
    ));
    assert!(!sandbox::env_allowed("GITHUB_TOKEN", &[]));
    assert!(!sandbox::env_allowed("DB_PASSWORD", &[]));
}

#[test]
fn ordinary_variables_pass_only_when_allowed() {
    assert!(sandbox::env_allowed("PATH", &[]));
    assert!(!sandbox::env_allowed("MY_PROJECT_FLAG", &[]));
    assert!(sandbox::env_allowed(
        "MY_PROJECT_FLAG",
        &["MY_PROJECT_FLAG".to_string()]
    ));
}

#[test]
fn shell_metacharacters_are_detected() {
    assert!(sandbox::has_shell_metacharacters("cargo test && rm -rf /"));
    assert!(sandbox::has_shell_metacharacters("cat x | grep y"));
    assert!(!sandbox::has_shell_metacharacters("cargo test --lib"));
}

#[test]
fn simple_splitting_preserves_quoted_arguments() {
    let parts = sandbox::split_simple("cargo test -- --nocapture");
    assert_eq!(parts, vec!["cargo", "test", "--", "--nocapture"]);
}

#[tokio::test]
async fn stdout_and_stderr_stay_separate() {
    let workspace = Workspace::new();

    let good = sandbox::run(
        &spec(version_command(), &workspace),
        &Cancel::new(),
        &Redactor::from_env(),
        |_: StreamKind, _: &str| {},
    )
    .await
    .expect("cargo --version runs");
    assert_eq!(good.exit_code, Some(0));
    assert!(good.success());
    assert!(good.stdout.contains("cargo"), "stdout: {}", good.stdout);
    assert!(good.stderr.trim().is_empty(), "stderr: {}", good.stderr);

    let bad = sandbox::run(
        &spec(bad_flag_command(), &workspace),
        &Cancel::new(),
        &Redactor::from_env(),
        |_: StreamKind, _: &str| {},
    )
    .await
    .expect("cargo runs even with a bad flag");
    assert_ne!(bad.exit_code, Some(0));
    assert!(!bad.success());
    assert!(!bad.stderr.trim().is_empty(), "expected stderr output");
}

#[tokio::test]
async fn secrets_in_the_parent_environment_do_not_reach_the_child() {
    std::env::set_var("RAI_TEST_LEAKY_TOKEN", "super-secret-value-123");

    let workspace = Workspace::new();
    let outcome = sandbox::run(
        &spec(env_dump_command(), &workspace),
        &Cancel::new(),
        &Redactor::from_env(),
        |_: StreamKind, _: &str| {},
    )
    .await
    .expect("env dump runs");

    assert!(
        !outcome.stdout.contains("super-secret-value-123"),
        "the child must not see secret-shaped variables"
    );
    assert!(outcome.stdout.contains("PATH") || outcome.stdout.contains("Path"));
}

#[tokio::test]
async fn output_is_capped_and_flagged_as_truncated() {
    let workspace = Workspace::new();
    let outcome = sandbox::run(
        &spec(env_dump_command(), &workspace).with_max_output(64),
        &Cancel::new(),
        &Redactor::from_env(),
        |_: StreamKind, _: &str| {},
    )
    .await
    .expect("env dump runs");

    assert!(outcome.truncated, "expected the cap to bite");
    assert!(outcome.stdout.len() <= 64 + 64);
    assert!(outcome.stdout_bytes > 64);
}

#[tokio::test]
async fn a_hung_command_is_killed_at_the_timeout() {
    let workspace = Workspace::new();
    let started = std::time::Instant::now();
    let outcome = sandbox::run(
        &spec(slow_command(), &workspace).with_timeout(Duration::from_secs(1)),
        &Cancel::new(),
        &Redactor::from_env(),
        |_: StreamKind, _: &str| {},
    )
    .await
    .expect("the runner returns, it does not hang");

    assert!(outcome.timed_out, "expected a timeout");
    assert!(!outcome.success());
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "the timeout must kill the child promptly"
    );
}

#[tokio::test]
async fn known_secrets_are_redacted_from_captured_output() {
    let workspace = Workspace::new();
    let (program, args) = sandbox::shell_command("echo supersecretvalue123");
    let mut argv = vec![program];
    argv.extend(args);

    let outcome = sandbox::run(
        &spec(argv, &workspace),
        &Cancel::new(),
        &Redactor::empty().with_secret("supersecretvalue123"),
        |_: StreamKind, _: &str| {},
    )
    .await
    .expect("echo runs");

    assert!(
        !outcome.stdout.contains("supersecretvalue123"),
        "stdout: {}",
        outcome.stdout
    );
    assert!(outcome.redactions >= 1);
}

#[tokio::test]
async fn streamed_chunks_reach_the_callback() {
    let workspace = Workspace::new();
    let mut seen = String::new();
    let outcome = sandbox::run(
        &spec(version_command(), &workspace),
        &Cancel::new(),
        &Redactor::from_env(),
        |kind: StreamKind, chunk: &str| {
            if kind == StreamKind::Stdout {
                seen.push_str(chunk);
            }
        },
    )
    .await
    .expect("cargo --version runs");

    assert!(!seen.trim().is_empty());
    assert!(seen.contains("cargo"));
    assert!(outcome.stdout.contains("cargo"));
}

/// Configuration that pre-approves exactly one command shape.
const RUN_CONFIG: &str = r#"
[policy]
approval = "deny"

[commands]
auto_allow = ["cargo --version"]
timeout_seconds = 60
"#;

#[test]
fn run_executes_an_allowlisted_command() {
    let workspace = Workspace::new();
    workspace.write_config(RUN_CONFIG);

    let output = cli(&workspace, &["run", "cargo", "--version"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("cargo"));
}

#[test]
fn run_refuses_a_command_that_is_not_allowlisted() {
    let workspace = Workspace::new();
    workspace.write_config(RUN_CONFIG);

    let output = cli(&workspace, &["run", "git", "reset", "--hard"]);
    assert!(!output.status.success());
    let message = format!("{}{}", stdout(&output), stderr(&output)).to_lowercase();
    assert!(
        message.contains("denied") || message.contains("refused"),
        "expected a refusal, got: {message}"
    );
}

#[test]
fn run_splits_a_single_quoted_command_string() {
    let workspace = Workspace::new();
    workspace.write_config(RUN_CONFIG);

    let output = cli(&workspace, &["run", "cargo --version"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn run_requires_shell_for_metacharacters() {
    let workspace = Workspace::new();
    workspace.write_config(RUN_CONFIG);

    let output = cli(&workspace, &["run", "cargo --version && echo hi"]);
    assert!(!output.status.success());
    let message = format!("{}{}", stdout(&output), stderr(&output));
    assert!(
        message.contains("--shell"),
        "expected guidance about --shell, got: {message}"
    );
}

#[test]
fn run_reports_the_child_exit_code() {
    let workspace = Workspace::new();
    workspace.write_config(
        r#"
[policy]
approval = "auto"
auto_approve = ["Command"]
"#,
    );

    let failing = cli(&workspace, &["run", "cargo", "--definitely-not-a-flag"]);
    assert!(!failing.status.success());
    // The child's exit code is what the user sees, not a generic failure code.
    assert_ne!(failing.status.code(), Some(0));
    assert!(failing.status.code().is_some(), "expected a real exit code");
}
