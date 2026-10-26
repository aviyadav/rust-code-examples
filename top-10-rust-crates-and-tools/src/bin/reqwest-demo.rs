//! HTTP client calls with reqwest.
//!
//! So the demo runs offline and without an API key, it starts a tiny local
//! "orders API" (axum) on a random port, then talks to it with reqwest exactly
//! like it would talk to a real remote service.

use axum::{
    Json, Router,
    extract::Path,
    http::{HeaderMap, StatusCode, header},
    routing::get,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const TOKEN: &str = "demo-secret-token";

#[derive(Debug, Serialize, Deserialize)]
struct Order {
    id: u64,
    customer_email: String,
    total_cents: u64,
}

#[derive(Debug, Serialize)]
struct NewOrder {
    customer_email: String,
    total_cents: u64,
}

// ---------- Fake remote API (server side) ----------

fn authorized(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Bearer {TOKEN}"))
}

async fn get_order(headers: HeaderMap, Path(id): Path<u64>) -> Result<Json<Order>, StatusCode> {
    if !authorized(&headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    match id {
        42 => Ok(Json(Order {
            id,
            customer_email: "ferris@rust-lang.org".into(),
            total_cents: 1999,
        })),
        _ => Err(StatusCode::NOT_FOUND),
    }
}

async fn create_order(
    headers: HeaderMap,
    Json(new): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<Order>), StatusCode> {
    if !authorized(&headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let order = Order {
        id: 43,
        customer_email: new["customer_email"].as_str().unwrap_or_default().into(),
        total_cents: new["total_cents"].as_u64().unwrap_or_default(),
    };
    Ok((StatusCode::CREATED, Json(order)))
}

async fn slow() -> &'static str {
    tokio::time::sleep(Duration::from_secs(5)).await;
    "finally"
}

async fn start_fake_api() -> std::io::Result<String> {
    let app = Router::new()
        .route("/orders/{id}", get(get_order))
        .route("/orders", axum::routing::post(create_order))
        .route("/slow", get(slow));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(base)
}

// ---------- reqwest (client side) ----------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let base = start_fake_api().await?;
    let token = std::env::var("ORDERS_API_TOKEN").unwrap_or_else(|_| TOKEN.into());

    // Build one Client and reuse it: it keeps a connection pool
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("orders-sync/1.0")
        .build()?;

    // 1. GET + bearer auth + JSON body into a typed struct
    let order: Order = client
        .get(format!("{base}/orders/42"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!("GET  /orders/42  -> {order:?}");

    // 2. POST a JSON body
    let created: Order = client
        .post(format!("{base}/orders"))
        .bearer_auth(&token)
        .json(&NewOrder {
            customer_email: "corro@example.com".into(),
            total_cents: 4550,
        })
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!("POST /orders     -> {created:?}");

    // 3. error_for_status turns 4xx/5xx into an Err you can inspect
    let err = client
        .get(format!("{base}/orders/7"))
        .bearer_auth(&token)
        .send()
        .await?
        .error_for_status()
        .unwrap_err();
    println!(
        "GET  /orders/7   -> error: {err}  (status: {:?})",
        err.status()
    );

    // 4. Missing credentials -> 401. Checking the status by hand instead
    let response = client.get(format!("{base}/orders/42")).send().await?;
    println!("GET  /orders/42 without token -> {}", response.status());

    // 5. Per-request timeout overrides the client default
    let err = client
        .get(format!("{base}/slow"))
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .unwrap_err();
    println!("GET  /slow       -> timed out: {}", err.is_timeout());

    // 6. Fire several requests concurrently over the same client
    let ids = [42, 7, 42];
    let results = get_concurrently(&client, &base, &token, &ids).await?;
    for (id, status) in ids.iter().zip(results) {
        println!("concurrent GET /orders/{id} -> {status}");
    }

    Ok(())
}

/// Sends one GET per ID at the same time and returns the status codes in order.
async fn get_concurrently(
    client: &Client,
    base: &str,
    token: &str,
    ids: &[u64],
) -> anyhow::Result<Vec<reqwest::StatusCode>> {
    // Client is cheap to clone (it's an Arc inside), and all clones share one pool
    let handles: Vec<_> = ids
        .iter()
        .map(|id| {
            let request = client.get(format!("{base}/orders/{id}")).bearer_auth(token);
            tokio::spawn(async move { request.send().await })
        })
        .collect();

    let mut statuses = Vec::new();
    for handle in handles {
        statuses.push(handle.await??.status());
    }
    Ok(statuses)
}
