//! `rai` binary entry point.
//!
//! Assembly only: parse arguments, load configuration, wire collaborators, and
//! dispatch. The interesting logic lives in the library.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;

use rai::cli::{Cli, Command};
use rai::util::install_ctrlc;

fn main() -> ExitCode {
    let cli = Cli::parse();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rai: cannot start the async runtime: {error}");
            return ExitCode::from(1);
        }
    };

    match runtime.block_on(dispatch(cli)) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("rai: error: {error:#}");
            ExitCode::from(1)
        }
    }
}

/// Route one invocation.
async fn dispatch(cli: Cli) -> Result<ExitCode> {
    let cancel = rai::util::Cancel::new();
    install_ctrlc(cancel.clone());
    let cancel = Arc::new(cancel);

    match &cli.command {
        Command::Ask(args) => rai::commands::ask(&cli, args, &cancel).await,
        Command::Edit(args) => rai::commands::edit(&cli, args, &cancel).await,
        Command::Review(args) => rai::commands::review(&cli, args, &cancel).await,
        Command::Run(args) => rai::commands::run(&cli, args, &cancel).await,
        Command::Mcp(args) => rai::commands::mcp(&cli, args, &cancel).await,
        Command::McpServe(_) => rai::commands::mcp_serve(&cli).await,
        Command::Init(args) => rai::commands::init(&cli, args).await,
        Command::Config(args) => rai::commands::config(&cli, args).await,
        Command::Index(args) => rai::commands::index(&cli, args).await,
        Command::Memory(args) => rai::commands::memory(&cli, args).await,
        Command::Sessions(args) => rai::commands::sessions(&cli, args).await,
    }
}
