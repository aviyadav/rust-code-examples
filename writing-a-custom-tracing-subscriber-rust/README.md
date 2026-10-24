# custom-tracing-subscriber

A from-scratch [`tracing`](https://docs.rs/tracing) subscriber implementation
based on *Writing a Custom Tracing Subscriber* by Enzo Lombardi. It demonstrates
the article's mechanism end to end: level and module filtering, a visitor-based
text formatter, newline-delimited JSON, and spans tracked with real state.

The project is intentionally small and readable. It implements `Subscriber`
directly instead of composing `tracing-subscriber` layers, because the point is
to show what the framework is doing underneath.

## What you get

- `CustomSubscriber`: text or JSON output, level/module filtering, RFC 3339
  timestamps, and nested span context.
- Span fields added later with `Span::record`; span data stays alive while a
  `Span` clone exists.
- A writer seam: stdout, a file, or an in-memory buffer for tests.
- `CsvTextSubscriber`: a second, text-only subscriber that renders CSV rows
  through a custom `TextVisitor`, with `csv.batch > csv.row` span context.
- Reusable stdout/file logging built on the same event-emitting code.
- 24 tests covering formatting, filtering, parsers, span state, and destinations.

## Quick start

Requires Rust 1.85+ and Cargo.

```console
cargo run
cargo test
```

`cargo run` processes six synthetic orders from
[`data/orders.json`](data/orders.json) three times: filtered text, filtered JSON,
then WARN/ERROR only. Section headers go to stderr; log lines go to stdout.

## Examples

| Command | What it shows | Entry point |
| --- | --- | --- |
| `cargo run` | Text/JSON formatting, filters, nested spans, order pipeline | [`src/main.rs`](src/main.rs) |
| `cargo run --example csv_text_visitor` | CSV → events → `TextVisitor`, span state, `Span::record` | [`examples/csv_text_visitor.rs`](examples/csv_text_visitor.rs) |
| `cargo run --example logging_events` | Same events logged to stdout and a file | [`examples/logging_events.rs`](examples/logging_events.rs) |

### CSV TextVisitor example

[`src/csv_text.rs`](src/csv_text.rs) parses
[`data/checkout_events.csv`](data/checkout_events.csv), opens a `csv.batch` span
plus one `csv.row` span per record, and formats each event with a custom
`TextVisitor`:

```console
cargo run --example csv_text_visitor
```

```text
[INFO ] csv.batch{records=8} > csv.row{order_id=ORD-2001, total_cents=6496}: csv.text: order accepted | source=shop::checkout | order_id=ORD-2001 | customer="Ada Lovelace" | total_cents=6496 | status=approved
```

The visitor puts the message first, then `key=value` fields separated by ` | `.
Values containing whitespace, commas, quotes, or pipes are quoted. The parser
supports quoted fields, embedded commas, and doubled quotes; it is line-oriented,
so fields cannot contain embedded newlines. `CsvTextSubscriber::with_max_level`
keeps only records at the requested severity or above.

### stdout and file logging

[`src/logging.rs`](src/logging.rs) has one shared event source, `emit_events`,
and three destinations:

```rust
log_to_stdout();                       // standard output
log_to_file("logs/events.log")?;       // creates parent dirs, truncates file
log_to_writer(my_writer);              // any Write + Send + 'static
```

`cargo run --example logging_events` writes `logs/events.log` and prints the file
contents back. The same `CustomSubscriber` text formatter is used for both
stdout and file output. The file is created or truncated on each run; writes are
flushed after every event.

## Configuration

```rust
use custom_tracing_subscriber::{CustomSubscriber, OutputFormat, SubscriberConfig};
use tracing::Level;

let config = SubscriberConfig {
    default_level: Level::INFO,
    module_filters: vec![
        ("my_app::db".into(), Level::TRACE),
        ("my_app::noisy".into(), Level::WARN),
    ],
    format: OutputFormat::Json,
    include_timestamps: true,
    include_spans: true,
};

let subscriber = CustomSubscriber::new(config);
tracing::subscriber::set_global_default(subscriber).expect("install subscriber");
```

- `module_filters` are checked in order; the first matching target prefix wins,
  so put specific prefixes before broad ones.
- `default_level` is the fallback when no module prefix matches.
- `OutputFormat::Text` renders a readable line. `OutputFormat::Json` emits one
  JSON object per line; event fields keep their numeric/bool/string types.
- Use `CustomSubscriber::with_writer(writer)` for a file, socket, or test buffer.
  `CustomSubscriber::with_default_level(Level::WARN)` is a shortcut for a text
  subscriber with that default level.
- `tracing::subscriber::with_default` installs a subscriber for one closure,
  which is how the tests isolate themselves; `set_global_default` installs one
  process-wide.

### Output shapes

Text output has the form:

```text
2026-... [INFO ] request{request_id=req-42, user=ada}: my_app::handler: order accepted order_id="ORD-2001" total_cents=6496
```

JSON output has the form:

```json
{"fields":{"message":"order accepted","order_id":"ORD-2001","total_cents":6496},"level":"INFO","spans":[{"fields":{"request_id":"req-42","user":"ada"},"name":"request","target":"my_app::handler"}],"target":"my_app::handler","timestamp":"2026-..."}
```

Span fields are stored as strings; event fields in JSON preserve integer,
unsigned integer, float, boolean, and string values.

## How it works

`CustomSubscriber` implements the `Subscriber` hooks directly:

- `enabled` applies the first matching module filter, then the default level.
- `new_span` assigns an atomic span ID and stores the span name and recorded
  fields in a `RwLock<HashMap>`.
- `record` appends fields to an existing span, which is what `Span::record`
  calls.
- `enter` / `exit` maintain a thread-local stack of active span IDs.
- `event` collects the active span chain, formats it with `TextVisitor` or
  `JsonVisitor`, and writes one line to the configured writer.
- `clone_span` / `try_close` reference-count span handles so data is not removed
  while another clone can still enter the span.

`CsvTextSubscriber` follows the same span-state model in a smaller, text-only
implementation. It has no JSON mode or module filters; its purpose is to show the
visitor pattern and nested span rendering with CSV input.

## Project layout

```text
src/
  lib.rs              CustomSubscriber, config, text/JSON visitors, span state
  main.rs             order-processing demo
  demo/               checkout pipeline: HTTP, cache, DB, pricing, payments
  csv_text.rs         CSV parser, CsvTextSubscriber, TextVisitor, span state
  logging.rs          shared event emitter plus stdout/file/writer destinations
examples/
  csv_text_visitor.rs
  logging_events.rs
data/
  orders.json
  checkout_events.csv
tests/
  subscriber.rs       text/JSON/filtering/timestamp/span tests
  csv_text.rs         CSV parser, visitor, span, and sample-data tests
  logging.rs          writer and file-destination tests
logs/events.log       generated by the logging example
```

## Testing

```console
cargo test
cargo test --test subscriber
cargo test --test csv_text
cargo test --test logging
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

The suite has **24 tests**:

- `tests/subscriber.rs` (11): filtering, text and JSON shape, timestamps, nested
  span order, `Span::record`, and configuration toggles.
- `tests/csv_text.rs` (5): CSV parsing, `TextVisitor`, max-level filtering,
  nested span context, span exit, and the sample data.
- `tests/logging.rs` (2): writer output and file parent-directory/content.
- Unit tests (6): span-handle reference counting and CSV parser error paths.

Tests use isolated subscribers and capture writers; no global subscriber is
installed.

## Limitations and production notes

This is a teaching implementation, not a drop-in production logger.

- Span context is thread-local. A span entered on one thread is not visible to
  events emitted on another thread.
- The span store is a `RwLock<HashMap>` and output is flushed per event. For
  high-volume use, prefer the `tracing-subscriber` registry/layers or batch and
  shard the output.
- The CSV parser is line-oriented and has no embedded-newline support.
- The file logger truncates its target on each run. `Subscriber::event` cannot
  return I/O errors, so write failures after the file is opened are ignored.
- Module filters use first-match prefix semantics, not longest-prefix matching.

## Attribution

Based on *Writing a Custom Tracing Subscriber* by Enzo Lombardi (Rustaceans).
The reference PDF is included as
[`writing-a-custom-tracing-subscriber-rust.pdf`](writing-a-custom-tracing-subscriber-rust.pdf).

## License

MIT. See [`Cargo.toml`](Cargo.toml).
