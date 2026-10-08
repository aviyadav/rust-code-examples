//! MCP client: connect to configured stdio servers and call their tools.
//!
//! The client keeps one reader task per server and routes responses to waiting
//! callers by request id, so a slow server cannot block unrelated work.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use super::JsonRpcRequest;
use crate::config::{Config, McpServerConfig};
use crate::error::{RaiError, Result};
use crate::VERSION;

/// A tool advertised by an MCP server.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// The result of `tools/call`.
#[derive(Debug, Clone)]
pub struct McpCallResult {
    pub text: String,
    pub is_error: bool,
    pub content: Value,
}

/// Response channel for one in-flight request.
type PendingSender = oneshot::Sender<std::result::Result<Value, String>>;

/// A connected MCP server.
pub struct McpClient {
    name: String,
    child: Mutex<Option<Child>>,
    stdin: AsyncMutex<ChildStdin>,
    pending: Arc<Mutex<HashMap<u64, PendingSender>>>,
    next_id: AtomicU64,
    tools: Mutex<Vec<McpTool>>,
    timeout: Duration,
    server_info: Mutex<Option<Value>>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("name", &self.name)
            .field(
                "pending",
                &self.pending.lock().map(|p| p.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl McpClient {
    /// Start a server and begin reading its responses.
    ///
    /// The child is killed when this client is dropped, so a crashed run cannot
    /// leave orphaned servers behind.
    pub async fn spawn(config: &McpServerConfig) -> Result<Arc<Self>> {
        if config.transport != "stdio" {
            return Err(RaiError::Mcp(format!(
                "MCP server `{}` uses transport `{}`; only stdio is supported",
                config.name, config.transport
            )));
        }

        let mut command = tokio::process::Command::new(&config.command);
        command
            .args(&config.args)
            .envs(config.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|e| {
            RaiError::Mcp(format!(
                "cannot start MCP server `{}` ({} {}): {e}",
                config.name,
                config.command,
                config.args.join(" ")
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RaiError::Mcp(format!("MCP server `{}` has no stdin", config.name)))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RaiError::Mcp(format!("MCP server `{}` has no stdout", config.name)))?;

        let pending: Arc<Mutex<HashMap<u64, PendingSender>>> = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        let server_name = config.name.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(id) = value.get("id").and_then(|v| v.as_u64()) else {
                    // A notification from the server; nothing to route.
                    continue;
                };
                let sender = reader_pending
                    .lock()
                    .ok()
                    .and_then(|mut map| map.remove(&id));
                let Some(sender) = sender else { continue };
                let outcome = match value.get("error") {
                    Some(error) => Err(format!(
                        "{} error {}: {}",
                        server_name,
                        error.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                        error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown error")
                    )),
                    None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = sender.send(outcome);
            }
        });

        Ok(Arc::new(Self {
            name: config.name.clone(),
            child: Mutex::new(Some(child)),
            stdin: AsyncMutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            tools: Mutex::new(Vec::new()),
            timeout: Duration::from_secs(config.timeout_seconds.max(1)),
            server_info: Mutex::new(None),
        }))
    }

    /// Server name from configuration.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Tools discovered so far.
    pub fn tools(&self) -> Vec<McpTool> {
        self.tools.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// `serverInfo` returned by `initialize`.
    pub fn server_info(&self) -> Option<Value> {
        self.server_info.lock().ok().and_then(|info| info.clone())
    }

    /// Send a request and wait for its response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| RaiError::Mcp("client state poisoned".into()))?;
            pending.insert(id, sender);
        }

        let payload = serde_json::to_vec(&JsonRpcRequest::call(id, method, params))?;
        {
            let mut stdin = self.stdin.lock().await;
            let write = async {
                stdin.write_all(&payload).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            .await;
            if let Err(error) = write {
                self.forget(id);
                return Err(RaiError::Mcp(format!(
                    "cannot write to MCP server `{}`: {error}",
                    self.name
                )));
            }
        }

        match tokio::time::timeout(self.timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(RaiError::Mcp(message)),
            Ok(Err(_)) => Err(RaiError::Mcp(format!(
                "MCP server `{}` closed the connection during `{method}`",
                self.name
            ))),
            Err(_) => {
                self.forget(id);
                Err(RaiError::Mcp(format!(
                    "MCP server `{}` did not answer `{method}` within {}s",
                    self.name,
                    self.timeout.as_secs()
                )))
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
    }

    /// Send a one-way notification.
    pub async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let payload = serde_json::to_vec(&JsonRpcRequest::notification(method, params))?;
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&payload).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Perform the MCP handshake.
    pub async fn initialize(&self) -> Result<Value> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": crate::MCP_PROTOCOL_VERSION,
                    "capabilities": { "roots": {}, "sampling": {} },
                    "clientInfo": { "name": "rai", "version": VERSION }
                }),
            )
            .await?;
        self.notify("notifications/initialized", Value::Null)
            .await?;
        if let Ok(mut info) = self.server_info.lock() {
            *info = result.get("serverInfo").cloned();
        }
        Ok(result)
    }

    /// Discover the server's tools and cache them.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = parse_tools(&result);
        if let Ok(mut cache) = self.tools.lock() {
            *cache = tools.clone();
        }
        Ok(tools)
    }

    /// List the server's resources.
    pub async fn list_resources(&self) -> Result<Value> {
        self.request("resources/list", json!({})).await
    }

    /// List the server's prompt templates.
    pub async fn list_prompts(&self) -> Result<Value> {
        self.request("prompts/list", json!({})).await
    }

    /// Call one tool.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<McpCallResult> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        Ok(McpCallResult {
            text: flatten_content(&result),
            is_error: result
                .get("isError")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            content: result.get("content").cloned().unwrap_or(Value::Null),
        })
    }

    /// Ask the server to stop, then make sure the child is gone.
    pub async fn shutdown(&self) {
        let _ = self.notify("notifications/cancelled", json!({})).await;
        let child = self.child.lock().ok().and_then(|mut c| c.take());
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// Extract tool definitions from a `tools/list` result.
fn parse_tools(result: &Value) -> Vec<McpTool> {
    result
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    let name = tool.get("name")?.as_str()?.to_string();
                    Some(McpTool {
                        description: tool
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        schema: tool
                            .get("inputSchema")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                        name,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Flatten `content` blocks into text for the model.
fn flatten_content(result: &Value) -> String {
    let Some(blocks) = result.get("content").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|v| v.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    parts.push(text.to_string());
                }
            }
            Some(other) => parts.push(format!("[{other} content]")),
            None => {}
        }
    }
    parts.join("\n")
}

/// Connect every enabled server from configuration.
///
/// Connection failures are reported, not fatal: a broken optional server must
/// not stop a run.
#[allow(dead_code)]
pub async fn connect_all(config: &Config) -> (Vec<Arc<McpClient>>, Vec<String>) {
    let mut clients = Vec::new();
    let mut problems = Vec::new();
    for server in config.mcp.servers.iter().filter(|s| s.enabled) {
        match McpClient::spawn(server).await {
            Ok(client) => {
                if let Err(error) = client.initialize().await {
                    problems.push(format!("mcp server `{}`: {error}", server.name));
                    client.shutdown().await;
                    continue;
                }
                if let Err(error) = client.list_tools().await {
                    problems.push(format!("mcp server `{}`: {error}", server.name));
                }
                clients.push(client);
            }
            Err(error) => problems.push(error.to_string()),
        }
    }
    (clients, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_definitions() {
        let result = json!({
            "tools": [
                {"name": "read", "description": "read a file", "inputSchema": {"type": "object"}},
                {"name": "write"}
            ]
        });
        let tools = parse_tools(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "read");
        assert_eq!(tools[0].description, "read a file");
        assert_eq!(tools[1].schema["type"], "object");
    }

    #[test]
    fn parses_an_empty_tool_list() {
        assert!(parse_tools(&json!({})).is_empty());
        assert!(parse_tools(&json!({"tools": []})).is_empty());
    }

    #[test]
    fn flattens_text_content_blocks() {
        let result = json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "text", "text": "second"}
            ]
        });
        assert_eq!(flatten_content(&result), "first\nsecond");
    }

    #[test]
    fn non_text_content_is_labelled() {
        let result = json!({"content": [{"type": "image", "data": "..."}]});
        assert_eq!(flatten_content(&result), "[image content]");
    }
}
