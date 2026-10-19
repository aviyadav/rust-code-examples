# delta-import-export

Small Rust CLI for moving data between CSV files and Delta Lake tables on S3, built on
[`delta_kernel`](https://crates.io/crates/delta_kernel) 0.30.

It ships three binaries:

| Binary | Source | Purpose |
| --- | --- | --- |
| `delta-import-export` | `src/main.rs` | CSV → Delta table on S3 |
| `export` | `src/bin/export.rs` | Delta table on S3 → local Parquet file |
| `generate` | `src/bin/generate.rs` | Synthetic hobbits CSV generator |

## Requirements

- Rust (edition 2024)
- An S3 bucket and AWS credentials in the environment
- `AWS_REGION` (or `AWS_DEFAULT_REGION`) set to the bucket's region

```sh
export AWS_ACCESS_KEY_ID='<access-key-id>'
export AWS_SECRET_ACCESS_KEY='<secret-access-key>'
export AWS_REGION='eu-north-1'
# export AWS_SESSION_TOKEN='...'   # only for temporary credentials
```

Permissions needed:

- **Import:** `s3:ListBucket`, `s3:GetObject`, `s3:PutObject`
- **Export:** `s3:ListBucket`, `s3:GetObject`

## Import a CSV into a Delta table

```sh
cargo run -- --table s3://<bucket>/<table-path> --csv ./hobbits.csv
```

What it does:

1. Infers the schema from the CSV header and up to 1000 records.
2. Creates the Delta table at `--table` with that schema.
3. Reads the CSV in batches of 8192 rows.
4. Writes each batch as a Parquet data file in the table.
5. Commits all files in a single Delta transaction, retrying up to 5 times on
   retryable I/O errors.

The table must not already exist; the create step fails otherwise.

### CSV format

The first row is the header and defines the column names. Types are inferred by
`arrow-csv`. The schema produced by the `generate` binary is:

```csv
hobbit_id,name,family_name,age,home,occupation,favorite_food,ring_bearer,adventures,last_updated
1,Frodo,Baggins,50,Bag End,Gentlehobbit,Mushrooms,true,3,2024-08-15T13:23:37.190
```

The checked-in `hobbits.csv` is an older 8-column sample without `adventures` or
`last_updated`. Regenerate it with `cargo run --bin generate -- --rows 8` to match
the full schema.

## Generate a synthetic CSV

```sh
cargo run --bin generate -- --rows 1000 --output ./hobbits.csv
```

Generates a CSV matching the importer's expected schema:

```
hobbit_id,name,family_name,age,home,occupation,favorite_food,ring_bearer,adventures,last_updated
```

Types are `Int64`, `Utf8`, `Utf8`, `Int64`, `Utf8`, `Utf8`, `Utf8`, `Boolean`,
`Int64`, and `Timestamp(Millisecond, None)`. `last_updated` is emitted as
`YYYY-MM-DDTHH:MM:SS.mmm`, which `arrow-csv` infers as a millisecond timestamp.

Options:

- `--rows <N>` — number of data rows (required)
- `--output <PATH>` — output file, defaults to `hobbits.csv`
- `--seed <U64>` — PRNG seed for reproducible output

## Export a Delta table to Parquet

```sh
cargo run --bin export -- \
  --table s3://<bucket>/<table-path> \
  --output ./hobbits.parquet
```

What it does:

1. Opens the latest snapshot of the Delta table.
2. Scans all active data files.
3. Writes the rows to a single local Parquet file with Snappy compression.

Because `default-run` is set, plain `cargo run` runs the importer. Use
`--bin export` for the exporter.

## Limitations

- **S3 only.** Both binaries build an `AmazonS3Builder`; local paths and `file://`
  URLs are not supported.
- **Import is not idempotent.** Re-running against the same table fails at table
  creation instead of appending.
- **Empty tables export nothing.** The Parquet writer is created on the first
  batch, so an empty table produces no output file.
- **Commit conflicts abort.** The importer retries retryable I/O errors but exits
  on a version conflict.
- `src/lib.rs` defines an unused `export`/`import` subcommand CLI; the binaries do
  not link against it.

## Project layout

```
src/main.rs          CSV → Delta importer (default binary)
src/bin/export.rs    Delta → Parquet exporter
src/bin/generate.rs  Synthetic CSV generator
src/lib.rs           Unused CLI scaffolding
hobbits.csv          Sample input
```
