use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use custom_tracing_subscriber::{CustomSubscriber, OutputFormat, SubscriberConfig};
use serde_json::{Value, json};
use tracing::field::Empty;
use tracing::subscriber::with_default;
use tracing::{Level, debug, info, info_span, warn};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).expect("log output should be UTF-8")
    }

    fn lines(&self) -> Vec<String> {
        self.text().lines().map(str::to_owned).collect()
    }
}

fn new_subscriber(config: SubscriberConfig) -> (Arc<CustomSubscriber>, Capture) {
    let capture = Capture::default();
    let subscriber = Arc::new(CustomSubscriber::with_writer(config, capture.clone()));
    (subscriber, capture)
}

fn text_config() -> SubscriberConfig {
    SubscriberConfig {
        format: OutputFormat::Text,
        include_timestamps: false,
        ..Default::default()
    }
}

fn json_config() -> SubscriberConfig {
    SubscriberConfig {
        format: OutputFormat::Json,
        include_timestamps: false,
        ..Default::default()
    }
}

#[test]
fn text_output_has_typed_fields_and_span_context() {
    let (subscriber, capture) = new_subscriber(text_config());

    with_default(Arc::clone(&subscriber), || {
        let span = info_span!("request", request_id = "abc-123", method = "GET");
        let _guard = span.enter();
        info!(
            target: "tests::checkout",
            user_id = 42,
            active = true,
            "user authenticated"
        );
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 1, "{lines:#?}");

    let line = &lines[0];
    assert!(line.starts_with("[INFO ]"), "{line}");
    assert!(
        line.contains("request{method=GET, request_id=abc-123}:"),
        "{line}"
    );
    assert!(line.contains("tests::checkout:"), "{line}");
    assert!(line.contains("user authenticated"), "{line}");
    assert!(line.contains("user_id=42"), "{line}");
    assert!(line.contains("active=true"), "{line}");
}

#[test]
fn json_output_preserves_types_and_span_context() {
    let (subscriber, capture) = new_subscriber(json_config());

    with_default(Arc::clone(&subscriber), || {
        let span = info_span!("request", request_id = "abc-123");
        let _guard = span.enter();
        info!(
            target: "tests::checkout",
            user_id = 42,
            active = true,
            ratio = 0.75,
            "user authenticated"
        );
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 1, "{lines:#?}");

    let event: Value = serde_json::from_str(&lines[0]).expect("valid JSON log line");
    assert_eq!(event["level"], "INFO");
    assert_eq!(event["target"], "tests::checkout");
    assert_eq!(event["fields"]["message"], "user authenticated");
    assert_eq!(event["fields"]["user_id"].as_i64(), Some(42));
    assert_eq!(event["fields"]["active"].as_bool(), Some(true));
    assert_eq!(event["fields"]["ratio"].as_f64(), Some(0.75));
    assert_eq!(event["spans"][0]["name"], "request");
    assert_eq!(event["spans"][0]["fields"]["request_id"], "abc-123");
    assert!(event.get("timestamp").is_none());
}

#[test]
fn json_output_uses_an_rfc3339_utc_timestamp() {
    let (subscriber, capture) = new_subscriber(SubscriberConfig {
        format: OutputFormat::Json,
        ..Default::default()
    });

    with_default(Arc::clone(&subscriber), || {
        info!(target: "tests::timestamp", "timestamped");
    });

    let event: Value = serde_json::from_str(&capture.text()).expect("valid JSON log line");
    let timestamp = event["timestamp"].as_str().expect("timestamp present");
    let parsed = chrono::DateTime::parse_from_rfc3339(timestamp).expect("RFC 3339 timestamp");
    assert_eq!(parsed.offset().local_minus_utc(), 0);
}

#[test]
fn text_output_uses_an_rfc3339_utc_timestamp() {
    let (subscriber, capture) = new_subscriber(SubscriberConfig::default());

    with_default(Arc::clone(&subscriber), || {
        info!(target: "tests::timestamp", "timestamped");
    });

    let line = capture.text();
    let timestamp = line
        .split_whitespace()
        .next()
        .expect("timestamp should be first");
    assert!(
        chrono::DateTime::parse_from_rfc3339(timestamp).is_ok(),
        "{line}"
    );
}

#[test]
fn default_level_filters_less_severe_events() {
    let (subscriber, capture) = new_subscriber(SubscriberConfig {
        default_level: Level::INFO,
        include_timestamps: false,
        ..Default::default()
    });

    with_default(Arc::clone(&subscriber), || {
        debug!(target: "tests::filter", "debug hidden");
        info!(target: "tests::filter", "info shown");
        warn!(target: "tests::filter", "warn shown");
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].contains("info shown"));
    assert!(lines[1].contains("warn shown"));
}

#[test]
fn module_filter_overrides_the_default_level() {
    let (subscriber, capture) = new_subscriber(SubscriberConfig {
        default_level: Level::WARN,
        module_filters: vec![("tests::db".to_string(), Level::TRACE)],
        include_timestamps: false,
        ..Default::default()
    });

    with_default(Arc::clone(&subscriber), || {
        debug!(target: "tests::db", "db debug shown");
        debug!(target: "tests::http", "http debug hidden");
        info!(target: "tests::http", "http info hidden");
        warn!(target: "tests::http", "http warn shown");
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].contains("db debug shown"));
    assert!(lines[1].contains("http warn shown"));
}

#[test]
fn module_filters_can_raise_and_lower_verbosity() {
    let (subscriber, capture) = new_subscriber(SubscriberConfig {
        default_level: Level::INFO,
        module_filters: vec![
            ("tests::db".to_string(), Level::TRACE),
            ("tests::noisy".to_string(), Level::WARN),
        ],
        include_timestamps: false,
        ..Default::default()
    });

    with_default(Arc::clone(&subscriber), || {
        debug!(target: "tests::db", "db debug shown");
        info!(target: "tests::noisy", "noisy info hidden");
        info!(target: "tests::http", "http info shown");
        warn!(target: "tests::http", "http warn shown");
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(lines[0].contains("db debug shown"));
    assert!(lines[1].contains("http info shown"));
    assert!(lines[2].contains("http warn shown"));
    assert!(!capture.text().contains("noisy info hidden"));
}

#[test]
fn nested_spans_render_in_order_and_pop_on_exit() {
    let (subscriber, capture) = new_subscriber(text_config());

    with_default(Arc::clone(&subscriber), || {
        let outer = info_span!("outer", request_id = "r-1");
        let _outer_guard = outer.enter();
        info!(target: "tests::spans", "outer event");

        {
            let inner = info_span!("inner", table = "users");
            let _inner_guard = inner.enter();
            info!(target: "tests::spans", "inner event");
        }

        info!(target: "tests::spans", "after nested span");
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(lines[0].contains("outer{request_id=r-1}:"), "{}", lines[0]);
    assert!(
        lines[1].contains("outer{request_id=r-1} > inner{table=users}:"),
        "{}",
        lines[1]
    );
    assert!(lines[2].contains("outer{request_id=r-1}:"), "{}", lines[2]);
    assert!(!lines[2].contains("inner"), "{}", lines[2]);
}

#[test]
fn span_record_updates_context_for_later_events() {
    let (subscriber, capture) = new_subscriber(text_config());

    with_default(Arc::clone(&subscriber), || {
        let span = info_span!("db", rows = Empty);
        let _guard = span.enter();
        info!(target: "tests::spans", "before record");
        span.record("rows", 15_u64);
        info!(target: "tests::spans", "after record");
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].contains("db:"), "{}", lines[0]);
    assert!(lines[1].contains("db{rows=15}:"), "{}", lines[1]);
}

#[test]
fn spans_can_be_omitted_from_text_output() {
    let (subscriber, capture) = new_subscriber(SubscriberConfig {
        include_spans: false,
        include_timestamps: false,
        ..Default::default()
    });

    with_default(Arc::clone(&subscriber), || {
        let span = info_span!("request", id = 1);
        let _guard = span.enter();
        info!(target: "tests::spans", "hidden span");
    });

    let line = capture.text();
    assert!(!line.contains("request{"), "{line}");
    assert!(!line.contains("request:"), "{line}");
    assert!(line.contains("hidden span"), "{line}");
}

#[test]
fn json_includes_an_empty_span_array_when_enabled() {
    let (subscriber, capture) = new_subscriber(json_config());

    with_default(Arc::clone(&subscriber), || {
        info!(target: "tests::json", "no span");
    });

    let event: Value = serde_json::from_str(&capture.text()).expect("valid JSON log line");
    assert_eq!(event["spans"], json!([]));
}
