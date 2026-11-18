use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub enum ClaimResult {
    /// (record_id, attempt_count) — attempt_count doubles as a fencing token.
    Acquired(Uuid, i32),
    AlreadyProcessed,
    /// Seconds until the current owner's lease expires (for scheduled re-check).
    CurrentlyLocked(i64),
}

/// Atomic claim: insert-or-take-over via a single upsert. Takes over only when the
/// row is FAILED_RETRYABLE or the PROCESSING lease has expired (crash recovery).
pub async fn claim_notification_lease(
    pool: &PgPool,
    notification_id: &str,
    channel: &str,
    recipient_id: &str,
    lease_secs: i64,
) -> Result<ClaimResult, sqlx::Error> {
    let row = sqlx::query(
        r#"
        INSERT INTO notification_deliveries
            (notification_id, channel, recipient_id, status, attempts, locked_until, updated_at)
        VALUES ($1, $2, $3, 'PROCESSING', 1, NOW() + ($4 * INTERVAL '1 second'), NOW())
        ON CONFLICT (notification_id, channel) DO UPDATE
        SET status = 'PROCESSING',
            attempts = notification_deliveries.attempts + 1,
            locked_until = NOW() + ($4 * INTERVAL '1 second'),
            updated_at = NOW()
        WHERE notification_deliveries.status = 'FAILED_RETRYABLE'
           OR (notification_deliveries.status = 'PROCESSING'
               AND notification_deliveries.locked_until < NOW())
        RETURNING id, attempts
        "#,
    )
    .bind(notification_id)
    .bind(channel)
    .bind(recipient_id)
    .bind(lease_secs as f64)
    .fetch_optional(pool)
    .await?;

    if let Some(r) = row {
        return Ok(ClaimResult::Acquired(r.get("id"), r.get("attempts")));
    }

    // Upsert matched nothing: classify why (completes the article's truncated match).
    let existing = sqlx::query(
        "SELECT status, locked_until FROM notification_deliveries
         WHERE notification_id = $1 AND channel = $2",
    )
    .bind(notification_id)
    .bind(channel)
    .fetch_optional(pool)
    .await?;

    match existing {
        Some(r) => {
            let status: String = r.get("status");
            if matches!(status.as_str(), "SUCCESS" | "DEAD_LETTER" | "SUPPRESSED") {
                Ok(ClaimResult::AlreadyProcessed)
            } else {
                let until: Option<DateTime<Utc>> = r.get("locked_until");
                let secs = until
                    .map(|t| (t - Utc::now()).num_seconds().max(1))
                    .unwrap_or(5);
                Ok(ClaimResult::CurrentlyLocked(secs))
            }
        }
        None => Ok(ClaimResult::CurrentlyLocked(1)), // raced insert; re-check shortly
    }
}

/// Fenced write: only the attempt that owns the lease may record the outcome.
pub async fn mark_success(
    pool: &PgPool,
    id: Uuid,
    attempts: i32,
    provider_reference_id: &str,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE notification_deliveries
         SET status='SUCCESS', provider_reference_id=$3, locked_until=NULL, updated_at=NOW()
         WHERE id=$1 AND attempts=$2 AND status='PROCESSING'",
    )
    .bind(id)
    .bind(attempts)
    .bind(provider_reference_id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

pub async fn mark_failed_retryable(
    pool: &PgPool,
    id: Uuid,
    attempts: i32,
    error: &str,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE notification_deliveries
         SET status='FAILED_RETRYABLE', last_error=$3, locked_until=NULL, updated_at=NOW()
         WHERE id=$1 AND attempts=$2 AND status='PROCESSING'",
    )
    .bind(id)
    .bind(attempts)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

pub async fn mark_dead_letter(
    pool: &PgPool,
    id: Uuid,
    attempts: i32,
    error: &str,
) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE notification_deliveries
         SET status='DEAD_LETTER', last_error=$3, locked_until=NULL, updated_at=NOW()
         WHERE id=$1 AND attempts=$2 AND status='PROCESSING'",
    )
    .bind(id)
    .bind(attempts)
    .bind(error)
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

pub async fn suppress(
    pool: &PgPool,
    notification_id: &str,
    channel: &str,
    recipient_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO notification_deliveries
            (notification_id, channel, recipient_id, status, attempts, updated_at)
         VALUES ($1,$2,$3,'SUPPRESSED',0,NOW())
         ON CONFLICT (notification_id, channel) DO NOTHING",
    )
    .bind(notification_id)
    .bind(channel)
    .bind(recipient_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn status_of(
    pool: &PgPool,
    notification_id: &str,
    channel: &str,
) -> Result<Option<String>, sqlx::Error> {
    let r = sqlx::query(
        "SELECT status FROM notification_deliveries WHERE notification_id=$1 AND channel=$2",
    )
    .bind(notification_id)
    .bind(channel)
    .fetch_optional(pool)
    .await?;
    Ok(r.map(|row| row.get::<String, _>("status")))
}

pub async fn is_opted_out(
    pool: &PgPool,
    recipient_id: &str,
    channel: &str,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "SELECT opted_out FROM notification_preferences WHERE recipient_id=$1 AND channel=$2",
    )
    .bind(recipient_id)
    .bind(channel)
    .fetch_optional(pool)
    .await?;
    Ok(r.map(|row| row.get::<bool, _>("opted_out")).unwrap_or(false))
}