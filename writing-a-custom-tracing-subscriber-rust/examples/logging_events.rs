use std::fs;
use std::path::PathBuf;

use custom_tracing_subscriber::logging::{log_to_file, log_to_stdout};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("=== logging events to stdout ===");
    log_to_stdout();

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logs/events.log");
    log_to_file(&path)?;

    eprintln!("=== logging events to file: {} ===", path.display());
    print!("{}", fs::read_to_string(&path)?);

    Ok(())
}
