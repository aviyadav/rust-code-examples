use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "profile-check",
    version,
    about = "Validate deployment profile files",
    propagate_version = true
)]
pub struct Cli {
    /// Enable debug logs when RUST_LOG is not set.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate a deployment profile.
    Validate(ValidateArgs),
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Path to the profile file.
    pub file: PathBuf,

    /// Reject unknown configuration keys.
    #[arg(long)]
    pub strict: bool,
}
