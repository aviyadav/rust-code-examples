//! Model Context Protocol, both roles.
//!
//! `rai` is an MCP *client* when it connects to configured servers, and an MCP
//! *server* when it exposes its own policy-aware project tools. Both sit behind
//! the same tool registry and policy engine, so an external client cannot reach
//! capabilities this process would not allow itself.

pub mod client;
pub mod server;

pub use client::{McpCallResult, McpClient, McpTool};
pub use server::serve;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::MCP_PROTOCOL_VERSION;

/// JSON-RPC 2.0 request or notification (`id` absent for notifications).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcRequest {
    pub fn call(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id: Some(id),
            method: method.into(),
            params,
        }
    }

    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id: None,
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Standard JSON-RPC error codes.
pub mod code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

pub fn jsonrpc_version() -> String {
    "2.0".to_string()
}

/// Build a success response.
pub fn success(id: Option<u64>, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: jsonrpc_version(),
        id,
        result: Some(result),
        error: None,
    }
}

/// Build an error response.
pub fn failure(id: Option<u64>, code: i64, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: jsonrpc_version(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

/// The `initialize` result payload both roles exchange.
pub fn initialize_result(name: &str, version: &str) -> Value {
    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "listChanged": false },
            "prompts": { "listChanged": false }
        },
        "serverInfo": { "name": name, "version": version }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_have_no_id_field_on_the_wire() {
        let notification = JsonRpcRequest::notification("notifications/initialized", Value::Null);
        let text = serde_json::to_string(&notification).unwrap();
        assert!(!text.contains("\"id\""));
        assert!(text.contains("notifications/initialized"));
    }

    #[test]
    fn responses_omit_empty_fields() {
        let response = success(Some(1), Value::Null);
        let text = serde_json::to_string(&response).unwrap();
        assert!(text.contains("\"result\":null"));
        assert!(!text.contains("error"));
    }

    #[test]
    fn errors_carry_a_code_and_message() {
        let response = failure(Some(2), code::METHOD_NOT_FOUND, "no such method");
        assert_eq!(response.error.unwrap().code, -32601);
        assert_eq!(response.id, Some(2));
    }

    #[test]
    fn initialize_advertises_the_protocol_version() {
        let value = initialize_result("rai", "0.1.0");
        assert_eq!(value["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(value["serverInfo"]["name"], "rai");
    }
}
