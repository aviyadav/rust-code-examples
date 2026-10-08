//! Networked tools.
//!
//! `fetch_docs` is the only tool in this class. It is bounded, timed out, and
//! refuses URLs that point at cloud metadata or carry embedded credentials.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    object_schema, optional_usize, require_str, Tool, ToolContext, ToolDefinition, ToolOutcome,
};
use crate::error::{RaiError, Result};

/// Fetch a documentation URL and return readable text.
pub struct FetchDocs;

/// Hosts that must never be reachable from a tool call: cloud instance
/// metadata services are the classic SSRF target.
const BLOCKED_HOSTS: &[&str] = &[
    "169.254.169.254",
    "metadata.google.internal",
    "100.100.100.200",
    "fd00:ec2::254",
];

/// Whether a URL is acceptable for a docs fetch.
pub fn url_is_allowed(url: &str) -> std::result::Result<(), String> {
    let lowered = url.trim().to_ascii_lowercase();
    if !(lowered.starts_with("http://") || lowered.starts_with("https://")) {
        return Err("only http and https URLs are supported".into());
    }
    let after_scheme = lowered
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    if authority.contains('@') {
        return Err("URLs with embedded credentials are refused".into());
    }
    let host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority);
    if BLOCKED_HOSTS.contains(&host) {
        return Err(format!("`{host}` is a metadata endpoint and is refused"));
    }
    Ok(())
}

#[async_trait]
impl Tool for FetchDocs {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "fetch_docs",
            "Fetch a public documentation or reference page over HTTP(S) and return its readable text.",
            object_schema(
                json!({
                    "url": {
                        "type": "string",
                        "description": "Absolute http(s) URL."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "description": "Maximum response bytes to read (default 200000)."
                    }
                }),
                &["url"],
            ),
            super::RiskClass::Network,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        let url = require_str(&args, "url", "fetch_docs")?;
        if let Err(problem) = url_is_allowed(&url) {
            return Err(RaiError::InvalidArguments {
                tool: "fetch_docs".into(),
                problem,
            });
        }
        let max_bytes = optional_usize(&args, "max_bytes")
            .unwrap_or(200_000)
            .clamp(1024, 2_000_000);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("rai/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| RaiError::Model(e.to_string()))?;

        let cancel = ctx.cancel.clone();
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err(RaiError::Cancelled),
            response = client.get(&url).send() => {
                response.map_err(|e| RaiError::Model(format!("fetch failed: {e}")))?
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Err(RaiError::Model(format!("{url} returned HTTP {status}")));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body = response
            .text()
            .await
            .map_err(|e| RaiError::Model(format!("could not read body: {e}")))?;

        let (body, truncated) = crate::util::truncate_bytes(&body, max_bytes);
        let text = if content_type.contains("html") || body.trim_start().starts_with('<') {
            html_to_text(&body)
        } else {
            body
        };
        let (text, redactions) = ctx.redactor.redact(&text);

        let mut out = format!("url: {url}\nstatus: {status}\n");
        if truncated {
            out.push_str("note: response truncated\n");
        }
        if redactions > 0 {
            out.push_str(&format!("note: {redactions} secret(s) redacted\n"));
        }
        out.push_str("---\n");
        out.push_str(&text);

        Ok(ToolOutcome::new(
            format!("fetched {url} ({status})"),
            out,
        )
        .with_data(json!({ "url": url, "status": status.as_u16(), "truncated": truncated, "redactions": redactions }))
        .truncated_if(truncated))
    }
}

/// Convert HTML to readable plain text.
///
/// Intentionally simple: scripts, styles, and tags are removed, block-level
/// elements become line breaks, and a handful of entities are decoded.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut i = 0usize;
    let mut skipping: Option<&str> = None;

    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i..];
            if skipping.is_none() {
                if rest.starts_with("<script") {
                    skipping = Some("</script>");
                } else if rest.starts_with("<style") {
                    skipping = Some("</style>");
                } else if rest.starts_with("<br")
                    || rest.starts_with("</p")
                    || rest.starts_with("</div")
                    || rest.starts_with("</li")
                    || rest.starts_with("</h")
                    || rest.starts_with("</tr")
                {
                    text.push('\n');
                } else if rest.starts_with("<li") {
                    text.push('\n');
                    text.push_str("- ");
                }
            }
            let Some(end) = html[i..].find('>') else {
                break;
            };
            let tag_end = i + end + 1;
            if let Some(marker) = skipping {
                if lower[i..].starts_with(marker) {
                    skipping = None;
                }
            }
            i = tag_end;
            continue;
        }
        let ch = html[i..].chars().next().unwrap_or(' ');
        // Skip the inner text of <script> and <style> blocks, not only their
        // tags.
        if skipping.is_none() {
            text.push(ch);
        }
        i += ch.len_utf8();
    }

    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    let mut out = String::with_capacity(decoded.len());
    let mut blank_run = 0usize;
    for line in decoded.lines() {
        let trimmed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(&trimmed);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_and_credentialed_urls() {
        assert!(url_is_allowed("file:///etc/passwd").is_err());
        assert!(url_is_allowed("https://user:pw@example.com/x").is_err());
        assert!(url_is_allowed("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(url_is_allowed("https://doc.rust-lang.org/std/").is_ok());
        assert!(url_is_allowed("http://localhost:8000/docs").is_ok());
    }

    #[test]
    fn strips_scripts_styles_and_tags() {
        let html = "<html><head><style>p{color:red}</style></head><body>\
            <h1>Title</h1><script>alert('x')</script><p>Hello &amp; welcome</p>\
            <ul><li>one</li><li>two</li></ul></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello & welcome"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("color:red"));
        assert!(text.contains("- one"));
    }

    #[test]
    fn collapses_whitespace() {
        let text = html_to_text("<p>a\n\n\n   b</p>");
        assert!(text.starts_with("a\n"));
        assert!(!text.contains("\n\n\n"));
    }
}
