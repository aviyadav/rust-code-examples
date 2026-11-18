//! Mock stand-in for APNs/FCM/Twilio/SendGrid (article §2/§3).
//! - Honors X-Idempotency-Key with a TTL dedup window (replays return the SAME reference id).
//! - Runtime failure injection via POST /control (transient/permanent rates, latency spikes).
//! - recipient_id starting with "invalid:" is permanently rejected (deterministic DEAD_LETTER demo).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::info;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
struct Control {
    transient_rate: f64,
    permanent_rate: f64,
    latency_ms: u64,
    latency_jitter_ms: u64,
    dedup_ttl_secs: u64,
}
impl Default for Control {
    fn default() -> Self {
        Self { transient_rate: 0.05, permanent_rate: 0.0, latency_ms: 30, latency_jitter_ms: 40, dedup_ttl_secs: 3600 }
    }
}

#[derive(Deserialize)]
struct ControlUpdate {
    transient_rate: Option<f64>,
    permanent_rate: Option<f64>,
    latency_ms: Option<u64>,
    latency_jitter_ms: Option<u64>,
    dedup_ttl_secs: Option<u64>,
}

#[derive(Clone)]
struct Entry {
    provider_reference_id: String,
    at: Instant,
}

#[derive(Default)]
struct Stats {
    accepted: AtomicU64,
    replayed: AtomicU64,
    transient_rejected: AtomicU64,
    permanent_rejected: AtomicU64,
    in_flight: AtomicI64,
}

struct ProvState {
    control: Mutex<Control>,
    store: Mutex<HashMap<String, Entry>>,
    delivered: Mutex<VecDeque<serde_json::Value>>,
    by_channel: Mutex<HashMap<String, u64>>,
    stats: Stats,
}

#[derive(Deserialize)]
struct SendRequest {
    #[serde(default = "d_id")]
    notification_id: String,
    #[serde(default = "d_channel")]
    channel: String,
    #[serde(default = "d_recipient")]
    recipient_id: String,
    #[serde(default)]
    payload: String,
    #[serde(default)]
    tenant_id: String,
}
fn d_id() -> String { Uuid::new_v4().to_string() }
fn d_channel() -> String { "push".into() }
fn d_recipient() -> String { "unknown".into() }

async fn send(State(st): State<Arc<ProvState>>, headers: HeaderMap, Json(req): Json<SendRequest>) -> Response {
    st.stats.in_flight.fetch_add(1, Ordering::Relaxed);
    let resp = handle_send(&st, &headers, req).await;
    st.stats.in_flight.fetch_sub(1, Ordering::Relaxed);
    resp
}

async fn handle_send(st: &Arc<ProvState>, headers: &HeaderMap, req: SendRequest) -> Response {
    let key = headers
        .get("x-idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}:{}", req.notification_id, req.channel));

    let ctl = st.control.lock().unwrap().clone();

    // Idempotent replay: a completed operation returns its stored result,
    // regardless of current failure injection (real idempotency semantics).
    {
        let mut store = st.store.lock().unwrap();
        if store.len() > 50_000 {
            let ttl = Duration::from_secs(ctl.dedup_ttl_secs);
            store.retain(|_, e| e.at.elapsed() < ttl);
        }
        if let Some(e) = store.get(&key) {
            if e.at.elapsed() < Duration::from_secs(ctl.dedup_ttl_secs) {
                st.stats.replayed.fetch_add(1, Ordering::Relaxed);
                return (StatusCode::OK, Json(json!({
                    "provider_reference_id": e.provider_reference_id,
                    "dedup_replayed": true,
                }))).into_response();
            }
        }
    }

    // Simulated WAN/provider latency.
    let jitter = if ctl.latency_jitter_ms > 0 {
        rand::thread_rng().gen_range(0..=ctl.latency_jitter_ms)
    } else { 0 };
    tokio::time::sleep(Duration::from_millis(ctl.latency_ms + jitter)).await;

    let roll: f64 = rand::thread_rng().gen();
    if req.recipient_id.starts_with("invalid:") || roll < ctl.permanent_rate {
        st.stats.permanent_rejected.fetch_add(1, Ordering::Relaxed);
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "permanent: invalid recipient or rejected" }))).into_response();
    }
    if roll < ctl.permanent_rate + ctl.transient_rate {
        st.stats.transient_rejected.fetch_add(1, Ordering::Relaxed);
        let code = [500u16, 503, 429, 504][rand::thread_rng().gen_range(0..4)];
        return (StatusCode::from_u16(code).unwrap(), Json(json!({ "error": format!("transient: HTTP {code}") }))).into_response();
    }

    let reference = format!("prov_{}_{}", req.channel, Uuid::new_v4());
    st.store.lock().unwrap().insert(key, Entry { provider_reference_id: reference.clone(), at: Instant::now() });
    st.stats.accepted.fetch_add(1, Ordering::Relaxed);
    *st.by_channel.lock().unwrap().entry(req.channel.clone()).or_insert(0) += 1;
    {
        let mut d = st.delivered.lock().unwrap();
        d.push_back(json!({
            "notification_id": req.notification_id,
            "channel": req.channel,
            "recipient_id": req.recipient_id,
            "tenant_id": req.tenant_id,
            "payload": req.payload,
            "provider_reference_id": reference,
            "at": chrono::Utc::now().to_rfc3339(),
        }));
        while d.len() > 500 { d.pop_front(); }
    }
    (StatusCode::OK, Json(json!({
        "provider_reference_id": reference,
        "dedup_replayed": false,
    }))).into_response()
}

async fn stats(State(st): State<Arc<ProvState>>) -> Json<serde_json::Value> {
    Json(json!({
        "accepted": st.stats.accepted.load(Ordering::Relaxed),
        "idempotent_replays": st.stats.replayed.load(Ordering::Relaxed),
        "transient_rejected": st.stats.transient_rejected.load(Ordering::Relaxed),
        "permanent_rejected": st.stats.permanent_rejected.load(Ordering::Relaxed),
        "in_flight": st.stats.in_flight.load(Ordering::Relaxed),
        "by_channel": *st.by_channel.lock().unwrap(),
        "control": st.control.lock().unwrap().clone(),
    }))
}

async fn control(State(st): State<Arc<ProvState>>, Json(u): Json<ControlUpdate>) -> Json<Control> {
    let mut c = st.control.lock().unwrap();
    if let Some(v) = u.transient_rate { c.transient_rate = v.clamp(0.0, 1.0); }
    if let Some(v) = u.permanent_rate { c.permanent_rate = v.clamp(0.0, 1.0); }
    if let Some(v) = u.latency_ms { c.latency_ms = v; }
    if let Some(v) = u.latency_jitter_ms { c.latency_jitter_ms = v; }
    if let Some(v) = u.dedup_ttl_secs { c.dedup_ttl_secs = v; }
    info!(?c, "control updated");
    Json(c.clone())
}

async fn reset(State(st): State<Arc<ProvState>>) -> Json<Control> {
    let mut c = st.control.lock().unwrap();
    *c = Control::default();
    Json(c.clone())
}

async fn messages(State(st): State<Arc<ProvState>>) -> Json<Vec<serde_json::Value>> {
    let d = st.delivered.lock().unwrap();
    Json(d.iter().rev().take(50).cloned().collect())
}

async fn health() -> &'static str { "ok" }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let port: u16 = common::config::env_parse("PROVIDER_PORT", 8081);
    let st = Arc::new(ProvState {
        control: Mutex::new(Control::default()),
        store: Mutex::new(HashMap::new()),
        delivered: Mutex::new(VecDeque::new()),
        by_channel: Mutex::new(HashMap::new()),
        stats: Stats::default(),
    });
    let app = Router::new()
        .route("/v1/send", post(send))
        .route("/stats", get(stats))
        .route("/messages", get(messages))
        .route("/control", post(control))
        .route("/control/reset", post(reset))
        .route("/health", get(health))
        .with_state(st);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    info!("mock provider listening on http://localhost:{port}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
        .await?;
    Ok(())
}
