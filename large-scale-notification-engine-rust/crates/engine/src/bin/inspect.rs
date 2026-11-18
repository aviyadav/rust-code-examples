use std::time::Duration;

use clap::Parser;
use sqlx::Row;

#[derive(Parser)]
#[command(name = "inspect", about = "Delivery state dashboard (PostgreSQL SSOT)")]
struct Args {
    #[arg(long, help = "refresh every 2 seconds")]
    watch: bool,
}

async fn report(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    println!("\n=== delivery counts by channel/status ===");
    let rows = sqlx::query(
        "SELECT channel, status, COUNT(*) AS n
         FROM notification_deliveries GROUP BY channel, status ORDER BY channel, status",
    )
    .fetch_all(pool)
    .await?;
    println!("{:<8} {:<18} {:>8}", "channel", "status", "count");
    let mut total = 0i64;
    for r in &rows {
        let n: i64 = r.get("n");
        total += n;
        println!("{:<8} {:<18} {:>8}",
            r.get::<String, _>("channel"), r.get::<String, _>("status"), n);
    }
    println!("total: {total}");

    println!("\n=== 15 most recently updated ===");
    let rows = sqlx::query(
        "SELECT notification_id, channel, recipient_id, status, attempts,
                COALESCE(last_error,'') AS last_error, updated_at
         FROM notification_deliveries ORDER BY updated_at DESC LIMIT 15",
    )
    .fetch_all(pool)
    .await?;
    for r in &rows {
        let err: String = r.get("last_error");
        let err: String = err.chars().take(36).collect();
        println!("{:<22} {:<6} {:<16} {:<17} att={} {}",
            r.get::<String, _>("notification_id"),
            r.get::<String, _>("channel"),
            r.get::<String, _>("recipient_id"),
            r.get::<String, _>("status"),
            r.get::<i32, _>("attempts"),
            err);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let url = common::config::env_or("DATABASE_URL", "postgres://notify:notify@localhost:5432/notifications");
    let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
    loop {
        report(&pool).await?;
        if !args.watch { break; }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}