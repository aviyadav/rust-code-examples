//! A second, text-only subscriber example: CSV rows become tracing events and a
//! custom [`TextVisitor`] renders them.
//!
//! Unlike [`crate::CustomSubscriber`], this subscriber has no JSON mode. It
//! exists to show the visitor pattern on plain text data: each CSV row is parsed
//! into a [`CsvRecord`], emitted as an event, and formatted by [`TextVisitor`].
//! Spans are tracked too, following the article's "Tracking Spans with State"
//! and "Storing Span Attributes" sections: a thread-local enter stack plus a
//! lock-protected span map with reference counts.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Level, Metadata, Subscriber};

thread_local! {
    /// IDs of the spans currently entered on this thread, outermost first.
    static SPAN_STACK: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
}

const REQUIRED_COLUMNS: [&str; 7] = [
    "level",
    "source",
    "message",
    "order_id",
    "customer",
    "total_cents",
    "status",
];

/// One text record read from the CSV.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvRecord {
    pub level: Level,
    pub source: String,
    pub message: String,
    pub order_id: String,
    pub customer: String,
    pub total_cents: u64,
    pub status: String,
}

/// A CSV parse failure with the one-based source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvError {
    pub line: usize,
    pub message: String,
}

impl CsvError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CSV error on line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for CsvError {}

/// Parses the text CSV into records. The header is matched by name, so column
/// order is not significant, but every required column must be present.
///
/// This is a small line-oriented parser: quoted fields may contain commas and
/// doubled quotes, but not embedded newlines.
pub fn parse_csv(input: &str) -> Result<Vec<CsvRecord>, CsvError> {
    let mut rows = Vec::new();

    for (index, line) in input.lines().enumerate() {
        let line_number = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        rows.push((line_number, parse_line(line, line_number)?));
    }

    let Some((header_line, header)) = rows.first() else {
        return Err(CsvError::new(1, "CSV is empty"));
    };

    let mut indices = HashMap::new();
    for (index, name) in header.iter().enumerate() {
        indices.insert(name.trim(), index);
    }

    for required in REQUIRED_COLUMNS {
        if !indices.contains_key(required) {
            return Err(CsvError::new(
                *header_line,
                format!("missing required column `{required}`"),
            ));
        }
    }

    let column = |name: &str| indices[name];
    let mut records = Vec::new();

    for (line_number, row) in rows.iter().skip(1) {
        if row.len() != header.len() {
            return Err(CsvError::new(
                *line_number,
                format!("expected {} columns, found {}", header.len(), row.len()),
            ));
        }

        let level = parse_level(&row[column("level")], *line_number)?;
        let total_cents = row[column("total_cents")]
            .parse::<u64>()
            .map_err(|_| CsvError::new(*line_number, "total_cents must be an unsigned integer"))?;

        records.push(CsvRecord {
            level,
            source: row[column("source")].clone(),
            message: row[column("message")].clone(),
            order_id: row[column("order_id")].clone(),
            customer: row[column("customer")].clone(),
            total_cents,
            status: row[column("status")].clone(),
        });
    }

    Ok(records)
}

fn parse_line(line: &str, line_number: usize) -> Result<Vec<String>, CsvError> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' if field.is_empty() => in_quotes = true,
                ',' => fields.push(std::mem::take(&mut field)),
                _ => field.push(c),
            }
        }
    }

    if in_quotes {
        return Err(CsvError::new(line_number, "unterminated quoted field"));
    }

    fields.push(field);
    Ok(fields)
}

fn parse_level(value: &str, line_number: usize) -> Result<Level, CsvError> {
    match value.trim().to_ascii_uppercase().as_str() {
        "TRACE" => Ok(Level::TRACE),
        "DEBUG" => Ok(Level::DEBUG),
        "INFO" => Ok(Level::INFO),
        "WARN" => Ok(Level::WARN),
        "ERROR" => Ok(Level::ERROR),
        _ => Err(CsvError::new(
            line_number,
            format!("invalid level `{value}`"),
        )),
    }
}

/// Emits one CSV record as a tracing event with a fixed target and fields.
///
/// The event target is `csv.text`; the original `source` column is carried as a
/// field because tracing metadata targets are static strings.
pub fn emit(record: &CsvRecord) {
    macro_rules! send {
        ($level:expr) => {
            tracing::event!(
                target: "csv.text",
                $level,
                source = record.source.as_str(),
                order_id = record.order_id.as_str(),
                customer = record.customer.as_str(),
                total_cents = record.total_cents,
                status = record.status.as_str(),
                "{}",
                record.message
            )
        };
    }

    match record.level {
        Level::TRACE => send!(Level::TRACE),
        Level::DEBUG => send!(Level::DEBUG),
        Level::INFO => send!(Level::INFO),
        Level::WARN => send!(Level::WARN),
        Level::ERROR => send!(Level::ERROR),
    }
}

/// Replays a batch of records inside nested spans.
///
/// `csv.batch` is the outer span. Each row gets a `csv.row` span, and
/// `Span::record` adds `total_cents` after the span is created. Events emitted
/// by [`emit`] therefore show both spans in their context.
pub fn emit_batch(records: &[CsvRecord]) {
    let batch = tracing::info_span!("csv.batch", records = records.len());
    let _batch_guard = batch.enter();

    for record in records {
        let row = tracing::info_span!(
            "csv.row",
            order_id = %record.order_id,
            total_cents = tracing::field::Empty
        );
        let _row_guard = row.enter();
        row.record("total_cents", record.total_cents);
        emit(record);
    }
}

/// Collects event fields into a single text line. The `message` field comes
/// first; other fields are rendered as `key=value` and separated with ` | `.
#[derive(Default)]
pub struct TextVisitor {
    fields: Vec<(String, String)>,
}

impl TextVisitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> String {
        let mut message = None;
        let mut fields = Vec::new();

        for (name, value) in self.fields {
            if name == "message" {
                message = Some(value);
            } else {
                fields.push(format!("{name}={}", render_value(&value)));
            }
        }

        let mut line = String::new();
        if let Some(message) = message {
            line.push_str(&message);
        }
        if !fields.is_empty() {
            if !line.is_empty() {
                line.push_str(" | ");
            }
            line.push_str(&fields.join(" | "));
        }
        line
    }

    fn push(&mut self, field: &Field, value: String) {
        self.fields.push((field.name().to_string(), value));
    }
}

fn render_value(value: &str) -> String {
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == ',' || c == '"' || c == '|')
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

impl Visit for TextVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_string());
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

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.to_string());
    }
}

/// Span state stored between `new_span` and `try_close`.
#[derive(Debug, Clone)]
struct SpanData {
    name: &'static str,
    fields: BTreeMap<&'static str, String>,
}

struct SpanEntry {
    data: SpanData,
    refs: usize,
}

/// Collects span fields into their string-keyed state map.
struct SpanFieldCollector<'a> {
    fields: &'a mut BTreeMap<&'static str, String>,
}

impl Visit for SpanFieldCollector<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
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

/// A text subscriber with level filtering and thread-local span context.
///
/// Events are rendered by [`TextVisitor`]; active spans are tracked in a
/// thread-local stack and a lock-protected map, like the article's
/// `SpanTrackingSubscriber`.
pub struct CsvTextSubscriber {
    max_level: Level,
    spans: RwLock<HashMap<u64, SpanEntry>>,
    writer: Mutex<Box<dyn Write + Send>>,
}

impl Default for CsvTextSubscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl CsvTextSubscriber {
    pub fn new() -> Self {
        Self::with_writer(io::stdout())
    }

    pub fn with_writer<W>(writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        Self {
            max_level: Level::TRACE,
            spans: RwLock::new(HashMap::new()),
            writer: Mutex::new(Box::new(writer)),
        }
    }

    /// Keeps events at `level` or more severe. `Level::WARN` keeps WARN and
    /// ERROR, for example.
    pub fn with_max_level(mut self, level: Level) -> Self {
        self.max_level = level;
        self
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

static NEXT_SPAN_ID: AtomicU64 = AtomicU64::new(1);

impl Subscriber for CsvTextSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &self.max_level
    }

    fn new_span(&self, attrs: &Attributes<'_>) -> Id {
        let id = NEXT_SPAN_ID.fetch_add(1, Ordering::Relaxed);
        let mut fields = BTreeMap::new();
        attrs.record(&mut SpanFieldCollector {
            fields: &mut fields,
        });

        let data = SpanData {
            name: attrs.metadata().name(),
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
            values.record(&mut SpanFieldCollector {
                fields: &mut entry.data.fields,
            });
        }
    }

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let mut visitor = TextVisitor::new();
        event.record(&mut visitor);

        let spans = self.collect_span_context();
        let span_prefix = if spans.is_empty() {
            String::new()
        } else {
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
            format!("{}: ", labels.join(" > "))
        };

        let line = format!(
            "[{:5}] {}{}: {}",
            metadata.level().as_str(),
            span_prefix,
            metadata.target(),
            visitor.finish()
        );

        let mut writer = self.writer.lock().unwrap();
        let _ = writeln!(&mut *writer, "{}", line.trim_end());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const HEADER: &str = "level,source,message,order_id,customer,total_cents,status";

    #[test]
    fn cloned_span_state_survives_until_the_last_handle_closes() {
        let subscriber = Arc::new(CsvTextSubscriber::with_writer(std::io::sink()));

        tracing::subscriber::with_default(Arc::clone(&subscriber), || {
            let span = tracing::info_span!("keep", order_id = "ORD-1");
            let clone = span.clone();
            drop(span);

            assert_eq!(subscriber.spans.read().unwrap().len(), 1);
            drop(clone);
        });

        assert_eq!(subscriber.spans.read().unwrap().len(), 0);
    }

    #[test]
    fn parses_quoted_commas_and_escaped_quotes() {
        let input = format!(
            "{HEADER}\nINFO,shop::checkout,\"order accepted, notified\",ORD-1,\"Ada \"\"Countess\"\" Lovelace\",6496,approved\n"
        );
        let records = parse_csv(&input).expect("valid CSV");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "order accepted, notified");
        assert_eq!(records[0].customer, "Ada \"Countess\" Lovelace");
        assert_eq!(records[0].total_cents, 6496);
    }

    #[test]
    fn rejects_a_missing_required_column() {
        let input = "level,source,message,order_id,customer,status\nINFO,x,y,ORD-1,Ada,approved\n";
        let error = parse_csv(input).expect_err("total_cents is required");

        assert!(
            error
                .message
                .contains("missing required column `total_cents`"),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_invalid_level() {
        let input = format!("{HEADER}\nBOGUS,shop,x,ORD-1,Ada,1,approved\n");
        let error = parse_csv(&input).expect_err("BOGUS is not a level");

        assert!(error.message.contains("invalid level"), "{error}");
    }

    #[test]
    fn rejects_an_unterminated_quote() {
        let input = format!("{HEADER}\nINFO,shop,\"oops,ORD-1,Ada,1,approved\n");
        let error = parse_csv(&input).expect_err("quote is unterminated");

        assert!(error.message.contains("unterminated"), "{error}");
    }
}
