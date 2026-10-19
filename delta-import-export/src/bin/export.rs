use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use delta_kernel::Snapshot;
use delta_kernel::engine::arrow_data::EngineDataArrowExt;
use delta_kernel::object_store::aws::AmazonS3Builder;
use delta_kernel_default_engine::DefaultEngineBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

#[derive(Parser, Debug)]
#[command(
    name = "delta-export",
    version,
    about = "Export an S3 Delta table to a local Parquet file"
)]
struct Args {
    #[arg(short, long)]
    table: String,

    #[arg(short, long)]
    output: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    println!("Delta table: {}", args.table);
    println!("Output file: {}", args.output.display());

    let table_url = delta_kernel::try_parse_uri(&args.table)?;
    let object_store = AmazonS3Builder::from_env().with_url(&args.table).build()?;
    let engine = DefaultEngineBuilder::new(Arc::new(object_store)).build();

    let snapshot = Snapshot::builder_for(&table_url).build(&engine)?;
    println!("Delta version: {}", snapshot.version());

    let scan = snapshot.scan_builder().build()?;
    let batches = scan.execute(Arc::new(engine))?;

    let mut writer = None;
    let mut total_rows = 0usize;

    for engine_data in batches {
        let batch = EngineDataArrowExt::try_into_record_batch(engine_data)?;

        if writer.is_none() {
            let properties = WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .build();
            writer = Some(ArrowWriter::try_new(
                File::create(&args.output)?,
                batch.schema(),
                Some(properties),
            )?);
        }

        if let Some(writer) = writer.as_mut() {
            writer.write(&batch)?;
        }
        total_rows += batch.num_rows();
    }

    if let Some(writer) = writer {
        writer.close()?;
    }

    println!("Rows written: {total_rows}");
    println!("Output: {}", args.output.display());
    Ok(())
}
