use serde::{Deserialize, Serialize};

/// Raw event accepted by the API and routed through the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationEvent {
    pub notification_id: String,
    pub tenant_id: String,
    pub channel: String, // push | sms | email
    pub recipient_id: String,
    pub template_id: String,
    pub payload: String, // JSON vars before orchestration, rendered text after
}

/// Article §3: retry envelope with exponential backoff + full jitter schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryEvent {
    pub event: NotificationEvent,
    pub attempt: i32,
    pub retry_at_epoch_ms: i64,
}

/// Article §4: poison pills / permanent failures diverted with tracing context.
#[derive(Debug, Serialize, Deserialize)]
pub struct DlqEnvelope {
    pub source_topic: String,
    pub partition: i32,
    pub offset: i64,
    pub reason: String,
    pub payload_utf8_lossy: String,
    pub failed_at_epoch_ms: i64,
}

/// Mock provider response. `dedup_replayed` proves provider-side idempotency (article §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub provider_reference_id: String,
    pub dedup_replayed: bool,
}

pub fn valid_channel(c: &str) -> bool {
    matches!(c, "push" | "sms" | "email")
}