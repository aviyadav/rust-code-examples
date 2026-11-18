use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Utc;
use common::config::{env_or, env_parse};
use common::{topics, NotificationEvent, RetryEvent};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use rand::Rng;
use rdkafka::client::ClientContext;
use rdkafka::consumer::{
    BaseConsumer, CommitMode, Consumer, ConsumerContext, Rebalance, StreamConsumer,
};
use rdkafka::message::Message;
use rdkafka::producer::{FutureProducer, Producer};
use rdkafka::{Offset, TopicPartitionList};
use reqwest::Client;
use sqlx::PgPool;
use tokio::sync::{mpsc, watch, Semaphore};
use tracing::{debug, error, info, warn};

use crate::bump;
use crate::db::{self, ClaimResult};
use crate::hot::HotTier;
use crate::metrics::Metrics;
use crate::provider_client::{self, ProviderError};
use crate::watermark::{TrackerEvent, WatermarkTracker};

pub struct WorkerConfig {
    pub channel: String,
    pub group: String,
    pub brokers: String,
    pub database_url: String,
    pub redis_url: String,
    pub provider_url: String,
    pub concurrency: usize,
    pub rps: u32,
    pub tenant_rps: u32,
    pub lease_secs: i64,
    pub http_timeout_secs: u64,
    pub channel_cap: usize,
    pub max_attempts: i32,
}

impl WorkerConfig {
    pub fn from_env(channel: &str) -> Self {
        Self {
            channel: channel.to_string(),
            group: env_or("WORKER_GROUP", &format!("workers-{channel}")),
            brokers: env_or("KAFKA_BROKERS", "localhost:29092"),
            database_url: env_or(
                "DATABASE_URL",
                "postgres://notify:notify@localhost:5432/notifications",
            ),
            redis_url: env_or("REDIS_URL", "redis://localhost:6379"),
            provider_url: env_or("PROVIDER_URL", "http://127.0.0.1:8081/v1/send"),
            concurrency: env_parse("PROVIDER_CONCURRENCY", 200),
            rps: env_parse("PROVIDER_RPS", 50),
            tenant_rps: env_parse("TENANT_RPS", 1000),
            lease_secs: env_parse("LEASE_SECS", 30),
            http_timeout_secs: env_parse("HTTP_TIMEOUT_SECS", 3),
            channel_cap: env_parse("JOB_CHANNEL_CAP", 500),
            max_attempts: env_parse("MAX_ATTEMPTS", 8),
        }
    }
}

struct Job {
    event: NotificationEvent,
    partition: i32,
    offset: i64,
}

struct RawMsg {
    partition: i32,
    offset: i64,
    payload: Vec<u8>,
}

struct JobCtx {
    topic: String,
    pool: PgPool,
    hot: HotTier,
    http: Client,
    producer: FutureProducer,
    provider_url: String,
    semaphore: Arc<Semaphore>,
    limiter: Arc<DefaultDirectRateLimiter>,
    metrics: Arc<Metrics>,
    cfg_leases: i64,
    tenant_rps: u32,
    max_attempts: i32,
}

/// Rebalance hooks (article §4): flush contiguous watermarks on revoke, reset on assign.
struct WorkerContext {
    tracker: Arc<WatermarkTracker>,
    topic: String,
}
impl ClientContext for WorkerContext {}
impl ConsumerContext for WorkerContext {
    fn pre_rebalance<'a>(&self, base: &BaseConsumer<Self>, rebalance: &Rebalance<'a>) {
        if let Rebalance::Revoke(tpl) = rebalance {
            let parts: Vec<i32> = tpl
                .elements()
                .iter()
                .filter(|e| e.topic() == self.topic)
                .map(|e| e.partition())
                .collect();
            let mut flush = TopicPartitionList::new();
            for p in &parts {
                if let Some(next) = self.tracker.flush_target(*p) {
                    let _ = flush.add_partition_offset(&self.topic, *p, Offset::Offset(next));
                }
            }
            if flush.count() > 0 {
                if let Err(e) = base.commit(&flush, CommitMode::Sync) {
                    warn!("rebalance flush commit failed: {e}");
                }
            }
            self.tracker.revoke(&parts);
            info!(?parts, "partitions revoked; watermarks flushed");
        }
    }

    fn post_rebalance<'a>(&self, _base: &BaseConsumer<Self>, rebalance: &Rebalance<'a>) {
        if let Rebalance::Assign(tpl) = rebalance {
            let parts: Vec<i32> = tpl
                .elements()
                .iter()
                .filter(|e| e.topic() == self.topic)
                .map(|e| e.partition())
                .collect();
            self.tracker.revoke(&parts); // fresh start; redelivery re-dispatches
            info!(?parts, "partitions assigned");
        }
    }
}

type WConsumer = StreamConsumer<WorkerContext>;

/// Article §3 backoff formula: base + rand(0..=base), capped exponent.
pub fn backoff_secs(attempts: i32) -> u64 {
    let base = 2u64.pow(attempts.clamp(1, 6) as u32);
    base + rand::thread_rng().gen_range(0..=base)
}

fn complete(tx: &mpsc::UnboundedSender<TrackerEvent>, job: &Job) {
    let _ = tx.send(TrackerEvent::Completed(job.partition, job.offset));
}

async fn schedule_retry(
    ctx: &JobCtx,
    event: &NotificationEvent,
    attempt: i32,
    delay_secs: u64,
) -> bool {
    let re = RetryEvent {
        event: event.clone(),
        attempt,
        retry_at_epoch_ms: Utc::now().timestamp_millis() + (delay_secs as i64) * 1000,
    };
    match common::kafka::produce_json(&ctx.producer, topics::RETRY, &event.notification_id, &re)
        .await
    {
        Ok(()) => {
            bump!(ctx.metrics, retries_scheduled);
            true
        }
        Err(e) => {
            error!("retry produce failed: {e}; offset left uncommitted for redelivery");
            false
        }
    }
}

async fn process_job(ctx: Arc<JobCtx>, job: Job, tx: mpsc::UnboundedSender<TrackerEvent>) {
    bump!(ctx.metrics, jobs);
    let e = &job.event;

    // 1) Hot-tier dedup hint (never authoritative).
    if ctx
        .hot
        .recently_success(&e.notification_id, &e.channel)
        .await
    {
        bump!(ctx.metrics, hot_dedup);
        complete(&tx, &job);
        return;
    }

    // 2) Durable atomic claim (article §1).
    let (db_id, attempts) = match db::claim_notification_lease(
        &ctx.pool,
        &e.notification_id,
        &e.channel,
        &e.recipient_id,
        ctx.cfg_leases,
    )
    .await
    {
        Ok(ClaimResult::Acquired(id, a)) => (id, a),
        Ok(ClaimResult::AlreadyProcessed) => {
            bump!(ctx.metrics, db_dedup);
            complete(&tx, &job);
            return;
        }
        Ok(ClaimResult::CurrentlyLocked(secs)) => {
            // Improvement over the article: instead of holding the offset hostage
            // (which stalls the watermark), convert contention into a scheduled
            // re-check after the lease deadline. At-least-once is preserved.
            bump!(ctx.metrics, locked);
            let delay = secs as u64 + rand::thread_rng().gen_range(0..=2);
            if schedule_retry(&ctx, e, 0, delay).await {
                complete(&tx, &job);
            }
            return;
        }
        Err(err) => {
            bump!(ctx.metrics, db_errors);
            error!("claim failed: {err}; scheduling infra retry");
            if schedule_retry(&ctx, e, 0, 5).await {
                complete(&tx, &job);
            }
            return;
        }
    };

    if attempts > ctx.max_attempts {
        bump!(ctx.metrics, dead_letter);
        let _ = db::mark_dead_letter(&ctx.pool, db_id, attempts, "max attempts exceeded").await;
        let _ = common::kafka::send_dlq(
            &ctx.producer,
            &ctx.topic,
            job.partition,
            job.offset,
            "max attempts exceeded",
            e.payload.as_bytes(),
        )
        .await;
        complete(&tx, &job);
        return;
    }

    // 3) Tenant window limiter (Redis; degrades open).
    if ctx.tenant_rps > 0 && !ctx.hot.tenant_allow(&e.tenant_id, ctx.tenant_rps).await {
        if schedule_retry(&ctx, e, attempts, 1).await {
            complete(&tx, &job);
        }
        return;
    }

    // 4) Explicit concurrency barriers (article §3): global rate + in-flight permits.
    ctx.limiter.until_ready().await;
    let _permit = match ctx.semaphore.acquire().await {
        Ok(p) => p,
        Err(_) => {
            if schedule_retry(&ctx, e, attempts, 1).await {
                complete(&tx, &job);
            }
            return;
        }
    };

    // 5) Transport.
    let started = Instant::now();
    let outcome = provider_client::dispatch(&ctx.http, &ctx.provider_url, e).await;
    ctx.metrics
        .observe_latency(started.elapsed().as_millis() as u64);

    match outcome {
        Ok(resp) => {
            if resp.dedup_replayed {
                bump!(ctx.metrics, provider_replays);
            }
            match db::mark_success(&ctx.pool, db_id, attempts, &resp.provider_reference_id).await {
                Ok(0) => {
                    // Fencing: our lease was superseded mid-flight; the newer attempt owns the outcome.
                    bump!(ctx.metrics, fenced);
                    warn!(id = %e.notification_id, "success fenced (lease superseded)");
                    complete(&tx, &job);
                }
                Ok(_) => {
                    bump!(ctx.metrics, success);
                    ctx.hot.mark_success(&e.notification_id, &e.channel).await;
                    complete(&tx, &job);
                }
                Err(err) => {
                    // Provider succeeded but SSOT write failed. Safe to retry: the
                    // stable idempotency key makes the provider replay the result.
                    bump!(ctx.metrics, db_errors);
                    error!("mark_success failed: {err}; scheduling idempotent replay");
                    if schedule_retry(&ctx, e, attempts, ctx.cfg_leases as u64).await {
                        complete(&tx, &job);
                    }
                }
            }
        }
        Err(ProviderError::Transient(err)) => {
            bump!(ctx.metrics, transient);
            let _ = db::mark_failed_retryable(&ctx.pool, db_id, attempts, &err).await;
            let delay = backoff_secs(attempts);
            debug!(id = %e.notification_id, attempts, delay, "transient failure: {err}");
            if schedule_retry(&ctx, e, attempts, delay).await {
                complete(&tx, &job);
            }
        }
        Err(ProviderError::Permanent(err)) => {
            bump!(ctx.metrics, permanent);
            let _ = db::mark_dead_letter(&ctx.pool, db_id, attempts, &err).await;
            let _ = common::kafka::send_dlq(
                &ctx.producer,
                &ctx.topic,
                job.partition,
                job.offset,
                &format!("permanent provider error: {err}"),
                e.payload.as_bytes(),
            )
            .await;
            complete(&tx, &job);
        }
    }
}

/// Bounded dispatch with pause/resume backpressure: polling (and thus heartbeats)
/// continues even when the internal queue is full (article §4, latency spike).
async fn submit_job(
    consumer: &Arc<WConsumer>,
    job_tx: &mpsc::Sender<Job>,
    sd: &mut watch::Receiver<bool>,
    stash: &mut VecDeque<RawMsg>,
    job: Job,
    metrics: &Metrics,
) -> Result<()> {
    match job_tx.try_send(job) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(anyhow!("dispatcher channel closed")),
        Err(mpsc::error::TrySendError::Full(job)) => {
            bump!(metrics, backpressure);
            let assignment = consumer.assignment()?;
            consumer.pause(&assignment)?;
            let mut pending = Some(job);
            let result = loop {
                tokio::select! {
                    _ = sd.changed() => break Ok(()), // pending job dropped; offset uncommitted => redelivery
                    r = job_tx.reserve() => match r {
                        Ok(permit) => { permit.send(pending.take().unwrap()); break Ok(()); }
                        Err(_) => break Err(anyhow!("dispatcher channel closed")),
                    },
                    r = consumer.recv() => {
                        // Keeps polling alive while paused; stash any prefetch race.
                        if let Ok(m) = r {
                            stash.push_back(RawMsg {
                                partition: m.partition(),
                                offset: m.offset(),
                                payload: m.payload().unwrap_or(&[]).to_vec(),
                            });
                        }
                    }
                }
            };
            let current = consumer.assignment()?;
            let _ = consumer.resume(&current);
            result
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_raw(
    ctx: &Arc<JobCtx>,
    consumer: &Arc<WConsumer>,
    job_tx: &mpsc::Sender<Job>,
    evt_tx: &mpsc::UnboundedSender<TrackerEvent>,
    sd: &mut watch::Receiver<bool>,
    stash: &mut VecDeque<RawMsg>,
    raw: RawMsg,
) -> Result<()> {
    let event: NotificationEvent = match serde_json::from_slice(&raw.payload) {
        Ok(e) => e,
        Err(err) => {
            // Poison pill (article §4): DLQ + advance offset so the partition never stalls.
            bump!(ctx.metrics, poison);
            if let Err(e) = common::kafka::send_dlq(
                &ctx.producer,
                &ctx.topic,
                raw.partition,
                raw.offset,
                &format!("deserialization: {err}"),
                &raw.payload,
            )
            .await
            {
                error!("DLQ produce failed: {e}; offset left uncommitted (no data loss)");
                return Ok(());
            }
            let _ = evt_tx.send(TrackerEvent::Dispatched(raw.partition, raw.offset));
            let _ = evt_tx.send(TrackerEvent::Completed(raw.partition, raw.offset));
            return Ok(());
        }
    };
    let _ = evt_tx.send(TrackerEvent::Dispatched(raw.partition, raw.offset));
    let job = Job {
        event,
        partition: raw.partition,
        offset: raw.offset,
    };
    submit_job(consumer, job_tx, sd, stash, job, &ctx.metrics).await
}

pub async fn run(cfg: WorkerConfig) -> Result<()> {
    let topic = topics::channel_topic(&cfg.channel)
        .ok_or_else(|| anyhow!("invalid channel {}", cfg.channel))?
        .to_string();

    common::kafka::ensure_topics(&cfg.brokers).await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&cfg.database_url)
        .await?;
    common::schema::ensure_schema(&pool).await?;
    let hot = HotTier::connect(&cfg.redis_url).await;
    let producer = common::kafka::new_producer(&cfg.brokers)?;
    let http = Client::builder()
        .timeout(Duration::from_secs(cfg.http_timeout_secs))
        .pool_max_idle_per_host(50)
        .build()?;

    let metrics = Arc::new(Metrics::default());
    let tracker = Arc::new(WatermarkTracker::default());
    let ctx = Arc::new(JobCtx {
        topic: topic.clone(),
        pool,
        hot,
        http,
        producer,
        provider_url: cfg.provider_url.clone(),
        semaphore: Arc::new(Semaphore::new(cfg.concurrency)),
        limiter: Arc::new(RateLimiter::direct(Quota::per_second(
            std::num::NonZeroU32::new(cfg.rps.max(1)).unwrap(),
        ))),
        metrics: metrics.clone(),
        cfg_leases: cfg.lease_secs,
        tenant_rps: cfg.tenant_rps,
        max_attempts: cfg.max_attempts,
    });

    let consumer: Arc<WConsumer> = Arc::new(
        common::kafka::consumer_config(&cfg.brokers, &cfg.group).create_with_context(
            WorkerContext {
                tracker: tracker.clone(),
                topic: topic.clone(),
            },
        )?,
    );
    consumer.subscribe(&[&topic])?;

    let (sd_tx, mut sd_rx) = watch::channel(false);
    {
        let sd_tx = sd_tx.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = sd_tx.send(true);
        });
    }

    // Single committer task: serialized watermark advancement => monotonic commits.
    let (evt_tx, mut evt_rx) = mpsc::unbounded_channel::<TrackerEvent>();
    {
        let consumer = consumer.clone();
        let tracker = tracker.clone();
        let topic = topic.clone();
        tokio::spawn(async move {
            while let Some(ev) = evt_rx.recv().await {
                let target = match ev {
                    TrackerEvent::Dispatched(p, o) => {
                        tracker.dispatched(p, o);
                        None
                    }
                    TrackerEvent::Completed(p, o) => tracker.completed(p, o).map(|n| (p, n)),
                };
                if let Some((p, next)) = target {
                    let mut tpl = TopicPartitionList::new();
                    let _ = tpl.add_partition_offset(&topic, p, Offset::Offset(next));
                    let _ = consumer.commit(&tpl, CommitMode::Async);
                }
            }
        });
    }

    // Bounded internal queue + per-job tasks (bounded by channel cap + semaphore).
    let (job_tx, mut job_rx) = mpsc::channel::<Job>(cfg.channel_cap);
    {
        let ctx = ctx.clone();
        let evt_tx = evt_tx.clone();
        tokio::spawn(async move {
            while let Some(job) = job_rx.recv().await {
                let ctx = ctx.clone();
                let tx = evt_tx.clone();
                tokio::spawn(async move {
                    process_job(ctx, job, tx).await;
                });
            }
        });
    }

    // Periodic metrics.
    {
        let metrics = metrics.clone();
        let channel = cfg.channel.clone();
        let mut sd = sd_rx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = sd.changed() => break,
                    _ = iv.tick() => metrics.log_line(&channel),
                }
            }
        });
    }

    info!(channel = %cfg.channel, group = %cfg.group, topic = %topic, "worker started");

    let mut stash: VecDeque<RawMsg> = VecDeque::new();
    loop {
        while let Some(raw) = stash.pop_front() {
            handle_raw(
                &ctx, &consumer, &job_tx, &evt_tx, &mut sd_rx, &mut stash, raw,
            )
            .await?;
        }
        let msg = tokio::select! {
            _ = sd_rx.changed() => break,
            r = consumer.recv() => r,
        };
        match msg {
            Ok(m) => {
                let raw = RawMsg {
                    partition: m.partition(),
                    offset: m.offset(),
                    payload: m.payload().unwrap_or(&[]).to_vec(),
                };
                handle_raw(
                    &ctx, &consumer, &job_tx, &evt_tx, &mut sd_rx, &mut stash, raw,
                )
                .await?;
            }
            Err(e) => {
                error!("kafka consume error: {e}");
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }

    // Graceful shutdown: flush contiguous watermarks synchronously.
    let mut tpl = TopicPartitionList::new();
    for (p, next) in tracker.flush_targets() {
        let _ = tpl.add_partition_offset(&topic, p, Offset::Offset(next));
    }
    if tpl.count() > 0 {
        let _ = consumer.commit(&tpl, CommitMode::Sync);
    }
    consumer.unsubscribe();
    if let Err(e) = ctx.producer.flush(Duration::from_secs(5)) {
        warn!("producer flush failed on shutdown: {e}");
    }
    metrics.log_line(&cfg.channel);
    info!("worker shutdown complete (in-flight leases expire and are reclaimed)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::backoff_secs;

    #[test]
    fn backoff_bounds() {
        for _ in 0..100 {
            let b = backoff_secs(1);
            assert!((2..=4).contains(&b));
            let b = backoff_secs(10);
            assert!((64..=128).contains(&b));
        }
    }
}
