//! Compile-time checked SQL with sqlx.
//!
//! The `query!` / `query_as!` / `query_scalar!` macros connect to the database in
//! `DATABASE_URL` (see `.env`) while compiling. They check that the SQL parses,
//! that every column exists, and that column types and nullability match the
//! Rust types. A typo in a column name is a compile error, not a runtime surprise.

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::time::Duration;

/// Stored as TEXT ('pending', 'paid', ...) and decoded straight into an enum.
#[derive(Debug, Clone, Copy, PartialEq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
enum Status {
    Pending,
    Paid,
    Shipped,
    Refunded,
}

struct Order {
    id: i64,
    customer_email: String,
    total_cents: i64,
    status: Status,
}

async fn connect() -> sqlx::Result<SqlitePool> {
    // Each in-memory SQLite connection is its own database, so keep exactly one alive
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None::<Duration>)
        .max_lifetime(None::<Duration>)
        .connect("sqlite::memory:")
        .await?;

    // Same migrations that built dev.db, embedded into the binary at compile time
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

async fn create_order(pool: &SqlitePool, email: &str, total_cents: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar!(
        "INSERT INTO orders (customer_email, total_cents) VALUES ($1, $2) RETURNING id",
        email,
        total_cents
    )
    .fetch_one(pool)
    .await
}

/// Moves an order from `pending` to `paid`. Returns false if it wasn't pending.
async fn pay_order(pool: &SqlitePool, order_id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query!(
        "UPDATE orders SET status = 'paid' WHERE id = $1 AND status = 'pending'",
        order_id
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

async fn find_paid_order(pool: &SqlitePool, order_id: i64) -> sqlx::Result<Order> {
    // `as "status: Status"` tells the macro to decode the TEXT column into our enum
    sqlx::query_as!(
        Order,
        r#"SELECT id, customer_email, total_cents, status AS "status: Status"
             FROM orders
            WHERE id = $1 AND status = $2"#,
        order_id,
        "paid"
    )
    .fetch_one(pool)
    .await
}

/// Inserts every order or none of them.
async fn import_batch(pool: &SqlitePool, batch: &[(&str, i64)]) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for (email, total_cents) in batch {
        sqlx::query!(
            "INSERT INTO orders (customer_email, total_cents) VALUES ($1, $2)",
            email,
            total_cents
        )
        .execute(&mut *tx)
        .await?; // an error here drops `tx`, which rolls everything back
    }
    tx.commit().await
}

async fn status_report(pool: &SqlitePool) -> sqlx::Result<()> {
    // Aggregates could be NULL as far as SQLite knows; `!` asserts they never are
    let rows = sqlx::query!(
        r#"SELECT status AS "status: Status",
                  COUNT(*)         AS "orders!: i64",
                  SUM(total_cents) AS "revenue_cents!: i64"
             FROM orders
            GROUP BY status
            ORDER BY status"#
    )
    .fetch_all(pool)
    .await?;

    println!("\n{:<10} {:>6} {:>10}", "STATUS", "ORDERS", "REVENUE");
    for row in rows {
        println!(
            "{:<10} {:>6} {:>10}",
            format!("{:?}", row.status),
            row.orders,
            format!("${:.2}", row.revenue_cents as f64 / 100.0)
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> sqlx::Result<()> {
    let pool = connect().await?;

    // Create a few orders
    let ferris = create_order(&pool, "ferris@rust-lang.org", 1999).await?;
    let corro = create_order(&pool, "corro@example.com", 4550).await?;
    let alice = create_order(&pool, "alice@example.com", 899).await?;
    println!("created orders {ferris}, {corro}, {alice}");

    // Pay two of them; paying twice is a no-op
    for id in [ferris, alice, alice] {
        let paid = pay_order(&pool, id).await?;
        println!(
            "pay order {id}: {}",
            if paid { "ok" } else { "already paid" }
        );
    }

    // The query from the original snippet
    let order = find_paid_order(&pool, ferris).await?;
    println!(
        "\nfound order #{} for {}: ${:.2} ({:?})",
        order.id,
        order.customer_email,
        order.total_cents as f64 / 100.0,
        order.status
    );

    // `fetch_one` returns RowNotFound when nothing matches
    match find_paid_order(&pool, corro).await {
        Err(sqlx::Error::RowNotFound) => println!("order {corro} is not paid yet"),
        Ok(_) => println!("order {corro} is unexpectedly paid"),
        Err(err) => return Err(err),
    }

    // Transactions: the second row breaks the CHECK constraint, so neither is saved
    let before = sqlx::query_scalar!("SELECT COUNT(*) FROM orders")
        .fetch_one(&pool)
        .await?;
    let result = import_batch(&pool, &[("bob@example.com", 2500), ("eve@example.com", -1)]).await;
    let after = sqlx::query_scalar!("SELECT COUNT(*) FROM orders")
        .fetch_one(&pool)
        .await?;
    println!(
        "\nbatch import failed ({}), orders before: {before}, after: {after}",
        result.unwrap_err()
    );

    status_report(&pool).await?;
    Ok(())
}
