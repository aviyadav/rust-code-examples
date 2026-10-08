//! MCP server: expose policy-aware project tools to another client.
//!
//! The surface is deliberately narrow. `project.apply_patch` is better than
//! `write_any_file`; `project.run_tests` is safer than `shell`;
//! `project.read_file` is safer than `read_absolute_path`. MCP standardizes
//! communication; it does not replace authorization design.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::{code, failure, initialize_result, success, JsonRpcRequest, JsonRpcResponse};
use crate::config::Config;
use crate::error::{RaiError, Result};
use crate::events::{Emitter, OutputMode};
use crate::index::{IndexOptions, RepoIndex};
use crate::policy::PolicyEngine;
use crate::redact::Redactor;
use crate::tools::{self, Mode, ToolContext, ToolDefinition};
use crate::util::{install_ctrlc, Cancel};
use crate::VERSION;

/// Public MCP tool name paired with the internal tool that implements it.
pub const SERVER_TOOLS: &[(&str, &str)] = &[
    ("project.search", "search_text"),
    ("project.read_file", "read_file"),
    ("project.apply_patch", "apply_patch"),
    ("project.run_tests", "run_tests"),
    ("project.summarize_diff", "summarize_diff"),
    ("project.explain_failure", "explain_failure"),
];

/// Map a public MCP tool name onto the internal tool name.
///
/// Bare internal names are accepted too, so a client that was configured with
/// `search_text` is not penalized for skipping the namespace.
pub fn internal_name(public: &str) -> Option<&'static str> {
    SERVER_TOOLS
        .iter()
        .find(|(public_name, _)| *public_name == public)
        .map(|(_, internal)| *internal)
        .or_else(|| {
            SERVER_TOOLS
                .iter()
                .find(|(_, internal)| *internal == public)
                .map(|(_, internal)| *internal)
        })
}

/// Serve MCP over stdin/stdout until the stream closes.
///
/// Nothing except protocol messages is written to stdout; diagnostics go to
/// stderr so they cannot corrupt a client's parse.
pub async fn serve(config: Arc<Config>) -> Result<()> {
    let root = config.workspace_root();
    let cancel = Cancel::new();
    install_ctrlc(cancel.clone());

    let index = load_or_build_index(&root, &config).await?;
    let policy = Arc::new(server_policy(&config)?);

    let emitter = Arc::new(
        Emitter::new(OutputMode::Json, true, "mcp")
            .with_log_file(&config.metadata_dir().join("logs").join("mcp.jsonl"))
            .mirror_into(Arc::new(crate::session::SessionStore::new(
                &config.metadata_dir(),
            ))),
    );

    let ctx = ToolContext {
        root: root.clone(),
        config: Arc::clone(&config),
        policy: Arc::clone(&policy),
        index,
        emitter,
        redactor: Arc::new(Redactor::from_env()),
        cancel,
        session: "mcp".to_string(),
        mode: Mode::Edit,
        dry_run: false,
        commands: Arc::new(std::sync::Mutex::new(Vec::new())),
        patches: Arc::new(std::sync::Mutex::new(Vec::new())),
    };

    let registry = tools::builtin();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => handle_request(request, &ctx, &registry).await,
            Err(error) => Some(failure(
                None,
                code::PARSE_ERROR,
                format!("invalid JSON-RPC request: {error}"),
            )),
        };
        if let Some(response) = response {
            let payload = serde_json::to_vec(&response)?;
            stdout.write_all(&payload).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

/// Dispatch one request. `None` means "notification, no reply".
async fn handle_request(
    request: JsonRpcRequest,
    ctx: &ToolContext,
    registry: &tools::ToolRegistry,
) -> Option<JsonRpcResponse> {
    let id = request.id;
    match request.method.as_str() {
        "initialize" => Some(success(id, initialize_result("rai", VERSION))),
        "ping" => Some(success(id, json!({}))),
        "tools/list" => Some(success(id, json!({ "tools": tool_listing(registry) }))),
        "tools/call" => Some(call_tool(id, request.params, ctx, registry).await),
        "resources/list" => Some(success(id, json!({ "resources": [] }))),
        "prompts/list" => Some(success(id, json!({ "prompts": [] }))),
        "notifications/initialized" | "notifications/cancelled" | "notifications/progress" => None,
        other => Some(failure(
            id,
            code::METHOD_NOT_FOUND,
            format!("unsupported method `{other}`"),
        )),
    }
}

/// Describe the exposed tools, reusing each internal tool's own schema.
fn tool_listing(registry: &tools::ToolRegistry) -> Vec<Value> {
    SERVER_TOOLS
        .iter()
        .filter_map(|(public, internal)| {
            let definition = registry.definition(internal)?;
            let mut description = definition.description.clone();
            description.push_str(" Exposed by rai as a policy-aware project tool.");
            Some(json!({
                "name": public,
                "description": description,
                "inputSchema": definition.schema,
            }))
        })
        .collect()
}

/// Handle `tools/call`.
///
/// Policy and execution failures are returned as `isError` content, not as
/// JSON-RPC errors: the caller asked a well-formed question and deserves a
/// readable answer.
async fn call_tool(
    id: Option<u64>,
    params: Value,
    ctx: &ToolContext,
    registry: &tools::ToolRegistry,
) -> JsonRpcResponse {
    let Some(public) = params.get("name").and_then(|v| v.as_str()) else {
        return failure(id, code::INVALID_PARAMS, "tools/call requires `name`");
    };
    let Some(internal) = internal_name(public) else {
        return failure(
            id,
            code::INVALID_PARAMS,
            format!(
                "unknown tool `{public}` (available: {})",
                SERVER_TOOLS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let Some(definition) = registry.definition(internal) else {
        return failure(
            id,
            code::INTERNAL_ERROR,
            format!("internal tool `{internal}` is missing"),
        );
    };
    if !tools::tool_available(Mode::Edit, true, &definition) {
        return success(
            id,
            tool_result(
                &format!("tool `{public}` is not part of the exposed surface"),
                true,
                None,
            ),
        );
    }

    // Policy is evaluated against the *public* name so the audit log reads the
    // way a client's configuration reads.
    let public_definition = ToolDefinition {
        name: public.to_string(),
        ..definition.clone()
    };
    match ctx.policy.evaluate(&public_definition, &arguments) {
        crate::policy::Decision::Denied { reason } => {
            return success(
                id,
                tool_result(&format!("policy denied `{public}`: {reason}"), true, None),
            )
        }
        crate::policy::Decision::NeedsApproval { reason } => {
            return success(
                id,
                tool_result(
                    &format!(
                        "`{public}` needs approval, which this server cannot ask for: {reason}. \
                         Configure [policy].auto_approve to allow it explicitly."
                    ),
                    true,
                    None,
                ),
            )
        }
        crate::policy::Decision::Allowed { .. } => {}
    }

    let Some(tool) = registry.get(internal) else {
        return failure(
            id,
            code::INTERNAL_ERROR,
            format!("internal tool `{internal}` is not registered"),
        );
    };

    match tool.execute(arguments, ctx, public).await {
        Ok(outcome) => success(id, tool_result(&outcome.text, false, outcome.data.clone())),
        Err(error) => success(id, tool_result(&format!("{error}"), true, None)),
    }
}

/// Build the MCP tool result envelope.
fn tool_result(text: &str, is_error: bool, data: Option<Value>) -> Value {
    let mut value = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    if let Some(data) = data {
        if let Some(object) = value.as_object_mut() {
            object.insert("structuredContent".to_string(), data);
        }
    }
    value
}

/// Policy for a server context.
///
/// Interactive approval is impossible here: the "user" on the other end is a
/// program. Prompt mode is therefore downgraded to deny, loudly, rather than
/// silently reading the protocol stream.
fn server_policy(config: &Config) -> Result<PolicyEngine> {
    let mut effective = config.clone();
    if effective.policy.approval.eq_ignore_ascii_case("prompt") {
        eprintln!(
            "rai mcp: [policy].approval = \"prompt\" cannot be honored without a terminal; \
             treating it as \"deny\". Set [policy].auto_approve to allow specific risk classes."
        );
        effective.policy.approval = "deny".to_string();
    }
    PolicyEngine::from_config(&effective)
}

/// Reuse the on-disk index when it belongs to this workspace, else rebuild.
async fn load_or_build_index(root: &std::path::Path, config: &Config) -> Result<Arc<RepoIndex>> {
    let path = config.metadata_dir().join("indexes").join("index.json");
    if let Ok(index) = RepoIndex::load(&path) {
        let normalize = |value: &str| value.replace('\\', "/");
        if normalize(&index.root) == normalize(&root.to_string_lossy()) {
            return Ok(Arc::new(index));
        }
    }
    let options = IndexOptions {
        excludes: config.workspace.exclude.clone(),
        max_files: config.search.max_indexed_files,
        max_scan_bytes: config.workspace.max_file_bytes.min(512 * 1024),
    };
    let root: PathBuf = root.to_path_buf();
    Ok(Arc::new(RepoIndex::build_async(root, options).await?))
}

impl From<RaiError> for JsonRpcResponse {
    fn from(error: RaiError) -> Self {
        failure(None, code::INTERNAL_ERROR, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_public_names() {
        assert_eq!(internal_name("project.search"), Some("search_text"));
        assert_eq!(internal_name("project.read_file"), Some("read_file"));
        assert_eq!(internal_name("project.apply_patch"), Some("apply_patch"));
        assert_eq!(internal_name("project.run_tests"), Some("run_tests"));
        assert_eq!(
            internal_name("project.summarize_diff"),
            Some("summarize_diff")
        );
        assert_eq!(
            internal_name("project.explain_failure"),
            Some("explain_failure")
        );
        assert_eq!(internal_name("project.nope"), None);
    }

    #[test]
    fn accepts_bare_internal_names() {
        assert_eq!(internal_name("search_text"), Some("search_text"));
        assert_eq!(internal_name("read_file"), Some("read_file"));
    }

    #[test]
    fn listing_covers_every_server_tool() {
        let registry = tools::builtin();
        let listing = tool_listing(&registry);
        assert_eq!(listing.len(), SERVER_TOOLS.len());
        for entry in &listing {
            assert!(entry["name"].as_str().unwrap().starts_with("project."));
            assert_eq!(entry["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn error_results_are_flagged() {
        let value = tool_result("denied", true, None);
        assert_eq!(value["isError"], true);
        assert_eq!(value["content"][0]["text"], "denied");
    }

    #[test]
    fn structured_data_is_preserved() {
        let value = tool_result("ok", false, Some(json!({"files": 2})));
        assert_eq!(value["structuredContent"]["files"], 2);
    }

    #[test]
    fn prompt_mode_is_downgraded_for_servers() {
        let mut config = Config::default();
        config.policy.approval = "prompt".into();
        let policy = server_policy(&config).unwrap();
        assert_eq!(policy.approval().as_str(), "deny");
    }
}
