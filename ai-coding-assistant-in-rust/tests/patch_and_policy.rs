//! Patch application and policy gates, exercised through the real registry.
//!
//! These tests call the tools directly rather than through the agent, so a
//! failure points at the tool contract, not the loop.

mod common;
use common::{context, Workspace};

use rai::error::RaiError;
use rai::tools::{builtin, Mode, ToolContext, ToolOutcome};
use serde_json::json;

const REWRITE: &str = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n-    let x = 1;\n+    let x = 2;\n }\n";

async fn call(tool: &str, args: serde_json::Value, ctx: &ToolContext) -> rai::Result<ToolOutcome> {
    let registry = builtin();
    let handle = registry.get(tool).expect("tool is registered");
    handle.execute(args, ctx, "test-call").await
}

fn writeable(workspace: &Workspace) -> rai::config::Config {
    workspace.config_with(|config| {
        config.workspace.allow_writes = true;
    })
}

#[tokio::test]
async fn applies_a_patch_and_reports_stats() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {\n    let x = 1;\n}\n");
    let config = writeable(&workspace);
    let ctx = context(&workspace, &config, Mode::Edit).await;

    let outcome = call("apply_patch", json!({ "patch": REWRITE }), &ctx)
        .await
        .expect("patch applies");

    assert!(workspace.read("src/main.rs").contains("let x = 2;"));
    assert_eq!(outcome.data.as_ref().unwrap()["added"], 1);
    assert_eq!(outcome.data.as_ref().unwrap()["removed"], 1);
    assert!(outcome.text.contains("modified src/main.rs"));
}

#[tokio::test]
async fn a_dry_run_validates_without_writing() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {\n    let x = 1;\n}\n");
    let config = writeable(&workspace);
    let ctx = context(&workspace, &config, Mode::Edit).await;

    let outcome = call(
        "apply_patch",
        json!({ "patch": REWRITE, "dry_run": true }),
        &ctx,
    )
    .await
    .expect("dry run validates");

    assert!(workspace.read("src/main.rs").contains("let x = 1;"));
    assert!(outcome.text.contains("dry run"));
    assert_eq!(outcome.data.as_ref().unwrap()["dry_run"], true);
}

#[tokio::test]
async fn ask_mode_cannot_write_files() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {\n    let x = 1;\n}\n");
    let config = writeable(&workspace);
    let ctx = context(&workspace, &config, Mode::Ask).await;

    let error = call("apply_patch", json!({ "patch": REWRITE }), &ctx)
        .await
        .expect_err("ask mode must refuse writes");

    assert!(matches!(error, RaiError::PolicyDenied { .. }), "{error:?}");
    assert!(error
        .to_string()
        .contains("does not allow workspace writes"));
    assert!(workspace.read("src/main.rs").contains("let x = 1;"));
}

#[tokio::test]
async fn paths_outside_the_workspace_are_refused() {
    let workspace = Workspace::new();
    let config = writeable(&workspace);
    let ctx = context(&workspace, &config, Mode::Edit).await;

    let escape = "--- a/../../escaped.txt\n+++ b/../../escaped.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n";
    let error = call("apply_patch", json!({ "patch": escape }), &ctx)
        .await
        .expect_err("escaping paths must be refused");
    assert_eq!(error.kind(), "path_escape");

    let absolute = "--- a/C:/Windows/System32/drivers/etc/hosts\n+++ b/C:/Windows/System32/drivers/etc/hosts\n@@ -1,1 +1,1 @@\n-a\n+b\n";
    let error = call("apply_patch", json!({ "patch": absolute }), &ctx)
        .await
        .expect_err("absolute paths must be refused");
    assert!(matches!(
        error,
        RaiError::PathEscape { .. } | RaiError::AbsolutePath(_)
    ));
}

#[tokio::test]
async fn a_patch_that_does_not_match_is_reported_not_guessed() {
    let workspace = Workspace::new();
    workspace.write(
        "src/main.rs",
        "fn main() {\n    let totally_different = 9;\n}\n",
    );
    let config = writeable(&workspace);
    let ctx = context(&workspace, &config, Mode::Edit).await;

    let error = call("apply_patch", json!({ "patch": REWRITE }), &ctx)
        .await
        .expect_err("unmatched hunks must fail");
    assert_eq!(error.kind(), "patch");
    assert!(error.to_string().contains("did not match"));
    assert!(workspace.read("src/main.rs").contains("totally_different"));
}

#[tokio::test]
async fn files_with_uncommitted_changes_are_flagged() {
    let workspace = Workspace::new();
    workspace.write("src/main.rs", "fn main() {\n    let x = 1;\n}\n");
    workspace.git_init();
    // Dirty the file before the assistant touches it.
    workspace.write(
        "src/main.rs",
        "fn main() {\n    let x = 1;\n    let y = 3;\n}\n",
    );

    let config = writeable(&workspace);
    let ctx = context(&workspace, &config, Mode::Edit).await;
    let patch = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,3 +1,3 @@\n fn main() {\n     let x = 1;\n-    let y = 3;\n+    let y = 4;\n }\n";

    let outcome = call("apply_patch", json!({ "patch": patch }), &ctx)
        .await
        .expect("patch applies");

    let flagged = outcome.data.as_ref().unwrap()["already_modified"]
        .as_array()
        .expect("already_modified is a list");
    assert_eq!(flagged.len(), 1);
    assert!(outcome.text.contains("uncommitted changes"));
}

#[tokio::test]
async fn read_file_is_relative_and_rejects_binaries() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "line one\nline two\nline three\n");
    workspace.write(".rai/binary.bin", "text\0more");
    let config = workspace.config();
    let ctx = context(&workspace, &config, Mode::Ask).await;

    let outcome = call("read_file", json!({ "path": "src/lib.rs" }), &ctx)
        .await
        .expect("reads a text file");
    assert!(outcome.text.contains("path: src/lib.rs"));
    assert!(outcome.text.contains("line two"));

    let error = call("read_file", json!({ "path": "../../etc/passwd" }), &ctx)
        .await
        .expect_err("escape refused");
    assert_eq!(error.kind(), "path_escape");

    let error = call("read_file", json!({ "path": ".rai/binary.bin" }), &ctx)
        .await
        .expect_err("binary refused");
    assert_eq!(error.kind(), "not_text");
}

#[tokio::test]
async fn search_and_symbol_tools_work_from_the_index() {
    let workspace = Workspace::new();
    workspace.write(
        "src/engine.rs",
        "pub struct Engine;\npub fn evaluate() -> bool { true }\n",
    );
    let config = workspace.config();
    let ctx = context(&workspace, &config, Mode::Ask).await;

    let search = call("search_text", json!({ "query": "pub fn evaluate" }), &ctx)
        .await
        .expect("search runs");
    assert!(search.text.contains("src/engine.rs:2"), "{}", search.text);

    let symbols = call("list_symbols", json!({ "query": "Engine" }), &ctx)
        .await
        .expect("symbol lookup runs");
    assert!(symbols.text.contains("Engine"), "{}", symbols.text);
    assert!(symbols.text.contains("type"), "{}", symbols.text);

    let files = call("list_files", json!({ "glob": "*.rs" }), &ctx)
        .await
        .expect("listing runs");
    assert!(files.text.contains("src/engine.rs"));

    let none = call("list_files", json!({ "glob": "*.py" }), &ctx)
        .await
        .expect("listing runs");
    assert!(none.text.contains("0 file(s) matched"), "{}", none.text);
}

/// Commands are re-checked inside the tool, so a policy that denies commands
/// cannot be bypassed by the model asking for one directly.
#[tokio::test]
async fn run_command_is_gated_by_the_command_policy() {
    let workspace = Workspace::new();
    let config = workspace.config_with(|config| {
        config.policy.approval = "deny".to_string();
        config.commands.auto_allow = vec!["cargo --version".to_string()];
    });
    let ctx = context(&workspace, &config, Mode::Edit).await;

    let denied = call(
        "run_command",
        json!({ "argv": ["git", "reset", "--hard"] }),
        &ctx,
    )
    .await
    .expect_err("not allowlisted");
    assert!(
        matches!(denied, RaiError::PolicyDenied { .. }),
        "{denied:?}"
    );

    let allowed = call(
        "run_command",
        json!({ "argv": ["cargo", "--version"] }),
        &ctx,
    )
    .await
    .expect("allowlisted");
    assert!(allowed.text.contains("exit_code: 0") || allowed.text.contains("cargo"));
}

#[tokio::test]
async fn summarize_diff_and_explain_failure_are_deterministic() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn ok() {}\n");
    workspace.git_init();
    workspace.write("src/lib.rs", "pub fn ok() {}\npub fn added() {}\n");

    let config = workspace.config();
    let ctx = context(&workspace, &config, Mode::Review).await;

    let summary = call("summarize_diff", json!({}), &ctx)
        .await
        .expect("diff summary runs");
    assert!(summary.text.contains("src/lib.rs"), "{}", summary.text);
    assert!(summary.text.contains("added"), "{}", summary.text);

    let diagnosis = call(
        "explain_failure",
        json!({ "output": "error[E0308]: mismatched types\n  --> src/main.rs:42:17\n" }),
        &ctx,
    )
    .await
    .expect("diagnosis runs");
    assert!(
        diagnosis.text.contains("compile-error"),
        "{}",
        diagnosis.text
    );
    assert!(
        diagnosis.text.contains("src/main.rs:42:17"),
        "{}",
        diagnosis.text
    );
}
