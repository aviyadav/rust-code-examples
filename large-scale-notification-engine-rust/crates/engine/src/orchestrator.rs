use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use common::config::env_or;
use common::{topics, NotificationEvent};
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::message::Message;
use rdkafka::Offset;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::{db, hot::HotTier};

fn render(template_id: &str, payload: &str) -> String {
    let vars: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(payload).unwrap_or_default();
    let tpl = match template_id {
        "order_shipped" => "Hi {{name}}, your order {{order_id}} has shipped!",
        "otp" => "Your verification code is {{code}}. It expires in 5 minutes.",
        _ => "{{body}}",
    };
    let mut out = tpl.to_string();
    for (k, v) in vars {
        let val = match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        out = out.replace(&format!("{{{{{k}}}}}"), &val);
    }
    out
}

#[derive(Default)]
struct Counters {
    routed: AtomicU64,
    suppressed: AtomicU64,
    deduped: AtomicU64,
    poison: AtomicU64,
}

pub async fn run() -> Result<()> {
    let brokers = env_or("KAFKA_BROKERS", "localhost:29092");
    let database_url = env_or(
        "DATABASE_URL",
        "postgres://notify:notify@localhost:5432/notifications",
    );
    let redis_url = env_or("REDIS_URL", "redis://localhost:6379");

    common::kafka::ensure_topics(&brokers).await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    common::schema::ensure_schema(&pool).await?;
    let hot = HotTier::connect(&redis_url).await;
    let producer = common::kafka::new_producer(&brokers)?;

    let consumer: StreamConsumer =
        common::kafka::consumer_config(&brokers, "orchestrator").create()?;
    consumer.subscribe(&[topics::INGEST])?;

    let (sd_tx, mut sd_rx) = watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = sd_tx.send(true);
    });

    let c = Arc::new(Counters::default());
    {
        let c = c.clone();
        let mut sd = sd_rx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = sd.changed() => break,
                    _ = iv.tick() => info!(
                        routed = c.routed.load(Ordering::Relaxed),
                        suppressed = c.suppressed.load(Ordering::Relaxed),
                        deduped = c.deduped.load(Ordering::Relaxed),
                        poison = c.poison.load(Ordering::Relaxed),
                        "orchestrator metrics"
                    ),
                }
            }
        });
    }

    info!("orchestrator started");
    // Sequential per-partition processing => direct commit_message is monotonic here.
    loop {
        let m = tokio::select! {
            _ = sd_rx.changed() => break,
            r = consumer.recv() => r,
        };
        let m = match m {
            Ok(m) => m,
            Err(e) => {
                error!("consume: {e}");
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        };
        let payload = m.payload().unwrap_or(&[]);

        let mut event: NotificationEvent = match serde_json::from_slice(payload) {
            Ok(e) => e,
            Err(err) => {
                c.poison.fetch_add(1, Ordering::Relaxed);
                if common::kafka::send_dlq(
                    &producer,
                    topics::INGEST,
                    m.partition(),
                    m.offset(),
                    &format!("deserialization: {err}"),
                    payload,
                )
                .await
                .is_ok()
                {
                    let _ = consumer.commit_message(&m, CommitMode::Async);
                }
                continue;
            }
        };
        let Some(target) = topics::channel_topic(&event.channel) else {
            c.poison.fetch_add(1, Ordering::Relaxed);
            let _ = common::kafka::send_dlq(
                &producer,
                topics::INGEST,
                m.partition(),
                m.offset(),
                "unknown channel",
                payload,
            )
            .await;
            let _ = consumer.commit_message(&m, CommitMode::Async);
            continue;
        };

        // Preferences / opt-outs. On DB error we degrade open and route anyway;
        // the worker's durable claim remains the correctness boundary.
        match db::is_opted_out(&pool, &event.recipient_id, &event.channel).await {
            Ok(true) => {
                let _ = db::suppress(
                    &pool,
                    &event.notification_id,
                    &event.channel,
                    &event.recipient_id,
                )
                .await;
                c.suppressed.fetch_add(1, Ordering::Relaxed);
                let _ = consumer.commit_message(&m, CommitMode::Async);
                continue;
            }
            Ok(false) => {}
            Err(e) => warn!("opt-out check failed ({e}); routing anyway"),
        }

        // Dedup short-circuit for terminal states only (in-flight may still crash).
        let terminal = if hot
            .recently_success(&event.notification_id, &event.channel)
            .await
        {
            true
        } else {
            match db::status_of(&pool, &event.notification_id, &event.channel).await {
                Ok(Some(s)) => matches!(s.as_str(), "SUCCESS" | "SUPPRESSED" | "DEAD_LETTER"),
                Ok(None) => false,
                Err(e) => {
                    warn!("dedup check failed ({e}); routing anyway");
                    false
                }
            }
        };
        if terminal {
            c.deduped.fetch_add(1, Ordering::Relaxed);
            let _ = consumer.commit_message(&m, CommitMode::Async);
            continue;
        }

        // Template rendering, then route keyed by recipient_id (article §3 ordering).
        event.payload = render(&event.template_id, &event.payload);
        match common::kafka::produce_json(&producer, target, &event.recipient_id, &event).await {
            Ok(()) => {
                c.routed.fetch_add(1, Ordering::Relaxed);
                debug!(id = %event.notification_id, channel = %event.channel, "routed");
                let _ = consumer.commit_message(&m, CommitMode::Async);
            }
            Err(e) => {
                error!("route produce failed: {e}; seeking back for redelivery");
                let _ = consumer.seek(
                    topics::INGEST,
                    m.partition(),
                    Offset::Offset(m.offset()),
                    Duration::from_secs(5),
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
    info!("orchestrator shutdown");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_variables() {
        let out = render("order_shipped", r#"{"name":"Ada","order_id":"ORD-1"}"#);
        assert_eq!(out, "Hi Ada, your order ORD-1 has shipped!");
    }
}
