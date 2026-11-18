use std::time::Duration;

use anyhow::{anyhow, Result};
use rdkafka::admin::{AdminClient, AdminOptions, NewTopic, TopicReplication};
use rdkafka::client::DefaultClientContext;
use rdkafka::error::RDKafkaErrorCode;
use rdkafka::producer::{FutureProducer, FutureRecord};
use rdkafka::ClientConfig;
use serde::Serialize;
use tracing::{debug, info};

use crate::events::DlqEnvelope;

pub mod topics {
    pub const INGEST: &str = "notifications.ingest";
    pub const PUSH: &str = "notifications.push";
    pub const SMS: &str = "notifications.sms";
    pub const EMAIL: &str = "notifications.email";
    pub const RETRY: &str = "notifications.retry";
    pub const DLQ: &str = "notifications.dlq";

    /// (topic, partitions). Created idempotently by every service at boot.
    pub const SPECS: &[(&str, i32)] = &[
        (INGEST, 6),
        (PUSH, 3),
        (SMS, 3),
        (EMAIL, 3),
        (RETRY, 3),
        (DLQ, 1),
    ];

    pub fn channel_topic(channel: &str) -> Option<&'static str> {
        match channel {
            "push" => Some(PUSH),
            "sms" => Some(SMS),
            "email" => Some(EMAIL),
            _ => None,
        }
    }
}

pub fn new_producer(brokers: &str) -> Result<FutureProducer> {
    let p: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("acks", "all")
        .set("enable.idempotence", "true")
        .set("linger.ms", "5")
        .set("message.timeout.ms", "10000")
        .create()?;
    Ok(p)
}

/// Article §3: manual commits only; offsets advance after durable outcomes.
pub fn consumer_config(brokers: &str, group: &str) -> ClientConfig {
    let mut c = ClientConfig::new();
    c.set("bootstrap.servers", brokers)
        .set("group.id", group)
        .set("enable.auto.commit", "false")
        .set("auto.offset.reset", "earliest")
        .set("max.poll.interval.ms", "300000")
        .set("session.timeout.ms", "45000")
        .set("enable.partition.eof", "false");
    c
}

pub async fn ensure_topics(brokers: &str) -> Result<()> {
    let admin: AdminClient<DefaultClientContext> =
        ClientConfig::new().set("bootstrap.servers", brokers).create()?;
    let new: Vec<NewTopic<'_>> = topics::SPECS
        .iter()
        .map(|(name, parts)| NewTopic::new(name, *parts, TopicReplication::Fixed(1)))
        .collect();
    let opts = AdminOptions::new().request_timeout(Some(Duration::from_secs(10)));
    let results = admin.create_topics(new.iter(), &opts).await?;
    for r in results {
        match r {
            Ok(name) => info!(topic = %name, "topic created"),
            Err((name, RDKafkaErrorCode::TopicAlreadyExists)) => debug!(topic = %name, "topic exists"),
            Err((name, code)) => return Err(anyhow!("create topic {name}: {code:?}")),
        }
    }
    Ok(())
}

pub async fn produce_json<T: Serialize>(
    producer: &FutureProducer,
    topic: &str,
    key: &str,
    value: &T,
) -> Result<()> {
    let body = serde_json::to_string(value)?;
    let record = FutureRecord::to(topic).key(key).payload(&body);
    producer
        .send(record, Duration::from_secs(10))
        .await
        .map_err(|(e, _)| anyhow!("produce to {topic}: {e}"))?;
    Ok(())
}

pub async fn produce_raw(
    producer: &FutureProducer,
    topic: &str,
    key: Option<&str>,
    bytes: &[u8],
) -> Result<()> {
    let mut record = FutureRecord::<str, [u8]>::to(topic).payload(bytes);
    if let Some(k) = key {
        record = record.key(k);
    }
    producer
        .send(record, Duration::from_secs(10))
        .await
        .map_err(|(e, _)| anyhow!("produce to {topic}: {e}"))?;
    Ok(())
}

pub async fn send_dlq(
    producer: &FutureProducer,
    source_topic: &str,
    partition: i32,
    offset: i64,
    reason: &str,
    payload: &[u8],
) -> Result<()> {
    let env = DlqEnvelope {
        source_topic: source_topic.to_string(),
        partition,
        offset,
        reason: reason.to_string(),
        payload_utf8_lossy: String::from_utf8_lossy(payload).into_owned(),
        failed_at_epoch_ms: chrono::Utc::now().timestamp_millis(),
    };
    let body = serde_json::to_string(&env)?;
    produce_raw(producer, topics::DLQ, None, body.as_bytes()).await
}