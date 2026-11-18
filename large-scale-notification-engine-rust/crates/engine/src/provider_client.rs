use common::{NotificationEvent, ProviderResponse};
use reqwest::Client;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("transient: {0}")]
    Transient(String),
    #[error("permanent: {0}")]
    Permanent(String),
}

pub async fn dispatch(
    client: &Client,
    url: &str,
    event: &NotificationEvent,
) -> Result<ProviderResponse, ProviderError> {
    // Stable idempotency key across retries => effectively-once at providers that
    // support idempotent ingest (article §2). Others degrade to at-least-once.
    let idem_key = format!("{}:{}", event.notification_id, event.channel);
    let response = client
        .post(url)
        .header("X-Idempotency-Key", &idem_key)
        .json(event)
        .send()
        .await
        .map_err(|e| ProviderError::Transient(e.to_string()))?;

    let status = response.status();
    if status.is_success() {
        response
            .json::<ProviderResponse>()
            .await
            .map_err(|e| ProviderError::Transient(format!("decode: {e}")))
    } else if status.as_u16() == 429 || status.is_server_error() {
        Err(ProviderError::Transient(format!("HTTP {status}")))
    } else {
        Err(ProviderError::Permanent(format!("HTTP {status}")))
    }
}