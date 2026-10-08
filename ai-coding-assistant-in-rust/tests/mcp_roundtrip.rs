//! MCP round trip: this crate's client against the real `rai mcp-serve` binary.
//!
//! The point is not that MCP "works" in the abstract. It is that an external
//! client reaches `project.apply_patch` through the same policy engine, so a
//! refusal is a refusal no matter who is asking.

mod common;
use common::{rai_binary, Workspace};

use std::collections::BTreeMap;

use rai::config::McpServerConfig;
use rai::mcp::McpClient;
use serde_json::json;

const ALLOW_WRITES: &str = r#"
[model]
provider = "local"
model = "local"

[workspace]
root = "."

[commands]
auto_allow = ["cargo --version"]

[policy]
approval = "auto"
auto_approve = ["WorkspaceWrite", "Command"]
"#;

const DENY_WRITES: &str = r#"
[model]
provider = "local"
model = "local"

[workspace]
root = "."

[policy]
approval = "deny"
"#;

/// Describe the local `rai mcp-serve` server for a workspace.
fn server(workspace: &Workspace) -> McpServerConfig {
    McpServerConfig {
        name: "rai".to_string(),
        transport: "stdio".to_string(),
        command: rai_binary().to_string_lossy().into_owned(),
        args: vec![
            "--config".to_string(),
            workspace
                .path(".rai/config.toml")
                .to_string_lossy()
                .into_owned(),
            "mcp-serve".to_string(),
        ],
        env: BTreeMap::new(),
        enabled: true,
        timeout_seconds: 120,
    }
}

const LIB_RS: &str = "pub struct Alpha;\npub fn beta() -> usize { 1 }\n";

const PATCH: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n pub struct Alpha;\n+pub struct Beta;\n pub fn beta() -> usize { 1 }\n";

#[tokio::test]
async fn handshake_lists_the_project_tool_surface() {
    let workspace = Workspace::new();
    workspace.write_config(ALLOW_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace))
        .await
        .expect("server starts");
    let info = client.initialize().await.expect("handshake");
    assert_eq!(info["serverInfo"]["name"], "rai");

    let tools = client.list_tools().await.expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"project.search"), "{names:?}");
    assert!(names.contains(&"project.read_file"), "{names:?}");
    assert!(names.contains(&"project.apply_patch"), "{names:?}");
    assert!(names.contains(&"project.run_tests"), "{names:?}");
    assert!(names.contains(&"project.summarize_diff"), "{names:?}");
    assert!(names.contains(&"project.explain_failure"), "{names:?}");

    client.shutdown().await;
}

#[tokio::test]
async fn search_and_read_return_repository_content() {
    let workspace = Workspace::new();
    workspace.write_config(ALLOW_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace)).await.expect("spawn");
    client.initialize().await.expect("handshake");

    let search = client
        .call_tool("project.search", json!({ "query": "Alpha" }))
        .await
        .expect("search runs");
    assert!(!search.is_error, "{}", search.text);
    assert!(search.text.contains("src/lib.rs"), "{}", search.text);

    let read = client
        .call_tool("project.read_file", json!({ "path": "src/lib.rs" }))
        .await
        .expect("read runs");
    assert!(!read.is_error, "{}", read.text);
    assert!(read.text.contains("pub struct Alpha"), "{}", read.text);

    client.shutdown().await;
}

#[tokio::test]
async fn apply_patch_writes_when_policy_allows_it() {
    let workspace = Workspace::new();
    workspace.write_config(ALLOW_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace)).await.expect("spawn");
    client.initialize().await.expect("handshake");

    let result = client
        .call_tool("project.apply_patch", json!({ "patch": PATCH }))
        .await
        .expect("call returns");
    assert!(!result.is_error, "{}", result.text);
    assert!(
        workspace.read("src/lib.rs").contains("pub struct Beta;"),
        "file was not patched"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn apply_patch_is_refused_when_policy_denies_it() {
    let workspace = Workspace::new();
    workspace.write_config(DENY_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace)).await.expect("spawn");
    client.initialize().await.expect("handshake");

    let result = client
        .call_tool("project.apply_patch", json!({ "patch": PATCH }))
        .await
        .expect("call returns a tool-level error, not a protocol error");

    assert!(result.is_error, "expected a refusal, got: {}", result.text);
    assert_eq!(
        workspace.read("src/lib.rs"),
        LIB_RS,
        "a refused patch must not touch the file"
    );

    client.shutdown().await;
}

#[tokio::test]
async fn run_tests_goes_through_the_sandbox() {
    let workspace = Workspace::new();
    workspace.write_config(ALLOW_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace)).await.expect("spawn");
    client.initialize().await.expect("handshake");

    let result = client
        .call_tool(
            "project.run_tests",
            json!({ "argv": ["cargo", "--version"] }),
        )
        .await
        .expect("call returns");
    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("cargo"), "{}", result.text);

    client.shutdown().await;
}

#[tokio::test]
async fn explain_failure_is_deterministic_and_offline() {
    let workspace = Workspace::new();
    workspace.write_config(ALLOW_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace)).await.expect("spawn");
    client.initialize().await.expect("handshake");

    let result = client
        .call_tool(
            "project.explain_failure",
            json!({ "output": "--- FAIL: TestLogin (0.00s)\n    auth_test.go:12: got 401" }),
        )
        .await
        .expect("call returns");
    assert!(!result.is_error, "{}", result.text);
    assert!(result.text.contains("test-failure"), "{}", result.text);
    assert!(result.text.contains("TestLogin"), "{}", result.text);

    client.shutdown().await;
}

#[tokio::test]
async fn an_unknown_tool_is_a_protocol_error() {
    let workspace = Workspace::new();
    workspace.write_config(ALLOW_WRITES);
    workspace.write("src/lib.rs", LIB_RS);

    let client = McpClient::spawn(&server(&workspace)).await.expect("spawn");
    client.initialize().await.expect("handshake");

    let error = client
        .call_tool("project.delete_everything", json!({}))
        .await
        .expect_err("unknown tools must be rejected");
    assert!(error.to_string().contains("unknown tool"), "{error}");

    client.shutdown().await;
}
