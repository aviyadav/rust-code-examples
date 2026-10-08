//! End-to-end agent runs driven by a scripted provider.
//!
//! These are the tests that matter for the trust story: the model proposes,
//! and the Rust runtime decides what actually happens.

mod common;

use common::*;
use rai::agent::RunOptions;
use rai::policy::Approver;
use rai::tools::Mode;

/// Tool results the agent handed back to the model.
fn tool_results(workspace: &Workspace, session: &str) -> String {
    workspace
        .session_messages(session)
        .into_iter()
        .filter(|message| message.role == "tool")
        .map(|message| message.text)
        .collect::<Vec<_>>()
        .join("\n---\n")
}

#[tokio::test]
async fn ask_mode_searches_then_answers() {
    let workspace = Workspace::new();
    workspace.write(
        "src/lib.rs",
        "pub fn parse(input: &str) -> usize {\n    input.len()\n}\n",
    );

    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"search_text","arguments":{"query":"fn parse"}}]},
            {"text":"The parser lives in src/lib.rs."}
        ]}"#,
    );

    let config = workspace.config();
    let (agent, _cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::DenyAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Ask, "where is the parser?"))
        .await
        .expect("run succeeds");

    assert!(report.ok, "run should complete: {:?}", report.warnings);
    assert_eq!(report.tool_calls, 1);
    assert_eq!(report.model_turns, 2);
    assert!(report.final_text.contains("src/lib.rs"));
    assert!(report.files_changed.is_empty());
    assert!(tool_results(&workspace, &report.session).contains("src/lib.rs"));
}

#[tokio::test]
async fn ask_mode_does_not_grow_a_write_path() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {}\n");

    // The scripted model tries to write in `ask` mode.
    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"apply_patch","arguments":{"patch":"--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn main() {}\n+fn main() { println!(\"hi\"); }\n"}}]},
            {"text":"I could not change the file."}
        ]}"#,
    );

    let config = workspace.config_with(|config| {
        config.policy.auto_approve = vec!["WorkspaceWrite".to_string()];
    });
    let (agent, _cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::AllowAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Ask, "add a println"))
        .await
        .expect("run succeeds");

    assert_eq!(workspace.read("src/main.rs"), "fn main() {}\n");
    assert!(report.patches.is_empty());
    let results = tool_results(&workspace, &report.session);
    assert!(
        results.contains("was not executed") && results.contains("ask mode"),
        "refusal should be explicit, got: {results}"
    );
}

#[tokio::test]
async fn a_risky_call_needs_approval_and_denial_is_reported() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {}\n");

    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"apply_patch","arguments":{"patch":"--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn main() {}\n+fn main() { let x = 1; }\n"}}]},
            {"text":"Stopped."}
        ]}"#,
    );

    // Default policy: `prompt` approval, and a non-interactive approver.
    let config = workspace.config();
    let (agent, _cancel) = agent(&workspace, Mode::Edit, client, &config, Approver::DenyAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Edit, "add a binding"))
        .await
        .expect("run succeeds");

    assert_eq!(workspace.read("src/main.rs"), "fn main() {}\n");
    let results = tool_results(&workspace, &report.session);
    assert!(
        results.contains("was not executed"),
        "denial should be explained to the model, got: {results}"
    );
}

#[tokio::test]
async fn edit_mode_applies_a_patch_and_reports_files() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {\n    let x = 1;\n}\n");

    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"read_file","arguments":{"path":"src/main.rs"}}]},
            {"tool_calls":[{"name":"apply_patch","arguments":{"patch":"--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n }\n"}}]},
            {"text":"Done: x is now 2."}
        ]}"#,
    );

    let config = workspace.config_with(|config| {
        config.policy.auto_approve = vec!["WorkspaceWrite".to_string()];
    });
    let (agent, _cancel) = agent(&workspace, Mode::Edit, client, &config, Approver::AllowAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Edit, "make x equal to 2"))
        .await
        .expect("run succeeds");

    assert!(workspace.read("src/main.rs").contains("let x = 2;"));
    assert_eq!(report.files_changed, vec!["src/main.rs".to_string()]);
    assert_eq!(report.patches.len(), 1);
    assert_eq!(report.patches[0].added, 1);
    assert_eq!(report.patches[0].removed, 1);
}

#[tokio::test]
async fn dry_run_validates_without_writing() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {\n    let x = 1;\n}\n");

    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"apply_patch","arguments":{"patch":"--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n }\n"}}]},
            {"text":"Validated only."}
        ]}"#,
    );

    let config = workspace.config_with(|config| {
        config.policy.auto_approve = vec!["WorkspaceWrite".to_string()];
    });
    let (agent, _cancel) = agent(&workspace, Mode::Edit, client, &config, Approver::AllowAll).await;

    let mut options = RunOptions::new(Mode::Edit, "make x equal to 2");
    options.dry_run = true;
    let report = agent.run(options).await.expect("run succeeds");

    assert!(
        workspace.read("src/main.rs").contains("let x = 1;"),
        "dry run must not write"
    );
    assert!(report.patches.is_empty());
    assert_eq!(report.tool_calls, 1);
}

#[tokio::test]
async fn tool_call_budget_is_enforced_and_every_call_still_gets_a_result() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn a() {}\n");

    // Five calls requested, budget of two.
    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[
                {"name":"read_file","arguments":{"path":"src/lib.rs"}},
                {"name":"read_file","arguments":{"path":"src/lib.rs"}},
                {"name":"read_file","arguments":{"path":"src/lib.rs"}},
                {"name":"read_file","arguments":{"path":"src/lib.rs"}},
                {"name":"read_file","arguments":{"path":"src/lib.rs"}}
            ]},
            {"text":"Stopping here."}
        ]}"#,
    );

    let config = workspace.config();
    let (agent, _cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::DenyAll).await;
    let mut budgets = agent.budgets();
    budgets.max_tool_calls = 2;
    let agent = agent.with_budgets(budgets);

    let report = agent
        .run(RunOptions::new(Mode::Ask, "read lib.rs"))
        .await
        .expect("run succeeds");

    assert_eq!(report.tool_calls, 2, "budget must cap executed calls");

    // Every tool_call id must still receive a result, or providers reject the
    // next request, so the over-budget calls get a refusal instead of silence.
    let messages = workspace.session_messages(&report.session);
    let tool_messages: Vec<String> = messages
        .iter()
        .filter(|message| message.role == "tool")
        .map(|message| message.text.clone())
        .collect();
    assert_eq!(tool_messages.len(), 5, "one result per requested call");
    assert_eq!(
        tool_messages
            .iter()
            .filter(|m| m.contains("budget"))
            .count(),
        3
    );
}

#[tokio::test]
async fn a_repeated_refusal_stops_the_run_instead_of_burning_the_budget() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {}\n");

    const PATCH: &str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn main() {}\n+fn main() { println!(\"hi\"); }\n";

    // Three identical write attempts; the runtime refuses every one.
    let call = serde_json::json!([
        { "name": "apply_patch", "arguments": { "patch": PATCH } }
    ]);
    let script = serde_json::json!({
        "responses": [
            { "tool_calls": call },
            { "tool_calls": call },
            { "tool_calls": call },
            { "text": "Still here." }
        ]
    });
    let client = scripted(&script.to_string());

    let config = workspace.config();
    let (agent, _cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::DenyAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Ask, "add a println"))
        .await
        .expect("run succeeds");

    assert!(
        report.final_text.contains("stopped")
            || report.final_text.contains("refused")
            || report.final_text.contains("was refused"),
        "expected the guard to explain itself, got: {}",
        report.final_text
    );
    assert!(
        report.tool_calls <= 2,
        "the loop must stop instead of repeating: {} calls",
        report.tool_calls
    );
    assert_eq!(workspace.read("src/main.rs"), "fn main() {}\n");
}

#[tokio::test]
async fn unknown_tools_are_reported_instead_of_crashing() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn a() {}\n");

    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"delete_everything","arguments":{}}]},
            {"text":"I will not do that."}
        ]}"#,
    );

    let config = workspace.config();
    let (agent, _cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::AllowAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Ask, "delete everything"))
        .await
        .expect("run succeeds");

    let results = tool_results(&workspace, &report.session);
    assert!(results.contains("delete_everything"));
    assert!(results.contains("unknown") || results.contains("not available"));
    assert!(!report.ok || report.tool_calls == 1);
}

#[tokio::test]
async fn cancellation_stops_the_run() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn a() {}\n");

    let client = scripted(r#"{"responses":[{"text":"never runs"}]}"#);
    let config = workspace.config();
    let (agent, cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::DenyAll).await;

    cancel.cancel();
    let error = agent
        .run(RunOptions::new(Mode::Ask, "anything"))
        .await
        .expect_err("cancelled runs fail");

    assert!(matches!(error, rai::RaiError::Cancelled), "got {error:?}");
}

#[tokio::test]
async fn sessions_are_written_and_resumable() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn a() {}\n");

    let client = scripted(r#"{"responses":[{"text":"First answer."}]}"#);
    let config = workspace.config();
    let (agent, _cancel) = agent(&workspace, Mode::Ask, client, &config, Approver::DenyAll).await;
    let report = agent
        .run(RunOptions::new(Mode::Ask, "first question"))
        .await
        .expect("run succeeds");

    assert!(workspace.metadata_dir().join("sessions").exists());
    let messages = workspace.session_messages(&report.session);
    assert!(messages.iter().any(|m| m.role == "user"));
    assert!(messages.iter().any(|m| m.role == "assistant"));
}

/// `--verify` runs the project's own verification command through the sandbox
/// and records the outcome in the work log.
#[tokio::test]
async fn edit_with_verify_runs_the_project_command_through_the_sandbox() {
    let workspace = Workspace::new();
    common::write_cargo_package(&workspace, "pub fn ok() -> usize { 1 }\n");

    let client = scripted(
        r#"{"responses":[
            {"tool_calls":[{"name":"search_text","arguments":{"query":"pub fn"}}]},
            {"text":"Nothing needed changing."}
        ]}"#,
    );

    let config = workspace.config_with(|config| {
        // Verification commands are Command risk; pre-approve them for the test.
        config.policy.auto_approve = vec!["Command".to_string()];
        config.commands.timeout_seconds = 180;
    });
    let (agent, _cancel) = agent(&workspace, Mode::Edit, client, &config, Approver::AllowAll).await;

    let mut options = RunOptions::new(Mode::Edit, "verify the project still builds");
    options.verify = true;
    options.allow_commands = true;

    let report = agent.run(options).await.expect("run succeeds");

    let verification = report.verification.expect("verification ran");
    assert!(verification.ok, "cargo check should pass on a valid crate");
    assert!(!verification.steps.is_empty());
    assert_eq!(
        verification.steps[0].argv.join(" "),
        "cargo check",
        "cargo check is the cheapest signal and must run first"
    );
    assert_eq!(verification.steps[0].exit_code, Some(0));
    assert!(
        report
            .commands
            .iter()
            .any(|command| command.argv.join(" ") == "cargo check"),
        "the verification command belongs in the work log"
    );
}
