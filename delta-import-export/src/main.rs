use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use arrow_csv::ReaderBuilder;
use arrow_csv::reader::Format;
use clap::Parser;
use delta_kernel::Snapshot;
use delta_kernel::committer::FileSystemCommitter;
use delta_kernel::engine::arrow_conversion::TryIntoKernel;
use delta_kernel::engine::arrow_data::ArrowEngineData;
use delta_kernel::object_store::aws::AmazonS3Builder;
use delta_kernel::transaction::create_table::create_table;
use delta_kernel::transaction::{CommitResult, RetryableTransaction, UpdateTableOperation};
use delta_kernel_default_engine::DefaultEngineBuilder;

#[derive(Parser, Debug)]
#[command(name = "delta-import", version)]
struct Args {
    #[arg(short, long)]
    table: String,

    #[arg(short, long)]
    csv: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("CSV: {}", args.csv.display());
    println!("Delta table: {}", args.table);

    let table_url = delta_kernel::try_parse_uri(&args.table)?;
    let object_store = AmazonS3Builder::from_env().with_url(&args.table).build()?;

    let engine = DefaultEngineBuilder::new(Arc::new(object_store)).build();
    let schema_file = File::open(&args.csv)?;
    let format = Format::default().with_header(true);
    let (arrow_schema, _) = format.infer_schema(schema_file, Some(1000))?;

    println!("Schema: {arrow_schema:#?}");

    let kernel_schema = Arc::new((&arrow_schema).try_into_kernel()?);

    println!("Creating Delta table...");

    let created = create_table(
        &args.table,
        kernel_schema,
        "delta-import-export/0.1.0",
        true,
    )
    .build(&engine, Box::new(FileSystemCommitter::new()))?
    .commit(&engine)?;

    if !created.is_committed() {
        return Err("Failed to create Delta table".into());
    }

    println!("Delta table created.");

    let snapshot = Snapshot::builder_for(table_url.clone()).build(&engine)?;

    let mut transaction = snapshot
        .update_table(
            UpdateTableOperation::Write,
            "delta-import-export/0.1.0",
            true,
        )
        .build(&engine, Box::new(FileSystemCommitter::new()))?;

    let write_context = transaction.write_state()?.write_context_builder().build()?;

    let csv_file = File::open(&args.csv)?;

    let csv_reader = ReaderBuilder::new(Arc::new(arrow_schema))
        .with_header(true)
        .with_batch_size(8192)
        .build(csv_file)?;

    let mut total_rows = 0usize;

    for batch in csv_reader {
        let batch = batch?;
        total_rows += batch.num_rows();
        println!("Writing batch: {} rows", batch.num_rows());
        let data = ArrowEngineData::new(batch);

        let file_metadata = engine.write_parquet(&data, &write_context).await?;
        transaction.add_files(file_metadata);
    }

    let mut retries = 0;
    let committed = loop {
        if retries > 5 {
            return Err("Exceeded maximum commit retries".into());
        }
        transaction = match transaction.commit(&engine)? {
            CommitResult::Committed(committed) => {
                break committed;
            }
            CommitResult::Conflicted(conflicted) => {
                return Err(format!(
                    "Transaction conflict at version {}",
                    conflicted.conflict_version()
                )
                .into());
            }
            CommitResult::Retryable(RetryableTransaction { transaction, error }) => {
                println!("Retrying commit: {error}");
                transaction
            }
        };
        retries += 1;
    };

    println!("Committed Delta version: {}", committed.commit_version());
    println!("Rows written: {}", total_rows);
    Ok(())
}
