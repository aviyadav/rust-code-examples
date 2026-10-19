use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "delta-import-export",
    version,
    about = "Import and export Delta Lake tables"
)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Export {
        #[arg(short, long)]
        table: String,

        #[arg(short, long)]
        output: PathBuf,
    },

    Import {
        #[arg(short, long)]
        table: String,

        #[arg(short, long)]
        input: PathBuf,
    },
}
