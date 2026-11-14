mod cli;
mod commands;
mod diagnostics;
mod profile;

use clap::Parser;
use miette::Result;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_tracing(cli.verbose);

    match cli.command {
        Command::Validate(args) => commands::validate::validate(args),
    }
}

fn init_tracing(verbose: bool) {
    let fallback_filter = if verbose {
        "profile_check=debug"
    } else {
        "profile_check=info"
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(fallback_filter));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
