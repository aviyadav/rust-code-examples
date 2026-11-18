use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::config::{env_or, env_parse};
use common::{topics, NotificationEvent};
use rdkafka::producer::FutureProducer;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use tracing::info;
use uuid::Uuid;

struct ApiState {
    producer: FutureProducer,
    pool: PgPool,
}

#[derive(Debug, Deserialize)]
struct IngestRequest {
    notification_id: Option<String>,
    #[serde(default = "d_tenant")]
    tenant_id: String,
    channel: String,
    recipient_id: String,
    #[serde(default = "d_template")]
    template_id: String,
    payload: Value,
}
fn d_tenant() -> String {
    "default".into()
}
fn d_template() -> String {
    "generic".into()
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

fn to_event(req: IngestRequest) -> Result<NotificationEvent, String> {
    if !common::valid_channel(&req.channel) {
        return Err(format!("invalid channel '{}'", req.channel));
    }
    if req.recipient_id.trim().is_empty() {
        return Err("recipient_id must not be empty".into());
    }
    Ok(NotificationEvent {
        notification_id: req
            .notification_id
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        tenant_id: req.tenant_id,
        channel: req.channel,
        recipient_id: req.recipient_id,
        template_id: req.template_id,
        payload: req.payload.to_string(),
    })
}

async fn accept_one(State(st): State<Arc<ApiState>>, Json(req): Json<IngestRequest>) -> Response {
    let event = match to_event(req) {
        Ok(e) => e,
        Err(m) => return err(StatusCode::BAD_REQUEST, m),
    };
    match common::kafka::produce_json(&st.producer, topics::INGEST, &event.recipient_id, &event)
        .await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({ "notification_id": event.notification_id, "status": "ACCEPTED" })),
        )
            .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn accept_batch(
    State(st): State<Arc<ApiState>>,
    Json(reqs): Json<Vec<IngestRequest>>,
) -> Response {
    if reqs.len() > 1000 {
        return err(StatusCode::BAD_REQUEST, "batch limit is 1000");
    }
    let mut ids = Vec::with_capacity(reqs.len());
    for req in reqs {
        let event = match to_event(req) {
            Ok(e) => e,
            Err(m) => return err(StatusCode::BAD_REQUEST, m),
        };
        if let Err(e) =
            common::kafka::produce_json(&st.producer, topics::INGEST, &event.recipient_id, &event)
                .await
        {
            return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
        }
        ids.push(event.notification_id);
    }
    (
        StatusCode::ACCEPTED,
        Json(json!({ "accepted": ids.len(), "notification_ids": ids })),
    )
        .into_response()
}

async fn get_status(State(st): State<Arc<ApiState>>, Path(id): Path<String>) -> Response {
    let rows = sqlx::query(
        "SELECT channel, status, attempts, provider_reference_id, last_error, updated_at
         FROM notification_deliveries WHERE notification_id = $1",
    )
    .bind(&id)
    .fetch_all(&st.pool)
    .await;
    match rows {
        Ok(rows) => {
            let out: Vec<Value> = rows.iter().map(|r| json!({
                "channel": r.get::<String, _>("channel"),
                "status": r.get::<String, _>("status"),
                "attempts": r.get::<i32, _>("attempts"),
                "provider_reference_id": r.get::<Option<String>, _>("provider_reference_id"),
                "last_error": r.get::<Option<String>, _>("last_error"),
                "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            })).collect();
            Json(json!({ "notification_id": id, "deliveries": out })).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn health() -> &'static str {
    "ok"
}

pub async fn run() -> Result<()> {
    let brokers = env_or("KAFKA_BROKERS", "localhost:29092");
    let database_url = env_or(
        "DATABASE_URL",
        "postgres://notify:notify@localhost:5432/notifications",
    );
    let port: u16 = env_parse("API_PORT", 8080);

    common::kafka::ensure_topics(&brokers).await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    common::schema::ensure_schema(&pool).await?;
    let producer = common::kafka::new_producer(&brokers)?;

    let state = Arc::new(ApiState { producer, pool });
    let app = Router::new()
        .route("/v1/notifications", post(accept_one))
        .route("/v1/notifications/batch", post(accept_batch))
        .route("/v1/notifications/:id", get(get_status))
        .route("/health", get(health))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!("ingest API listening on http://localhost:{port}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
