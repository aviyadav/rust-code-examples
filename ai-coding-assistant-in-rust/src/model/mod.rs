//! Provider-agnostic model interface.
//!
//! LLM APIs change quickly. The core loop depends on this internal interface,
//! never on one provider's request type, so adapters can be replaced without
//! touching policy, tools, or the agent.

pub mod local;
pub mod openai;

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, Provider};
use crate::error::{RaiError, Result};
use crate::tools::{Mode, ToolSpec};

/// Message author.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// A tool call requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// One conversation message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::plain(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::plain(Role::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::plain(Role::Assistant, text)
    }

    /// Assistant message carrying tool calls.
    pub fn assistant_calls(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    /// Tool result, matched to the originating call id.
    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }

    fn plain(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// What the loop asks a provider for.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: Option<u32>,
}

impl ModelRequest {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_output_tokens(mut self, tokens: u32) -> Self {
        self.max_output_tokens = Some(tokens);
        self
    }
}

/// Token accounting, when a provider reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// One model turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Option<TokenUsage>,
    pub provider: String,
    pub model: String,
}

impl ModelResponse {
    /// A response with no tool calls ends the loop.
    pub fn is_final(&self) -> bool {
        self.tool_calls.is_empty()
    }

    /// Bounded one-line summary for the event stream.
    pub fn summary(&self) -> String {
        let calls: Vec<&str> = self.tool_calls.iter().map(|c| c.name.as_str()).collect();
        if calls.is_empty() {
            format!("{} chars of text", self.text.len())
        } else {
            format!("{} tool call(s): {}", calls.len(), calls.join(", "))
        }
    }
}

/// A client for one provider.
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Provider name, as configured.
    fn provider(&self) -> &'static str;

    /// Model identifier, as configured.
    fn model(&self) -> &str;

    /// Whether the provider can call tools natively.
    fn supports_tools(&self) -> bool {
        true
    }

    /// One non-streaming turn.
    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse>;

    /// One streaming turn. The default forwards the final text as a delta.
    async fn stream(
        &self,
        request: ModelRequest,
        // Higher-ranked so the borrowed chunk is not tied to the trait object's
        // own lifetime.
        sink: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ModelResponse> {
        let response = self.respond(request).await?;
        if !response.text.is_empty() {
            // Clone so the caller keeps ownership of the response.
            let text = response.text.clone();
            sink(&text);
        }
        Ok(response)
    }
}

/// Build the configured client.
///
/// The local and scripted providers are deterministic and offline; they exist so
/// the governance layer can be exercised without a network or a key.
pub fn build(config: &Config, mode: Mode) -> Result<Arc<dyn ModelClient>> {
    match config.provider()? {
        Provider::Openai => Ok(Arc::new(openai::OpenAiClient::from_config(config)?)),
        Provider::Local => Ok(Arc::new(local::LocalModel::new(mode))),
        Provider::Scripted => {
            let path = config.model.script.clone().ok_or_else(|| {
                RaiError::Config("[model].script is required when provider = \"scripted\"".into())
            })?;
            let resolved = if path.is_absolute() {
                path
            } else {
                config.base_dir.join(path)
            };
            Ok(Arc::new(local::ScriptedModel::load(&resolved)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_constructors_round_trip_through_serde() {
        let message = Message::assistant_calls(
            "thinking",
            vec![ToolCall::new("c1", "read_file", json!({"path": "a.rs"}))],
        );
        let text = serde_json::to_string(&message).unwrap();
        let back: Message = serde_json::from_str(&text).unwrap();
        assert_eq!(back.role, Role::Assistant);
        assert_eq!(back.tool_calls[0].name, "read_file");
    }

    #[test]
    fn tool_result_carries_the_call_id() {
        let message = Message::tool_result("c9", "content");
        assert_eq!(message.role, Role::Tool);
        assert_eq!(message.tool_call_id.as_deref(), Some("c9"));
    }

    #[test]
    fn final_response_has_no_tool_calls() {
        let response = ModelResponse {
            text: "done".into(),
            tool_calls: vec![],
            usage: None,
            provider: "local".into(),
            model: "local".into(),
        };
        assert!(response.is_final());
        assert!(response.summary().contains("chars of text"));
    }

    #[test]
    fn role_parsing_is_lenient() {
        assert_eq!(Role::parse("ASSISTANT"), Some(Role::Assistant));
        assert_eq!(Role::parse(" tool "), Some(Role::Tool));
        assert_eq!(Role::parse("nope"), None);
    }
}
