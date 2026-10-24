use custom_tracing_subscriber::csv_text::{CsvTextSubscriber, emit_batch, parse_csv};

const CSV: &str = include_str!("../data/checkout_events.csv");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let records = parse_csv(CSV)?;

    eprintln!(
        "Rendering {} CSV records through TextVisitor, inside csv.batch/csv.row spans:",
        records.len()
    );

    tracing::subscriber::with_default(CsvTextSubscriber::new(), || {
        emit_batch(&records);
    });

    eprintln!("Done: message first, fields after `|`, and active spans before the event target.");
    Ok(())
}
