use std::time::Duration;

use anyhow::Context;
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use thiserror::Error;

/// Typed errors for the order "library" code. Callers can match on these.
#[derive(Error, Debug)]
pub enum OrderError {
    #[error("order {0} not found")]
    NotFound(u64),

    #[error("payment declined: {reason}")]
    PaymentDeclined { reason: String },

    // `#[from]` lets `?` convert these automatically and keeps them as the source
    #[error("sqlite error")]
    Sqlite(#[from] sqlx::Error),

    #[error("duckdb error")]
    DuckDb(#[from] duckdb::Error),
}

// ---------- SQLite (sqlx, async): the transactional orders store ----------

async fn setup_sqlite() -> Result<SqlitePool, OrderError> {
    // Each in-memory SQLite connection is its own database, so keep exactly one
    // connection alive for the whole program.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None::<Duration>)
        .max_lifetime(None::<Duration>)
        .connect("sqlite::memory:")
        .await?;

    sqlx::query(
        "CREATE TABLE orders (
            id             INTEGER PRIMARY KEY,
            customer_email TEXT    NOT NULL,
            total_cents    INTEGER NOT NULL CHECK (total_cents > 0)
        )",
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        "INSERT INTO orders VALUES
            (1, 'ferris@rust-lang.org', 1999),
            (2, 'corro@example.com',    4550),
            (3, 'alice@example.com',     899)",
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

async fn find_order(pool: &SqlitePool, id: u64) -> Result<u32, OrderError> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT total_cents FROM orders WHERE id = ?")
        .bind(id as i64)
        .fetch_optional(pool)
        .await?;
    row.map(|(cents,)| cents as u32)
        .ok_or(OrderError::NotFound(id))
}

async fn create_order(
    pool: &SqlitePool,
    id: u64,
    email: &str,
    total_cents: i64,
) -> Result<(), OrderError> {
    sqlx::query("INSERT INTO orders (id, customer_email, total_cents) VALUES (?, ?, ?)")
        .bind(id as i64)
        .bind(email)
        .bind(total_cents)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------- DuckDB (sync): the analytics / reporting store ----------

fn setup_duckdb() -> Result<duckdb::Connection, OrderError> {
    let conn = duckdb::Connection::open_in_memory()?;
    conn.execute_batch("CREATE TABLE payments (order_id BIGINT, amount_cents BIGINT)")?;
    Ok(conn)
}

fn record_payment(duck: &duckdb::Connection, id: u64, amount_cents: u32) -> Result<(), OrderError> {
    duck.execute(
        "INSERT INTO payments VALUES (?, ?)",
        duckdb::params![id as i64, amount_cents as i64],
    )?;
    Ok(())
}

fn import_payments_csv(duck: &duckdb::Connection, path: &str) -> Result<usize, OrderError> {
    Ok(duck.execute("INSERT INTO payments SELECT * FROM read_csv(?)", [path])?)
}

fn revenue_report(duck: &duckdb::Connection) -> Result<(i64, i64), OrderError> {
    Ok(duck.query_row(
        "SELECT count(*), CAST(coalesce(sum(amount_cents), 0) AS BIGINT) FROM payments",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

// ---------- Business logic ----------

fn charge(total_cents: u32) -> Result<(), OrderError> {
    if total_cents > 2000 {
        return Err(OrderError::PaymentDeclined {
            reason: "insufficient funds".into(),
        });
    }
    Ok(())
}

/// Application-level function: anyhow adds context on top of the typed errors.
async fn checkout(pool: &SqlitePool, duck: &duckdb::Connection, id: u64) -> anyhow::Result<()> {
    let total_cents = find_order(pool, id)
        .await
        .with_context(|| format!("failed to load order {id}"))?;
    charge(total_cents).with_context(|| format!("failed to charge order {id}"))?;
    record_payment(duck, id, total_cents)
        .with_context(|| format!("failed to record payment for order {id}"))?;
    println!("order {id} paid: ${:.2}", total_cents as f64 / 100.0);
    Ok(())
}

fn report(err: &anyhow::Error) {
    // `{:#}` prints the whole chain: context -> error -> underlying source
    eprintln!("error: {err:#}");

    // The typed error is still reachable through anyhow
    match err.downcast_ref::<OrderError>() {
        Some(OrderError::NotFound(_)) => eprintln!("  hint: check the order ID and try again"),
        Some(OrderError::Sqlite(_) | OrderError::DuckDb(_)) => {
            eprintln!("  hint: the database rejected the request, see the cause above")
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Setup failures are fatal: `?` in main prints the error chain and exits with code 1
    let pool = setup_sqlite().await.context("failed to set up SQLite")?;
    let duck = setup_duckdb().context("failed to set up DuckDB")?;

    // 1 and 3 succeed, 2 is declined, 42 doesn't exist
    for id in [1, 2, 3, 42] {
        if let Err(err) = checkout(&pool, &duck, id).await {
            report(&err);
        }
    }

    // SQLite rejects this row via the CHECK constraint -> OrderError::Sqlite
    if let Err(err) = create_order(&pool, 4, "bob@example.com", -500)
        .await
        .context("failed to create order 4")
    {
        report(&err);
    }

    // DuckDB can't find the file -> OrderError::DuckDb
    if let Err(err) = import_payments_csv(&duck, "missing-payments.csv")
        .context("failed to import payments from CSV")
    {
        report(&err);
    }

    let (count, cents) = revenue_report(&duck).context("failed to build revenue report")?;
    println!("revenue: {count} payments, ${:.2}", cents as f64 / 100.0);

    Ok(())
}
