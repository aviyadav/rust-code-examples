use anyhow::Result;
use sqlx::PgPool;

/// Article §1 DDL, completed (the article's index statement was truncated).
pub const STATEMENTS: &[&str] = &[
    r#"
    CREATE TABLE IF NOT EXISTS notification_deliveries (
        id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
        notification_id VARCHAR(128) NOT NULL,
        channel VARCHAR(32) NOT NULL,
        recipient_id VARCHAR(128) NOT NULL,
        status VARCHAR(32) NOT NULL DEFAULT 'RECEIVED',
        attempts INT NOT NULL DEFAULT 0,
        locked_until TIMESTAMPTZ,
        provider_reference_id VARCHAR(256),
        last_error TEXT,
        created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        CONSTRAINT uq_notification_channel UNIQUE (notification_id, channel)
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_deliveries_claim ON notification_deliveries (status, locked_until)",
    "CREATE INDEX IF NOT EXISTS idx_deliveries_recipient ON notification_deliveries (recipient_id, created_at DESC)",
    r#"
    CREATE TABLE IF NOT EXISTS notification_preferences (
        recipient_id VARCHAR(128) NOT NULL,
        channel VARCHAR(32) NOT NULL,
        opted_out BOOLEAN NOT NULL DEFAULT FALSE,
        updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        PRIMARY KEY (recipient_id, channel)
    )"#,
];

/// Idempotent boot-time migration. Advisory lock serializes concurrent services.
pub async fn ensure_schema(pool: &PgPool) -> Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(912_345_i64)
        .execute(&mut *conn)
        .await?;
    let mut result = Ok(());
    for stmt in STATEMENTS {
        if let Err(e) = sqlx::query(stmt).execute(&mut *conn).await {
            result = Err(e.into());
            break;
        }
    }
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(912_345_i64)
        .execute(&mut *conn)
        .await;
    result
}