//! A custom [`tracing`] subscriber with level/module filtering, text and JSON
//! output, and span-context tracking.
//!
//! This is the full-subscriber approach from "Writing a Custom Tracing
//! Subscriber" (Enzo Lombardi, *Rustaceans*): [`Subscriber`] is implemented
//! directly instead of composing `tracing_subscriber::Layer`s.
//!
//! The subscriber is intentionally in-memory and demo-sized. For high-volume
//! production use, prefer the `tracing-subscriber` registry plus layers.

pub mod csv_text;
pub mod logging;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Level, Metadata, Subscriber};

thread_local! {
    /// IDs of the spans currently entered on this thread, outermost first.
    static SPAN_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

/// Process-wide span ID source. Starts at 1 because `Id::from_u64(0)` panics.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// How an event is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Json,
    Text,
}

/// Runtime configuration for [`CustomSubscriber`].
#[derive(Debug, Clone)]
pub struct SubscriberConfig {
    /// Minimum severity accepted when no module filter matches.
    pub default_level: Level,
    /// `(target prefix, level)` overrides, checked in order. The first
    /// matching prefix wins, so put specific prefixes before general ones.
    pub module_filters: Vec<(String, Level)>,
    /// Text or newline-delimited JSON.
    pub format: OutputFormat,
    /// Include an RFC 3339 UTC timestamp (millisecond precision).
    pub include_timestamps: bool,
    /// Include the current span stack in each event.
    pub include_spans: bool,
}

impl Default for SubscriberConfig {
    fn default() -> Self {
        Self {
            default_level: Level::INFO,
            module_filters: Vec::new(),
            format: OutputFormat::Text,
            include_timestamps: true,
            include_spans: true,
        }
    }
}

/// Data captured when a span is created (and updated by `Span::record`).
#[derive(Debug, Clone)]
struct SpanData {
    name: &'static str,
    target: &'static str,
    fields: BTreeMap<&'static str, String>,
}

/// Span data plus the number of live `Span` handles that refer to it.
struct SpanEntry {
    data: SpanData,
    refs: usize,
}

/// A filtering, formatting, span-aware [`Subscriber`].
pub struct CustomSubscriber {
    config: SubscriberConfig,
    spans: RwLock<HashMap<u64, SpanEntry>>,
    writer: Mutex<Box<dyn Write + Send>>,
}

impl CustomSubscriber {
    /// Writes to stdout.
    pub fn new(config: SubscriberConfig) -> Self {
        Self::with_writer(config, io::stdout())
    }

    /// Writes to any sink: a file, a socket, or an in-memory buffer in tests.
    pub fn with_writer<W>(config: SubscriberConfig, writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self {
            config,
            spans: RwLock::new(HashMap::new()),
            writer: Mutex::new(Box::new(writer)),
        }
    }

    /// Text-format subscriber with the given default level.
    pub fn with_default_level(level: Level) -> Self {
        Self::new(SubscriberConfig {
            default_level: level,
            ..Default::default()
        })
    }

    fn format_event_json(&self, event: &Event<'_>) -> String {
        let metadata = event.metadata();
        let mut fields = serde_json::Map::new();
        event.record(&mut JsonVisitor { map: &mut fields });

        let mut object = json!({
            "level": metadata.level().as_str(),
            "target": metadata.target(),
            "fields": fields,
        });

        if self.config.include_timestamps {
            object["timestamp"] = json!(utc_timestamp());
        }

        if self.config.include_spans {
            let spans: Vec<Value> = self
                .collect_span_context()
                .into_iter()
                .map(|span| {
                    json!({
                        "name": span.name,
                        "target": span.target,
                        "fields": span.fields,
                    })
                })
                .collect();
            object["spans"] = json!(spans);
        }

        serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_string())
    }

    fn format_event_text(&self, event: &Event<'_>) -> String {
        let metadata = event.metadata();
        let mut output = String::new();

        if self.config.include_timestamps {
            output.push_str(&utc_timestamp());
            output.push(' ');
        }

        output.push_str(&format!("[{:5}] ", metadata.level().as_str()));

        if self.config.include_spans {
            let spans = self.collect_span_context();
            if !spans.is_empty() {
                let labels: Vec<String> = spans
                    .iter()
                    .map(|span| {
                        if span.fields.is_empty() {
                            span.name.to_string()
                        } else {
                            let fields: Vec<String> = span
                                .fields
                                .iter()
                                .map(|(key, value)| format!("{key}={value}"))
                                .collect();
                            format!("{}{{{}}}", span.name, fields.join(", "))
                        }
                    })
                    .collect();
                output.push_str(&labels.join(" > "));
                output.push_str(": ");
            }
        }

        output.push_str(metadata.target());
        output.push_str(": ");

        let mut visitor = TextVisitor::default();
        event.record(&mut visitor);
        output.push_str(&visitor.finish());

        output.trim_end().to_string()
    }

    fn collect_span_context(&self) -> Vec<SpanData> {
        let spans = self.spans.read().unwrap();
        SPAN_STACK.with(|stack| {
            stack
                .borrow()
                .iter()
                .filter_map(|id| spans.get(id).map(|entry| entry.data.clone()))
                .collect()
        })
    }
}

impl Subscriber for CustomSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        let target = metadata.target();

        for (module, level) in &self.config.module_filters {
            if target.starts_with(module.as_str()) {
                return metadata.level() <= level;
            }
        }

        metadata.level() <= &self.config.default_level
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        let mut fields = BTreeMap::new();
        attrs.record(&mut FieldCollector {
            fields: &mut fields,
        });

        let data = SpanData {
            name: attrs.metadata().name(),
            target: attrs.metadata().target(),
            fields,
        };

        self.spans
            .write()
            .unwrap()
            .insert(id, SpanEntry { data, refs: 1 });

        Id::from_u64(id)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        let mut spans = self.spans.write().unwrap();
        if let Some(entry) = spans.get_mut(&span.into_u64()) {
            values.record(&mut FieldCollector {
                fields: &mut entry.data.fields,
            });
        }
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {
        // The demo does not model causal relationships between spans.
    }

    fn event(&self, event: &Event<'_>) {
        let output = match self.config.format {
            OutputFormat::Json => self.format_event_json(event),
            OutputFormat::Text => self.format_event_text(event),
        };

        let mut writer = self.writer.lock().unwrap();
        let _ = writeln!(&mut *writer, "{output}");
        let _ = writer.flush();
    }

    fn enter(&self, span: &Id) {
        SPAN_STACK.with(|stack| stack.borrow_mut().push(span.into_u64()));
    }

    fn exit(&self, span: &Id) {
        SPAN_STACK.with(|stack| {
            let mut stack = stack.borrow_mut();
            if let Some(position) = stack.iter().rposition(|id| *id == span.into_u64()) {
                stack.remove(position);
            }
        });
    }

    fn clone_span(&self, id: &Id) -> Id {
        if let Some(entry) = self.spans.write().unwrap().get_mut(&id.into_u64()) {
            entry.refs += 1;
        }
        id.clone()
    }

    fn try_close(&self, id: Id) -> bool {
        let key = id.into_u64();
        let mut spans = self.spans.write().unwrap();

        if let Some(entry) = spans.get_mut(&key) {
            entry.refs = entry.refs.saturating_sub(1);
            if entry.refs == 0 {
                spans.remove(&key);
                return true;
            }
        }

        false
    }
}

fn utc_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Collects span field values as strings for context display.
struct FieldCollector<'a> {
    fields: &'a mut BTreeMap<&'static str, String>,
}

impl Visit for FieldCollector<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.fields.insert(field.name(), format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name(), value.to_string());
    }
}

/// Collects event fields with their native JSON types.
struct JsonVisitor<'a> {
    map: &'a mut serde_json::Map<String, Value>,
}

impl JsonVisitor<'_> {
    fn insert(&mut self, field: &Field, value: Value) {
        self.map.insert(field.name().to_string(), value);
    }
}

impl Visit for JsonVisitor<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.insert(field, Value::String(format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::String(value.to_string()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::Number(value.into()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        let value = serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(value.to_string()));
        self.insert(field, value);
    }
}

/// Formats event fields for a single text line, with `message` first.
#[derive(Default)]
struct TextVisitor {
    fields: Vec<(String, String)>,
}

impl TextVisitor {
    fn push(&mut self, field: &Field, value: String) {
        self.fields.push((field.name().to_string(), value));
    }

    fn finish(self) -> String {
        let mut message = None;
        let mut fields = Vec::new();

        for (name, value) in self.fields {
            if name == "message" {
                message = Some(value);
            } else {
                fields.push(format!("{name}={value}"));
            }
        }

        let mut output = String::new();
        if let Some(message) = message {
            output.push_str(&message);
        }
        if !fields.is_empty() {
            if !output.is_empty() {
                output.push(' ');
            }
            output.push_str(&fields.join(" "));
        }
        output
    }
}

impl Visit for TextVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.push(field, value.to_string());
        } else {
            self.push(field, format!("{value:?}"));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cloned_span_data_survives_until_the_last_handle_closes() {
        let subscriber = Arc::new(CustomSubscriber::with_writer(
            SubscriberConfig::default(),
            io::sink(),
        ));

        tracing::subscriber::with_default(Arc::clone(&subscriber), || {
            let span = tracing::info_span!("request");
            let clone = span.clone();
            drop(span);

            assert_eq!(subscriber.spans.read().unwrap().len(), 1);

            drop(clone);
        });

        assert_eq!(subscriber.spans.read().unwrap().len(), 0);
    }
}
