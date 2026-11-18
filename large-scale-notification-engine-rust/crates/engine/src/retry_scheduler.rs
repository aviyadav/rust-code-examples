use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use common::config::env_or;
use common::{topics, RetryEvent};
use rdkafka::consumer::{CommitMode, Consumer, StreamConsumer};
use rdkafka::message::Message;
use rdkafka::producer::FutureProducer;
use rdkafka::{Offset, TopicPartitionList};
use tokio::sync::watch;
use tracing::{error, info, warn};

async fn forward(producer: &FutureProducer, re: &RetryEvent) -> Result<()> {
    let target = topics::channel_topic(&re.event.channel)
        .ok_or_else(|| anyhow::anyhow!("unknown channel {}", re.event.channel))?;
    let mut last = None;
    for attempt in 0..3 {
        match common::kafka::produce_json(producer, target, &re.event.recipient_id, &re.event).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await;
            }
        }
    }
    Err(last.unwrap())
}

pub async fn run() -> Result<()> {
    let brokers = env_or("KAFKA_BROKERS", "localhost:29092");
    common::kafka::ensure_topics(&brokers).await?;
    let producer = Arc::new(common::kafka::new_producer(&brokers)?);
    let consumer: Arc<StreamConsumer> =
        Arc::new(common::kafka::consumer_config(&brokers, "retry-scheduler").create()?);
    consumer.subscribe(&[topics::RETRY])?;

    let (sd_tx, mut sd_rx) = watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = sd_tx.send(true);
    });

    info!("retry scheduler started");
    loop {
        let m = tokio::select! {
            _ = sd_rx.changed() => break,
            r = consumer.recv() => r,
        };
        let m = match m {
            Ok(m) => m,
            Err(e) => { error!("consume: {e}"); tokio::time::sleep(Duration::from_millis(250)).await; continue; }
        };
        let payload = m.payload().unwrap_or(&[]);
        let re: RetryEvent = match serde_json::from_slice(payload) {
            Ok(r) => r,
            Err(err) => {
                let _ = common::kafka::send_dlq(&producer, topics::RETRY, m.partition(), m.offset(),
                    &format!("retry deserialization: {err}"), payload).await;
                let _ = consumer.commit_message(&m, CommitMode::Async);
                continue;
            }
        };

        let due_in_ms = re.retry_at_epoch_ms - Utc::now().timestamp_millis();
        if due_in_ms <= 0 {
            match forward(&producer, &re).await {
                Ok(()) => { let _ = consumer.commit_message(&m, CommitMode::Async); }
                Err(e) => {
                    error!("forward failed: {e}; seeking back");
                    let _ = consumer.seek(topics::RETRY, m.partition(), Offset::Offset(m.offset()), Duration::from_secs(5));
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        } else {
            // Pause only this partition; others keep flowing. One outstanding timer
            // per paused partition => per-partition commits stay monotonic.
            let (p, o) = (m.partition(), m.offset());
            let mut tpl = TopicPartitionList::new();
            let _ = tpl.add_partition(topics::RETRY, p);
            if let Err(e) = consumer.pause(&tpl) {
                warn!("pause failed: {e}");
                continue;
            }
            let producer = producer.clone();
            let consumer = consumer.clone();
            let delay = Duration::from_millis((due_in_ms as u64).min(300_000));
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                match forward(&producer, &re).await {
                    Ok(()) => {
                        let mut commit = TopicPartitionList::new();
                        let _ = commit.add_partition_offset(topics::RETRY, p, Offset::Offset(o + 1));
                        let _ = consumer.commit(&commit, CommitMode::Async);
                    }
                    Err(e) => error!("delayed forward failed: {e}; offset uncommitted => redelivery"),
                }
                let _ = consumer.resume(&tpl);
            });
        }
    }
    info!("retry scheduler shutdown (pending timers dropped; offsets redeliver)");
    Ok(())
}