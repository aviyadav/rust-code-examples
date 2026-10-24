use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use custom_tracing_subscriber::csv_text::{CsvTextSubscriber, emit, emit_batch, parse_csv};
use tracing::Level;
use tracing::subscriber::with_default;

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

const CSV: &str = r#"level,source,message,order_id,customer,total_cents,status
INFO,shop::checkout,order accepted,ORD-1,Ada Lovelace,6496,approved
WARN,shop::pricing,"high-value, review",ORD-2,Grace Hopper,12900,"manual, review"
ERROR,shop::payments,"payment declined ""card_declined""",ORD-3,Alan Turing,6198,declined
"#;

fn run(records: &[custom_tracing_subscriber::csv_text::CsvRecord], subscriber: CsvTextSubscriber) {
    with_default(subscriber, || {
        for record in records {
            emit(record);
        }
    });
}

#[test]
fn text_visitor_renders_csv_records_as_text_lines() {
    let records = parse_csv(CSV).expect("valid test CSV");
    let capture = Capture::default();

    run(&records, CsvTextSubscriber::with_writer(capture.clone()));

    let lines = capture.lines();
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert_eq!(
        lines[0],
        r#"[INFO ] csv.text: order accepted | source=shop::checkout | order_id=ORD-1 | customer="Ada Lovelace" | total_cents=6496 | status=approved"#
    );
    assert_eq!(
        lines[1],
        r#"[WARN ] csv.text: high-value, review | source=shop::pricing | order_id=ORD-2 | customer="Grace Hopper" | total_cents=12900 | status="manual, review""#
    );
    assert_eq!(
        lines[2],
        r#"[ERROR] csv.text: payment declined "card_declined" | source=shop::payments | order_id=ORD-3 | customer="Alan Turing" | total_cents=6198 | status=declined"#
    );
}

#[test]
fn span_tracking_adds_nested_context_and_recorded_fields() {
    let records = parse_csv(CSV).expect("valid test CSV");
    let capture = Capture::default();

    with_default(CsvTextSubscriber::with_writer(capture.clone()), || {
        emit_batch(&records)
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(
        lines[0].starts_with(
            "[INFO ] csv.batch{records=3} > csv.row{order_id=ORD-1, total_cents=6496}: csv.text:"
        ),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].contains("csv.row{order_id=ORD-2, total_cents=12900}"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].contains("csv.row{order_id=ORD-3, total_cents=6198}"),
        "{}",
        lines[2]
    );
}

#[test]
fn span_context_is_popped_after_the_batch() {
    let records = parse_csv(CSV).expect("valid test CSV");
    let capture = Capture::default();

    with_default(CsvTextSubscriber::with_writer(capture.clone()), || {
        emit_batch(&records[..1]);
        tracing::info!(target: "csv.text", "after batch");
    });

    let lines = capture.lines();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].contains("csv.batch{records=1}"), "{}", lines[0]);
    assert_eq!(lines[1], "[INFO ] csv.text: after batch");
}

#[test]
fn max_level_filters_csv_rows() {
    let records = parse_csv(CSV).expect("valid test CSV");
    let capture = Capture::default();

    run(
        &records,
        CsvTextSubscriber::with_writer(capture.clone()).with_max_level(Level::WARN),
    );

    let lines = capture.lines();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[0].starts_with("[WARN ]"), "{}", lines[0]);
    assert!(lines[1].starts_with("[ERROR]"), "{}", lines[1]);
}

#[test]
fn sample_csv_parses() {
    let records = parse_csv(include_str!("../data/checkout_events.csv")).expect("sample CSV");
    assert_eq!(records.len(), 8);
}
