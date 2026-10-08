//! OpenAI-compatible adapter.
//!
//! Works with OpenAI and any `/chat/completions` server that speaks the same
//! shape: Ollama, LM Studio, vLLM, and hosted gateways. The adapter stays thin
//! on purpose — normalized semantics belong in the internal types, not here.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};

use super::{Message, ModelClient, ModelRequest, ModelResponse, Role, TokenUsage, ToolCall};
use crate::config::Config;
use crate::error::{RaiError, Result};

const MAX_ERROR_BODY: usize = 600;

/// A client for any OpenAI-compatible chat completions endpoint.
pub struct OpenAiClient {
    client: Client,
    base_url: String,
    model: String,
    api_key: Option<String>,
    api_key_env: String,
    temperature: f32,
    stream: bool,
}

impl OpenAiClient {
    /// Build from configuration. The API key is read from the environment,
    /// never from the config file.
    pub fn from_config(config: &Config) -> Result<Self> {
        let base_url = config.model.base_url.trim().to_string();
        if base_url.is_empty() {
            return Err(RaiError::Config(
                "[model].base_url must not be empty for provider = \"openai\"".into(),
            ));
        }
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(RaiError::Config(format!(
                "[model].base_url must be an http(s) URL, got `{base_url}`"
            )));
        }

        let api_key = std::env::var(&config.model.api_key_env)
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());

        let client = Client::builder()
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(20))
            .user_agent(format!("rai/{}", crate::VERSION))
            .build()
            .map_err(|e| RaiError::Model(format!("cannot build the HTTP client: {e}")))?;

        Ok(Self {
            client,
            base_url,
            model: config.model.model.clone(),
            api_key,
            api_key_env: config.model.api_key_env.clone(),
            temperature: config.model.temperature,
            stream: config.model.stream,
        })
    }

    /// Whether an API key was found in the environment.
    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }

    /// Actionable message when a key is missing for a remote endpoint.
    pub fn missing_key_hint(&self) -> String {
        format!(
            "no API key found in ${}; set it, or point [model].base_url at a local server",
            self.api_key_env
        )
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// True for endpoints that normally run without authentication.
    pub fn is_local_endpoint(&self) -> bool {
        let url = self.base_url.to_ascii_lowercase();
        url.contains("localhost") || url.contains("127.0.0.1") || url.contains("[::1]")
    }

    fn request_body(&self, request: &ModelRequest, stream: bool) -> Value {
        let messages: Vec<Value> = request.messages.iter().map(message_to_wire).collect();

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.temperature,
            "stream": stream,
        });

        if let Some(tokens) = request.max_output_tokens {
            body["max_tokens"] = json!(tokens);
        }
        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
        }
        body
    }

    fn http_error(&self, status: reqwest::StatusCode, body: &str) -> RaiError {
        let detail = crate::util::truncate_line(body.trim(), MAX_ERROR_BODY);
        let hint = match status.as_u16() {
            401 | 403 => format!(" (check ${})", self.api_key_env),
            404 => format!(" (check [model].base_url: {})", self.base_url),
            429 => " (rate limited; retry later)".to_string(),
            _ => String::new(),
        };
        RaiError::Model(format!("provider returned HTTP {status}{hint}: {detail}"))
    }
}

/// Convert an internal message into the wire format.
fn message_to_wire(message: &Message) -> Value {
    let role = message.role.as_str();
    match message.role {
        Role::Tool => json!({
            "role": "tool",
            "tool_call_id": message.tool_call_id.clone().unwrap_or_default(),
            "content": message.text,
        }),
        Role::Assistant if !message.tool_calls.is_empty() => {
            let calls: Vec<Value> = message
                .tool_calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments)
                                .unwrap_or_else(|_| "{}".to_string()),
                        }
                    })
                })
                .collect();
            json!({
                "role": role,
                "content": if message.text.is_empty() { Value::Null } else { json!(message.text) },
                "tool_calls": calls,
            })
        }
        _ => json!({ "role": role, "content": message.text }),
    }
}

/// Parse a `tool_calls` array from a non-streaming message object.
fn parse_tool_calls(value: &Value, _model: &str) -> Vec<ToolCall> {
    let Some(calls) = value.get("tool_calls").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let id = call
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("call-{index}"));
            let function = call.get("function").cloned().unwrap_or(Value::Null);
            let name = function
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let arguments = parse_arguments(function.get("arguments"));
            ToolCall::new(id, name, arguments)
        })
        .collect()
}

/// Tool arguments arrive as a JSON *string*; tolerate malformed input.
fn parse_arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                json!({})
            } else {
                serde_json::from_str(trimmed).unwrap_or_else(|_| json!({ "_raw": text }))
            }
        }
        Some(other) => other.clone(),
        None => json!({}),
    }
}

/// Extract token usage, when present.
///
/// Accepts both the OpenAI spelling (`prompt_tokens`) and the newer/other
/// spelling (`input_tokens`), because gateways differ.
fn parse_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(|v| v.as_u64()))
            .unwrap_or(0)
    };
    Some(TokenUsage {
        input_tokens: number(&["prompt_tokens", "input_tokens"]),
        output_tokens: number(&["completion_tokens", "output_tokens"]),
    })
}

/// Accumulator for streamed tool calls, which arrive in fragments.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    entries: Vec<(String, String, String)>,
}

impl ToolCallAccumulator {
    fn push(&mut self, delta: &Value) {
        let index = delta
            .get("index")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.entries.len() as u64) as usize;
        while self.entries.len() <= index {
            self.entries
                .push((String::new(), String::new(), String::new()));
        }
        let entry = &mut self.entries[index];
        if let Some(id) = delta.get("id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                entry.0 = id.to_string();
            }
        }
        if let Some(function) = delta.get("function") {
            if let Some(name) = function.get("name").and_then(|v| v.as_str()) {
                if !name.is_empty() {
                    entry.1.push_str(name);
                }
            }
            if let Some(args) = function.get("arguments").and_then(|v| v.as_str()) {
                entry.2.push_str(args);
            }
        }
    }

    fn finish(self) -> Vec<ToolCall> {
        self.entries
            .into_iter()
            .enumerate()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(index, (id, name, arguments))| {
                let id = if id.is_empty() {
                    format!("call-{index}")
                } else {
                    id
                };
                ToolCall::new(id, name, parse_arguments(Some(&Value::String(arguments))))
            })
            .collect()
    }
}

#[async_trait]
impl ModelClient for OpenAiClient {
    fn provider(&self) -> &'static str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn respond(&self, request: ModelRequest) -> Result<ModelResponse> {
        let body = self.request_body(&request, false);
        let text = self.post(&body).await?;
        let value: Value = serde_json::from_str(&text).map_err(|e| {
            RaiError::Model(format!(
                "malformed provider response: {e}: {}",
                crate::util::truncate_line(&text, MAX_ERROR_BODY)
            ))
        })?;
        self.parse_response(&value)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        sink: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> Result<ModelResponse> {
        if !self.stream {
            let response = self.respond(request).await?;
            if !response.text.is_empty() {
                let text = response.text.clone();
                sink(&text);
            }
            return Ok(response);
        }

        let body = self.request_body(&request, true);
        let response = self.send(body).await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(self.http_error(status, &text));
        }

        let mut chunks = response.bytes_stream();
        let mut buffer = String::new();
        let mut text = String::new();
        let mut calls = ToolCallAccumulator::default();
        let mut usage: Option<TokenUsage> = None;
        let mut done = false;

        'outer: while let Some(chunk) = chunks.next().await {
            let chunk = chunk.map_err(|e| RaiError::Model(format!("stream interrupted: {e}")))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim_end_matches('\r').to_string();
                buffer.drain(..=newline);

                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if payload == "[DONE]" {
                    done = true;
                    break 'outer;
                }
                let Ok(value) = serde_json::from_str::<Value>(payload) else {
                    // A partial or non-JSON line is not fatal mid-stream.
                    continue;
                };
                if let Some(parsed) = parse_usage(&value) {
                    usage = Some(parsed);
                }
                let Some(delta) = value
                    .get("choices")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("delta"))
                else {
                    continue;
                };
                if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        text.push_str(content);
                        sink(content);
                    }
                }
                if let Some(fragment) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for entry in fragment {
                        calls.push(entry);
                    }
                }
            }
        }

        let tool_calls = calls.finish();
        if !done && text.is_empty() && tool_calls.is_empty() {
            return Err(RaiError::Model(
                "stream ended without any content".to_string(),
            ));
        }

        Ok(ModelResponse {
            text,
            tool_calls,
            usage,
            provider: "openai".to_string(),
            model: self.model.clone(),
        })
    }
}

impl OpenAiClient {
    async fn send(&self, body: Value) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .post(self.endpoint())
            .header("content-type", "application/json")
            .json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        request
            .send()
            .await
            .map_err(|e| RaiError::Model(format!("request failed: {e}")))
    }

    async fn post(&self, body: &Value) -> Result<String> {
        let response = self.send(body.clone()).await?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| RaiError::Model(format!("could not read response body: {e}")))?;
        if !status.is_success() {
            return Err(self.http_error(status, &text));
        }
        Ok(text)
    }

    /// Map a non-streaming response body onto the internal type.
    fn parse_response(&self, value: &Value) -> Result<ModelResponse> {
        let choice = value
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| RaiError::Model("provider response contained no choices".into()))?;

        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            if reason == "content_filter" {
                return Err(RaiError::Model(
                    "provider refused the request (finish_reason = content_filter)".into(),
                ));
            }
        }

        let message = choice.get("message").unwrap_or(&Value::Null);
        let text = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tool_calls = parse_tool_calls(message, &self.model);

        Ok(ModelResponse {
            text,
            tool_calls,
            usage: parse_usage(value),
            provider: "openai".to_string(),
            model: self.model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;
    use crate::tools::{RiskClass, ToolSpec};
    use serde_json::json;

    fn client() -> OpenAiClient {
        let mut config = Config::default();
        config.model.provider = "openai".into();
        config.model.model = "test-model".into();
        config.model.base_url = "http://localhost:9/v1".into();
        OpenAiClient::from_config(&config).unwrap()
    }

    #[test]
    fn endpoint_appends_chat_completions_once() {
        let mut config = Config::default();
        config.model.base_url = "http://localhost:1234/v1/".into();
        let client = OpenAiClient::from_config(&config).unwrap();
        assert_eq!(
            client.endpoint(),
            "http://localhost:1234/v1/chat/completions"
        );
    }

    #[test]
    fn tool_results_carry_the_call_id() {
        let wire = message_to_wire(&Message::tool_result("call-1", "output"));
        assert_eq!(wire["role"], "tool");
        assert_eq!(wire["tool_call_id"], "call-1");
        assert_eq!(wire["content"], "output");
    }

    #[test]
    fn assistant_tool_calls_are_serialized_as_function_calls() {
        let message = Message::assistant_calls(
            "",
            vec![ToolCall::new("c1", "read_file", json!({"path": "a.rs"}))],
        );
        let wire = message_to_wire(&message);
        assert_eq!(wire["tool_calls"][0]["function"]["name"], "read_file");
        // Arguments must travel as a JSON *string*, as the API requires.
        let args = wire["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments is a string");
        assert!(args.contains("a.rs"));
    }

    #[test]
    fn request_body_includes_tools_when_present() {
        let request = ModelRequest::new(vec![Message::user("hi")]).with_tools(vec![ToolSpec {
            name: "read_file".into(),
            description: "read".into(),
            parameters: json!({"type": "object"}),
        }]);
        let body = client().request_body(&request, false);
        assert_eq!(body["stream"], false);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn request_body_omits_tools_when_absent() {
        let body = client().request_body(&ModelRequest::new(vec![Message::user("hi")]), true);
        assert!(body.get("tools").is_none());
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn tool_call_arguments_parse_from_string_payloads() {
        let value = json!({
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {"name": "read_file", "arguments": "{\"path\":\"src/main.rs\"}"}
            }]
        });
        let calls = parse_tool_calls(&value, "test-model");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "src/main.rs");
    }

    #[test]
    fn streamed_tool_call_fragments_are_joined() {
        let mut accumulator = ToolCallAccumulator::default();
        accumulator.push(&json!({
            "index": 0,
            "id": "call_1",
            "function": {"name": "apply_", "arguments": "{\"patch\":"}
        }));
        accumulator.push(&json!({
            "index": 0,
            "function": {"name": "patch", "arguments": "\"diff\"}"}
        }));
        let calls = accumulator.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "apply_patch");
        assert_eq!(calls[0].arguments["patch"], "diff");
    }

    #[test]
    fn usage_is_read_from_both_field_spellings() {
        let usage = parse_usage(&json!({"usage": {"prompt_tokens": 10, "completion_tokens": 4}}));
        assert_eq!(usage.unwrap().input_tokens, 10);
        let usage = parse_usage(&json!({"usage": {"input_tokens": 7, "output_tokens": 3}}));
        assert_eq!(usage.unwrap().output_tokens, 3);
        assert!(parse_usage(&json!({})).is_none());
    }

    #[test]
    fn responses_without_choices_are_an_error() {
        let error = client().parse_response(&json!({"id": "x"})).unwrap_err();
        assert!(matches!(error, RaiError::Model(_)));
    }

    #[test]
    fn content_filter_is_reported_as_a_refusal() {
        let error = client()
            .parse_response(&json!({
                "choices": [{"finish_reason": "content_filter", "message": {"content": ""}}]
            }))
            .unwrap_err();
        assert!(error.to_string().contains("content_filter"));
    }

    #[test]
    fn local_endpoints_are_detected() {
        assert!(client().is_local_endpoint());
        let def = crate::tools::ToolDefinition::local("x", "y", json!({}), RiskClass::ReadOnly);
        assert_eq!(def.risk, RiskClass::ReadOnly);
        assert_eq!(Role::User.as_str(), "user");
    }
}
