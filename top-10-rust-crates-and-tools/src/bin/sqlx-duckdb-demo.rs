//! sqlx writes, DuckDB analyzes: both on the same SQLite file.
//!
//! sqlx has no DuckDB driver, but DuckDB's `sqlite` extension can ATTACH a SQLite
//! database and query it directly. So the app uses sqlx (async, compile-time
//! checked) for its transactional writes, and DuckDB for the heavy reporting
//! queries, without copying any data between them.

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

const DB_PATH: &str = "orders-demo.db";

const CUSTOMERS: [&str; 6] = [
    "ferris@rust-lang.org",
    "corro@example.com",
    "alice@example.com",
    "bob@example.com",
    "carol@shop.io",
    "dave@shop.io",
];
const STATUSES: [&str; 5] = ["pending", "paid", "paid", "shipped", "refunded"];

// ---------- sqlx side: the app's write path ----------

async fn open_sqlite(path: &str) -> sqlx::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

/// Inserts `count` orders in one transaction, using deterministic fake data.
async fn seed_orders(pool: &SqlitePool, count: usize) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    for i in 0..count {
        let email = CUSTOMERS[(i * 7) % CUSTOMERS.len()];
        let total_cents = 500 + ((i * 7919) % 9500) as i64;
        let status = STATUSES[(i * 3) % STATUSES.len()];
        sqlx::query!(
            "INSERT INTO orders (customer_email, total_cents, status) VALUES ($1, $2, $3)",
            email,
            total_cents,
            status
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

// ---------- DuckDB side: the analytics path ----------

fn dollars(cents: f64) -> String {
    format!("${:.2}", cents / 100.0)
}

fn report(path: &str) -> duckdb::Result<()> {
    let duck = duckdb::Connection::open_in_memory()?;

    // INSTALL downloads the extension the first time (cached afterwards)
    duck.execute_batch(&format!(
        "INSTALL sqlite;
         LOAD sqlite;
         ATTACH '{path}' AS app (TYPE sqlite, READ_ONLY);"
    ))?;

    // Revenue per status, with a median. `GROUP BY ALL` and `quantile_cont` are DuckDB-isms
    println!("\n== Revenue by status (DuckDB over SQLite) ==");
    println!(
        "{:<10} {:>6} {:>12} {:>10}",
        "STATUS", "ORDERS", "REVENUE", "MEDIAN"
    );
    let mut stmt = duck.prepare(
        "SELECT status,
                count(*)                            AS orders,
                sum(total_cents)::DOUBLE            AS revenue,
                quantile_cont(total_cents, 0.5)     AS median
           FROM app.orders
          GROUP BY ALL
          ORDER BY revenue DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    for row in rows {
        let (status, orders, revenue, median) = row?;
        println!(
            "{status:<10} {orders:>6} {:>12} {:>10}",
            dollars(revenue),
            dollars(median)
        );
    }

    // Per email domain: `split_part` and `arg_max` (who placed the single biggest order)
    println!("\n== Revenue by customer domain (excluding refunds) ==");
    println!(
        "{:<14} {:>9} {:>12}  BIGGEST ORDER BY",
        "DOMAIN", "CUSTOMERS", "REVENUE"
    );
    let mut stmt = duck.prepare(
        "SELECT split_part(customer_email, '@', 2)       AS domain,
                count(DISTINCT customer_email)           AS customers,
                sum(total_cents)::DOUBLE                 AS revenue,
                arg_max(customer_email, total_cents)     AS biggest_order_by
           FROM app.orders
          WHERE status <> 'refunded'
          GROUP BY ALL
          ORDER BY revenue DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (domain, customers, revenue, top) = row?;
        println!(
            "{domain:<14} {customers:>9} {:>12}  {top}",
            dollars(revenue)
        );
    }

    // Window function: each customer's rank and share of total revenue
    println!("\n== Customer leaderboard ==");
    println!(
        "{:<4} {:<22} {:>12} {:>7}",
        "RANK", "CUSTOMER", "REVENUE", "SHARE"
    );
    let mut stmt = duck.prepare(
        "SELECT rank() OVER (ORDER BY sum(total_cents) DESC)                 AS rank,
                customer_email,
                sum(total_cents)::DOUBLE                                     AS revenue,
                100.0 * sum(total_cents) / sum(sum(total_cents)) OVER ()     AS share
           FROM app.orders
          WHERE status <> 'refunded'
          GROUP BY customer_email
          ORDER BY rank",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, f64>(3)?,
        ))
    })?;
    for row in rows {
        let (rank, email, revenue, share) = row?;
        println!(
            "{rank:<4} {email:<22} {:>12} {share:>6.1}%",
            dollars(revenue)
        );
    }

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Start fresh each run
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{DB_PATH}{suffix}"));
    }

    // 1. sqlx: create the schema and write orders
    let pool = open_sqlite(DB_PATH)
        .await
        .context("failed to open SQLite")?;
    seed_orders(&pool, 200)
        .await
        .context("failed to seed orders")?;
    let count = sqlx::query_scalar!("SELECT COUNT(*) FROM orders")
        .fetch_one(&pool)
        .await?;
    println!("sqlx wrote {count} orders to {DB_PATH}");

    // Closing the pool flushes the write-ahead log into the main file for DuckDB
    pool.close().await;

    // 2. DuckDB: analyze the same file. Its API is blocking, so keep it off the async threads
    tokio::task::spawn_blocking(|| report(DB_PATH))
        .await?
        .context("DuckDB report failed")?;

    Ok(())
}
