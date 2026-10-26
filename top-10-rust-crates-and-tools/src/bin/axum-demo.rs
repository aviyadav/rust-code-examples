use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::sync::Arc;
use std::time::Duration;

#[derive(Serialize, sqlx::FromRow)]
struct Order {
    id: i64,
    customer_email: String,
    total_cents: i64,
}

/// Errors a handler can return. `IntoResponse` turns each one into an HTTP response.
#[derive(thiserror::Error, Debug)]
enum AppError {
    #[error("order {0} not found")]
    NotFound(u64),

    #[error("database error")]
    Database(#[from] sqlx::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Database(err) => {
                // Log the real cause, but don't leak database details to the client
                eprintln!("database error: {err}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = Json(serde_json::json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}

struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Opens an in-memory SQLite database and seeds it with sample orders.
    async fn connect() -> Self {
        // Each in-memory SQLite connection is its own database, so keep exactly one alive
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None::<Duration>)
            .max_lifetime(None::<Duration>)
            .connect("sqlite::memory:")
            .await
            .expect("failed to open SQLite");

        sqlx::query(
            "CREATE TABLE orders (
                id             INTEGER PRIMARY KEY,
                customer_email TEXT    NOT NULL,
                total_cents    INTEGER NOT NULL
            );
            INSERT INTO orders VALUES
                (1, 'ferris@rust-lang.org', 1999),
                (2, 'corro@example.com',    4550),
                (3, 'alice@example.com',     899);",
        )
        .execute(&pool)
        .await
        .expect("failed to seed SQLite");

        Db { pool }
    }

    async fn fetch(&self, id: u64) -> Result<Order, AppError> {
        sqlx::query_as("SELECT id, customer_email, total_cents FROM orders WHERE id = ?")
            .bind(id as i64)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(AppError::NotFound(id))
    }
}

async fn get_order(
    State(db): State<Arc<Db>>,
    Path(id): Path<u64>,
) -> Result<Json<Order>, AppError> {
    let order = db.fetch(id).await?;
    Ok(Json(order))
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/orders/{id}", get(get_order))
        .with_state(Arc::new(Db::connect().await));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("listening on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}
