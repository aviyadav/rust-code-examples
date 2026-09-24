# Zero-Copy Log Parser

`zero-copy-log-parser` is a small Rust CLI that can either parse a log file line by line or generate a 500-line logfile from `sample.log`.

The implementation is intentionally lightweight:

- It avoids allocating new strings for parsed fields by borrowing slices from the original line.
- It validates the timestamp, log level, token field, and message field before processing.
- It redacts the token value directly inside the input line buffer.
- It reports the byte offsets of the timestamp, level, and message slices to demonstrate the zero-copy approach.
- It can turn the bundled `sample.log` into a larger synthetic logfile for testing or demos.

## Input format

Each line must follow this structure:

```text
<timestamp> <LEVEL> token=<value> message=<text>
```

Supported log levels are `DEBUG`, `INFO`, `WARN`, and `ERROR`.

Example:

```text
2026-09-15T10:20:30Z INFO token=abc123 message=Server started
```

## Output

For each valid line, the parser prints the level, timestamp, message, and the offsets of the borrowed slices inside the processed string.

Example output:

```text
INFO  2026-09-15T10:20:30Z | Server started [offsets: timestamp=0, level=21, message=40]
```

The token value is redacted before parsing is finalized, so the processed line keeps the same overall shape while hiding the original secret.

## Parsing Usage

```bash
cargo run -- <log-file>
```

Example:

```bash
cargo run -- ./logs/app.log
```

## Generation Usage

Generate a 500-line logfile based on `sample.log`:

```bash
cargo run --bin generate -- ./sample.log ./generated.log
```

You can also override the output size:

```bash
cargo run --bin generate -- ./sample.log ./generated.log 500
```

## Development

Run the test suite with:

```bash
cargo test
```

## Project details

- Package: `zero-copy-log-parser`
- Version: `0.1.0`
- Edition: `2024`
- Dependencies: none
- Sample input: `sample.log`

