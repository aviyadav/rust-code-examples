//! Synthetic load generator. Exercises every path in the article:
//! normal traffic, duplicate notification_ids, poison pills (raw invalid JSON to Kafka),
//! permanently-invalid recipients, and seeded opt-outs.

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use common::{topics, NotificationEvent};
use rand::Rng;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "generator", about = "Generate notification load")]
struct Args {
    #[arg(long, default_value_t = 1000)]
    count: u64,
    /// events/sec; 0 = unthrottled burst
    #[arg(long, default_value_t = 200)]
    rate: u64,
    #[arg(long, default_value_t = 5)]
    tenants: u32,
    #[arg(long, default_value_t = 200)]
    recipients: u32,
    #[arg(long, default_value = "push,sms,email")]
    channels: String,
    /// fraction resending an earlier notification_id (dedup demo)
    #[arg(long, default_value_t = 0.05)]
    duplicate_ratio: f64,
    /// fraction of raw invalid-JSON records produced directly to Kafka (DLQ demo)
    #[arg(long, default_value_t = 0.01)]
    poison_ratio: f64,
    /// fraction with recipient "invalid:*" (provider 400 => DEAD_LETTER demo)
    #[arg(long, default_value_t = 0.02)]
    bad_recipient_ratio: f64,
    /// api (through the Axum gateway) or kafka (direct to ingest topic)
    #[arg(long, default_value = "api")]
    via: String,
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    api_url: String,
    /// seed N recipients (user-0000..) as opted out of email
    #[arg(long, default_value_t = 0)]
    seed_opt_outs: u32,
    /// fixed prefix => reruns reuse ids (cross-run dedup demo)
    #[arg(long)]
    id_prefix: Option<String>,
}

#[derive(Serialize, Clone)]
struct ApiEvent {
    notification_id: String,
    tenant_id: String,
    channel: String,
    recipient_id: String,
    template_id: String,
    payload: serde_json::Value,
}

fn make_event(args: &Args, channels: &[String], prefix: &str, i: u64) -> (NotificationEvent, serde_json::Value) {
    let mut rng = rand::thread_rng();
    let channel = channels[rng.gen_range(0..channels.len())].clone();
    let r = rng.gen_range(0..args.recipients.max(1));
    let recipient = if rng.gen::<f64>() < args.bad_recipient_ratio {
        format!("invalid:user-{r:04}")
    } else {
        format!("user-{r:04}")
    };
    let tenant = format!("tenant-{:02}", rng.gen_range(0..args.tenants.max(1)));
    let (template, vars) = match channel.as_str() {
        "push" => ("order_shipped", json!({ "name": format!("User {r}"), "order_id": format!("ORD-{i:06}") })),
        "sms" => ("otp", json!({ "code": rng.gen_range(100000..999999) })),
        _ => ("promo", json!({ "body": format!("Weekly digest #{i} for {recipient}") })),
    };
    let event = NotificationEvent {
        notification_id: format!("{prefix}-{i:06}"),
        tenant_id: tenant,
        channel,
        recipient_id: recipient,
        template_id: template.to_string(),
        payload: vars.to_string(),
    };
    (event, vars)
}

async fn flush_batch(client: &reqwest::Client, url: &str, batch: &[ApiEvent]) -> Result<()> {
    let mut last = None;
    for attempt in 0..3 {
        match client.post(format!("{url}/v1/notifications/batch")).json(batch).send().await {
            Ok(r) if r.status().is_success() => return Ok(()),
            Ok(r) => last = Some(anyhow::anyhow!("API {}", r.status())),
            Err(e) => last = Some(e.into()),
        }
        tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
    }
    Err(last.unwrap())
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let channels: Vec<String> = args.channels.split(',').map(|s| s.trim().to_string()).collect();
    for c in &channels {
        anyhow::ensure!(common::valid_channel(c), "invalid channel in --channels: {c}");
    }
    anyhow::ensure!(args.via == "api" || args.via == "kafka", "--via must be api or kafka");

    let prefix = args.id_prefix.clone()
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string()[..8].to_string());
    let brokers = common::config::env_or("KAFKA_BROKERS", "localhost:29092");

    // Kafka producer needed for direct mode and/or poison pills.
    let producer = if args.via == "kafka" || args.poison_ratio > 0.0 {
        common::kafka::ensure_topics(&brokers).await?;
        Some(common::kafka::new_producer(&brokers)?)
    } else { None };

    // Seed opt-outs directly (data-generation responsibility).
    if args.seed_opt_outs > 0 {
        let url = common::config::env_or("DATABASE_URL", "postgres://notify:notify@localhost:5432/notifications");
        let pool = sqlx::postgres::PgPoolOptions::new().max_connections(2).connect(&url).await?;
        common::schema::ensure_schema(&pool).await?;
        for r in 0..args.seed_opt_outs {
            sqlx::query(
                "INSERT INTO notification_preferences (recipient_id, channel, opted_out)
                 VALUES ($1,'email',TRUE)
                 ON CONFLICT (recipient_id, channel) DO UPDATE SET opted_out=TRUE, updated_at=NOW()",
            )
            .bind(format!("user-{r:04}"))
            .execute(&pool)
            .await?;
        }
        println!("seeded {} email opt-outs (user-0000..)", args.seed_opt_outs);
    }

    let client = reqwest::Client::builder().timeout(Duration::from_secs(15)).build()?;
    let mut history: Vec<NotificationEvent> = Vec::new();
    let mut batch: Vec<ApiEvent> = Vec::new();
    let (mut produced, mut dups, mut poison) = (0u64, 0u64, 0u64);
    let batch_size = 100u64;
    let pace = |n: u64| -> Option<Duration> {
        if args.rate == 0 { None } else { Some(Duration::from_secs_f64(n as f64 / args.rate as f64)) }
    };
    let mut event_iv = pace(1).map(tokio::time::interval);
    let mut batch_iv = pace(batch_size).map(tokio::time::interval);

    println!("generating {} events (prefix={prefix}, via={}, rate={}/s)...", args.count, args.via, args.rate);

    for i in 0..args.count {
        let mut rng = rand::thread_rng();

        if rng.gen::<f64>() < args.poison_ratio {
            if let Some(p) = &producer {
                common::kafka::produce_raw(p, topics::INGEST, None, b"{this is not valid json").await?;
                poison += 1;
            }
            continue;
        }

        let event = if !history.is_empty() && rng.gen::<f64>() < args.duplicate_ratio {
            let e = history[rng.gen_range(0..history.len())].clone();
            dups += 1;
            e
        } else {
            let (e, _) = make_event(&args, &channels, &prefix, i);
            e
        };
        history.push(event.clone());
        if history.len() > 1000 { history.remove(0); }

        match args.via.as_str() {
            "kafka" => {
                if let Some(iv) = &mut event_iv { iv.tick().await; }
                let p = producer.as_ref().unwrap();
                common::kafka::produce_json(p, topics::INGEST, &event.recipient_id, &event).await?;
                produced += 1;
            }
            _ => {
                batch.push(ApiEvent {
                    notification_id: event.notification_id,
                    tenant_id: event.tenant_id,
                    channel: event.channel,
                    recipient_id: event.recipient_id,
                    template_id: event.template_id,
                    payload: serde_json::from_str(&event.payload).unwrap_or(json!({})),
                });
                if batch.len() as u64 >= batch_size {
                    if let Some(iv) = &mut batch_iv { iv.tick().await; }
                    flush_batch(&client, &args.api_url, &batch).await?;
                    produced += batch.len() as u64;
                    batch.clear();
                }
            }
        }
    }
    if !batch.is_empty() {
        flush_batch(&client, &args.api_url, &batch).await?;
        produced += batch.len() as u64;
    }

    println!("\ndone: produced={produced} (duplicates={dups}), poison={poison}, prefix={prefix}");
    println!("check progress:  ./scripts/status.sh");
    println!("provider stats:  curl -s localhost:8081/stats");
    println!("trace one:       curl -s localhost:8080/v1/notifications/{prefix}-000001");
    Ok(())
}