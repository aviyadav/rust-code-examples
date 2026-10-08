//! End-to-end tests for the OpenAI-compatible adapter.
//!
//! A tiny local HTTP server speaks the wire protocol, so the request body, SSE
//! parsing, tool-call fragment accumulation, and usage accounting are all
//! verified without a network, an API key, or a paid call.

use std::sync::Arc;

use rai::config::Config;
use rai::model::openai::OpenAiClient;
use rai::model::{Message, ModelClient, ModelRequest};
use rai::tools::ToolSpec;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve exactly one HTTP response, and return the request body the client sent.
async fn serve_once(
    response: String,
    content_type: &'static str,
) -> (u16, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut received = Vec::new();
        let mut chunk = [0u8; 8192];
        let mut body_start: Option<usize> = None;
        let mut expected: Option<usize> = None;

        loop {
            let read = socket.read(&mut chunk).await.expect("read");
            if read == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&received).to_string();

            if body_start.is_none() {
                if let Some(position) = text.find("\r\n\r\n") {
                    body_start = Some(position + 4);
                    expected = content_length(&text);
                }
            }
            if let (Some(start), Some(length)) = (body_start, expected) {
                if received.len() >= start + length {
                    break;
                }
            }
        }

        let text = String::from_utf8_lossy(&received).to_string();
        let body = match text.find("\r\n\r\n") {
            Some(position) => text[position + 4..].to_string(),
            None => String::new(),
        };

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        );
        socket.write_all(response.as_bytes()).await.expect("write");
        socket.flush().await.expect("flush");
        body
    });

    (port, handle)
}

fn content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    })
}

fn client_for(port: u16, stream: bool) -> OpenAiClient {
    let mut config = Config::default();
    config.model.provider = "openai".to_string();
    config.model.model = "test-model".to_string();
    config.model.base_url = format!("http://127.0.0.1:{port}/v1");
    config.model.stream = stream;
    config.model.api_key_env = "RAI_TEST_UNSET_KEY_ENV".to_string();
    OpenAiClient::from_config(&config).expect("client builds")
}

#[tokio::test]
async fn streams_text_deltas_from_an_openai_compatible_endpoint() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n\n",
        "data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, handle) = serve_once(sse.to_string(), "text/event-stream").await;

    let client = client_for(port, true);
    let mut seen = String::new();
    let response = client
        .stream(ModelRequest::new(vec![Message::user("hi")]), &mut |chunk| {
            seen.push_str(chunk)
        })
        .await
        .expect("stream succeeds");

    assert_eq!(response.text, "Hello world");
    assert_eq!(
        seen, "Hello world",
        "deltas must reach the sink as they arrive"
    );
    assert_eq!(response.usage.expect("usage").input_tokens, 11);
    assert_eq!(response.provider, "openai");

    let body = handle.await.expect("server finishes");
    let sent: Value = serde_json::from_str(&body).expect("request body is JSON");
    assert_eq!(sent["model"], "test-model");
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["messages"][0]["role"], "user");
}

#[tokio::test]
async fn accumulates_tool_call_fragments_split_across_deltas() {
    // Providers may split a single call across chunks; the adapter must join
    // name and argument fragments by index.
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"file\",\"arguments\":\"\\\"src/main.rs\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _handle) = serve_once(sse.to_string(), "text/event-stream").await;

    let client = client_for(port, true);
    let response = client
        .stream(
            ModelRequest::new(vec![Message::user("read the file")]),
            &mut |_| {},
        )
        .await
        .expect("stream succeeds");

    assert_eq!(response.tool_calls.len(), 1);
    let call = &response.tool_calls[0];
    assert_eq!(call.id, "call_1");
    assert_eq!(call.name, "read_file");
    assert_eq!(call.arguments["path"], "src/main.rs");
    assert!(!response.is_final());
}

#[tokio::test]
async fn non_streaming_responses_are_parsed_with_tools_and_usage() {
    let body = json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": "Looking that up.",
                "tool_calls": [{
                    "id": "call_abc",
                    "type": "function",
                    "function": {
                        "name": "search_text",
                        "arguments": "{\"query\":\"policy\"}"
                    }
                }]
            }
        }],
        "usage": { "prompt_tokens": 30, "completion_tokens": 7 }
    })
    .to_string();
    let (port, _handle) = serve_once(body, "application/json").await;

    let client = client_for(port, false);
    let request =
        ModelRequest::new(vec![Message::user("where is policy?")]).with_tools(vec![ToolSpec {
            name: "search_text".to_string(),
            description: "search".to_string(),
            parameters: json!({"type": "object"}),
        }]);

    let response = client.respond(request).await.expect("respond succeeds");
    assert_eq!(response.text, "Looking that up.");
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "search_text");
    assert_eq!(response.tool_calls[0].arguments["query"], "policy");
    let usage = response.usage.expect("usage");
    assert_eq!((usage.input_tokens, usage.output_tokens), (30, 7));
}

#[tokio::test]
async fn provider_errors_are_reported_as_model_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();

    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut chunk = [0u8; 4096];
        let _ = socket.read(&mut chunk).await;
        let body = "{\"error\":{\"message\":\"bad key\"}}";
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });

    let client = client_for(port, false);
    let error = client
        .respond(ModelRequest::new(vec![Message::user("hi")]))
        .await
        .expect_err("401 must be an error");
    let message = error.to_string();
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("bad key"), "{message}");
}

#[tokio::test]
async fn a_local_endpoint_without_a_key_is_still_usable() {
    let body = json!({
        "choices": [{ "message": { "content": "ok" } }]
    })
    .to_string();
    let (port, handle) = serve_once(body, "application/json").await;

    let client = client_for(port, false);
    assert!(client.is_local_endpoint());
    let response = client
        .respond(ModelRequest::new(vec![Message::user("ping")]))
        .await
        .expect("respond succeeds");
    assert_eq!(response.text, "ok");

    // No Authorization header must be sent when no key is configured.
    let sent = handle.await.expect("server finishes");
    assert!(!sent.to_lowercase().contains("authorization"));
}

/// Keep the unused-import linter honest about `Arc` usage in this file.
#[allow(dead_code)]
fn _assert_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Arc<OpenAiClient>>();
}
