//! Command-line surface.
//!
//! The CLI is the first policy layer. `ask` cannot write files, `edit` writes
//! only through the patching path, and `run` goes through the command sandbox.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// `rai` - a local AI coding assistant whose product is its governance layer.
#[derive(Debug, Parser)]
#[command(
    name = "rai",
    version,
    about = "Local AI coding assistant: the Rust governance layer between a model and your machine",
    long_about = None,
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Path to a config file, or to the project directory that owns `.rai/`.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Emit machine-readable JSONL events instead of human output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress terminal output. Sessions and logs are still written.
    #[arg(long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Overrides shared by the model-driven modes.
#[derive(Debug, Args, Clone, Default)]
pub struct ModelOverrides {
    /// `openai`, `local`, or `scripted`.
    #[arg(long, value_name = "NAME")]
    pub provider: Option<String>,

    /// Model name passed to the provider.
    #[arg(long, value_name = "NAME")]
    pub model: Option<String>,

    /// Base URL for an OpenAI-compatible endpoint.
    #[arg(long, value_name = "URL")]
    pub base_url: Option<String>,

    /// Scripted response file, when `--provider scripted`.
    #[arg(long, value_name = "PATH")]
    pub script: Option<PathBuf>,

    /// Workspace root, overriding `[workspace].root`.
    #[arg(long, value_name = "PATH")]
    pub workspace: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Answer a question using repository search and file reads.
    Ask(AskArgs),

    /// Propose and apply patches inside the workspace.
    Edit(EditArgs),

    /// Inspect the git diff and report risks.
    Review(ReviewArgs),

    /// Execute an approved project command.
    Run(RunArgs),

    /// Model Context Protocol: consume and expose tools.
    Mcp(McpArgs),

    /// Write `.rai/config.toml` and `.rai/memories.md`.
    Init(InitArgs),

    /// Show or validate configuration.
    Config(ConfigArgs),

    /// Build or refresh the local repository index.
    Index(IndexArgs),

    /// Inspect project memory.
    Memory(MemoryArgs),

    /// List, inspect, or compact session transcripts.
    Sessions(SessionsArgs),

    /// Serve `rai` as an MCP server on stdin/stdout.
    #[command(name = "mcp-serve")]
    McpServe(McpServeArgs),
}

#[derive(Debug, Args)]
pub struct AskArgs {
    /// The question to answer.
    pub prompt: String,

    /// Resume a previous session by id.
    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,

    /// Approve every call that policy would otherwise ask about.
    #[arg(long)]
    pub yes: bool,

    #[command(flatten)]
    pub model: ModelOverrides,

    /// Maximum tool calls for this run.
    #[arg(long = "max-tool-calls", value_name = "N")]
    pub max_tool_calls: Option<u32>,
}

#[derive(Debug, Args)]
pub struct EditArgs {
    /// The change to make. A unified diff, or a `replace in <path>: "old" -> "new"`
    /// directive, also works when the local provider is configured.
    pub task: String,

    /// Validate patches without writing anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Approve every call that policy would otherwise ask about.
    #[arg(long)]
    pub yes: bool,

    /// Let the model run allowlisted commands (tests, checks) to verify its work.
    #[arg(long = "allow-commands")]
    pub allow_commands: bool,

    /// Run inferred project verification commands after editing and feed failures back.
    #[arg(long)]
    pub verify: bool,

    /// Resume a previous session by id.
    #[arg(long, value_name = "ID")]
    pub resume: Option<String>,

    #[command(flatten)]
    pub model: ModelOverrides,

    /// Maximum tool calls for this run.
    #[arg(long = "max-tool-calls", value_name = "N")]
    pub max_tool_calls: Option<u32>,
}

#[derive(Debug, Args)]
pub struct ReviewArgs {
    /// Review staged changes (`git diff --cached`) instead of the worktree.
    #[arg(long)]
    pub staged: bool,

    /// Ask the configured model for a narrative review on top of the risk scan.
    #[arg(long)]
    pub narrative: bool,

    /// Print the deterministic findings without contacting a model.
    #[arg(long)]
    pub offline: bool,

    #[command(flatten)]
    pub model: ModelOverrides,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Command and arguments. With `--shell`, pass the whole line as one argument.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub argv: Vec<String>,

    /// Interpret the command line with the platform shell.
    #[arg(long)]
    pub shell: bool,

    /// Working directory, relative to the workspace root.
    #[arg(long, value_name = "PATH")]
    pub cwd: Option<String>,

    /// Override `[commands].timeout_seconds`.
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Approve the command even if it is not allowlisted.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub enum McpCommand {
    /// Connect to configured servers and list their tools.
    ListTools,

    /// Connect to configured servers and list their resources.
    ListResources,

    /// Call one tool, for debugging.
    Call {
        /// Tool name, as `server.tool` or a bare name when unambiguous.
        tool: String,

        /// JSON object of arguments.
        ///
        /// Named `--json-args` so it cannot collide with the global `--json`
        /// output flag.
        #[arg(long = "json-args", default_value = "{}")]
        args: String,

        /// Approve the call if it is not automatically allowed.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Overwrite an existing configuration.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    /// Validate the configuration and exit non-zero on error.
    #[arg(long)]
    pub check: bool,

    /// Print the commented template instead of the effective values.
    #[arg(long)]
    pub template: bool,
}

#[derive(Debug, Args)]
pub struct IndexArgs {
    /// Print index statistics as well as writing the index.
    #[arg(long)]
    pub stats: bool,
}

#[derive(Debug, Args)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Print project memory.
    List,

    /// Add a fact under a section, creating it if needed.
    Add {
        /// Section name, e.g. `commands` or `architecture`.
        section: String,
        /// The fact to record.
        text: String,
    },

    /// Remove facts containing a substring.
    Forget {
        /// Substring to match.
        needle: String,
    },
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List recorded sessions, newest first.
    List,

    /// Print the event log of one session.
    Show {
        /// Session id, or `latest`.
        id: String,
        /// Print raw JSONL records instead of a summary.
        #[arg(long)]
        raw: bool,
    },

    /// Compact a session transcript in place.
    Compact {
        /// Session id, or `latest`.
        id: String,
        /// Byte budget to compact down to.
        #[arg(long, value_name = "BYTES")]
        limit_bytes: Option<usize>,
    },
}

#[derive(Debug, Args)]
pub struct McpServeArgs {
    /// Write protocol events to `<metadata>/logs/mcp.jsonl` (default true).
    #[arg(long, default_value_t = true)]
    pub log: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_the_article_cli_surface() {
        let cli = Cli::try_parse_from(["rai", "ask", "explain this module"]).unwrap();
        assert!(matches!(cli.command, Command::Ask(_)));

        let cli = Cli::try_parse_from(["rai", "edit", "add JSON export to reports"]).unwrap();
        assert!(matches!(cli.command, Command::Edit(_)));

        let cli = Cli::try_parse_from(["rai", "review"]).unwrap();
        assert!(matches!(cli.command, Command::Review(_)));

        let cli = Cli::try_parse_from(["rai", "mcp", "list-tools"]).unwrap();
        assert!(matches!(cli.command, Command::Mcp(_)));

        let cli = Cli::try_parse_from([
            "rai",
            "mcp",
            "call",
            "filesystem.read",
            "--json-args",
            "{\"path\":\"src/main.rs\"}",
        ])
        .unwrap();
        match cli.command {
            Command::Mcp(args) => match args.command {
                McpCommand::Call { tool, args, .. } => {
                    assert_eq!(tool, "filesystem.read");
                    assert!(args.contains("src/main.rs"));
                }
                _ => panic!("expected mcp call"),
            },
            _ => panic!("expected mcp"),
        }
    }

    #[test]
    fn run_accepts_multi_token_and_hyphenated_arguments() {
        let cli = Cli::try_parse_from(["rai", "run", "cargo", "test", "--lib"]).unwrap();
        match cli.command {
            Command::Run(args) => {
                assert_eq!(args.argv, vec!["cargo", "test", "--lib"]);
                assert!(!args.shell);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = Cli::try_parse_from(["rai", "ask", "hello", "--json"]).unwrap();
        assert!(cli.json);
        let cli = Cli::try_parse_from(["rai", "edit", "x", "--dry-run", "--yes"]).unwrap();
        match cli.command {
            Command::Edit(args) => {
                assert!(args.dry_run && args.yes);
            }
            _ => panic!("expected edit"),
        }
    }

    #[test]
    fn version_is_available() {
        let error = Cli::try_parse_from(["rai", "--version"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
    }
}
