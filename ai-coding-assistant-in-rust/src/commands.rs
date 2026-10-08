//! Command implementations.
//!
//! Each command is a thin layer over the library: load configuration, assemble
//! collaborators, run one thing, report facts.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{bail, Context as _, Result};

use crate::agent::{Agent, AgentParts, RunOptions};
use crate::cli::{
    AskArgs, Cli, ConfigArgs, EditArgs, IndexArgs, InitArgs, McpArgs, McpCommand, MemoryArgs,
    MemoryCommand, ModelOverrides, ReviewArgs, RunArgs, SessionCommand, SessionsArgs,
};
use crate::config::Config;
use crate::events::{Emitter, OutputMode};
use crate::index::{IndexOptions, RepoIndex};
use crate::memory::ProjectMemory;
use crate::model::{self, ModelClient};
use crate::policy::{Approver, PolicyEngine};
use crate::redact::Redactor;
use crate::session::SessionStore;
use crate::tools::Mode;
use crate::util::Cancel;
use crate::{git, sandbox};

/// Everything a command needs, assembled once.
pub struct Prepared {
    pub config: Arc<Config>,
    pub emitter: Arc<Emitter>,
    pub redactor: Arc<Redactor>,
    pub index: Arc<RepoIndex>,
    pub policy: Arc<PolicyEngine>,
    pub client: Arc<dyn ModelClient>,
    pub memory: ProjectMemory,
    pub session: String,
    pub cancel: Arc<Cancel>,
}

impl Prepared {
    /// Emit a user-visible note.
    pub fn note(&self, message: impl Into<String>) {
        self.emitter.emit(crate::events::Event::Notice {
            message: message.into(),
        });
    }
}

/// Load configuration, apply overrides, and build every collaborator.
pub async fn prepare(
    cli: &Cli,
    overrides: &ModelOverrides,
    mode: Mode,
    cancel: &Arc<Cancel>,
) -> Result<Prepared> {
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let mut config = Config::discover(cli.config.as_deref(), &cwd)
        .with_context(|| "configuration could not be loaded")?;
    apply_overrides(&mut config, overrides);
    config
        .validate()
        .with_context(|| "configuration is not valid")?;
    let config = Arc::new(config);

    let metadata = config.metadata_dir();
    std::fs::create_dir_all(metadata.join("sessions"))?;
    std::fs::create_dir_all(metadata.join("indexes"))?;
    std::fs::create_dir_all(metadata.join("logs"))?;

    let session = crate::agent::new_session_id(mode);
    let output = if cli.json {
        OutputMode::Json
    } else {
        OutputMode::Text
    };
    let emitter = Arc::new(
        Emitter::new(output, cli.quiet, session.clone())
            .with_log_file(&metadata.join("logs").join(format!("{session}.jsonl")))
            .mirror_into(Arc::new(SessionStore::new(&metadata))),
    );

    let redactor = Arc::new(Redactor::from_env());
    let policy = Arc::new(PolicyEngine::from_config(&config)?);
    let index = Arc::new(build_index(&config, &emitter, false).await?);
    let client = model::build(&config, mode)?;
    let memory = ProjectMemory::load(&metadata.join("memories.md"));

    Ok(Prepared {
        config,
        emitter,
        redactor,
        index,
        policy,
        client,
        memory,
        session,
        cancel: Arc::clone(cancel),
    })
}

/// Apply CLI overrides on top of the configuration file.
fn apply_overrides(config: &mut Config, overrides: &ModelOverrides) {
    if let Some(provider) = &overrides.provider {
        config.model.provider = provider.clone();
    }
    if let Some(model) = &overrides.model {
        config.model.model = model.clone();
    }
    if let Some(base_url) = &overrides.base_url {
        config.model.base_url = base_url.clone();
    }
    if let Some(script) = &overrides.script {
        config.model.script = Some(script.clone());
        config.model.provider = "scripted".to_string();
    }
    if let Some(workspace) = &overrides.workspace {
        config.workspace.root = workspace.clone();
    }
}

/// Build (and optionally persist) the repository index.
pub async fn build_index(config: &Config, emitter: &Emitter, save: bool) -> Result<RepoIndex> {
    let root = config.workspace_root();
    let path = config.metadata_dir().join("indexes").join("index.json");
    let options = IndexOptions {
        excludes: config.workspace.exclude.clone(),
        max_files: config.search.max_indexed_files,
        max_scan_bytes: config.workspace.max_file_bytes.min(512 * 1024),
    };
    emitter.emit(crate::events::Event::Phase {
        name: "context".to_string(),
        detail: format!("indexing {} ", root.display()),
    });
    let index = RepoIndex::build_async(root, options).await?;
    if save {
        index.save(&path)?;
    }
    emitter.emit(crate::events::Event::Notice {
        message: format!(
            "indexed {} file(s), {} symbol(s), {} skipped as binary",
            index.file_count(),
            index.symbol_count(),
            index.skipped_binary
        ),
    });
    Ok(index)
}

/// Assemble the agent from prepared collaborators.
pub fn build_agent(prepared: &Prepared, assume_yes: bool) -> Result<Agent> {
    let approver = Approver::standard(assume_yes, std::io::stdin().is_terminal());
    Agent::new(AgentParts {
        client: Arc::clone(&prepared.client),
        config: Arc::clone(&prepared.config),
        emitter: Arc::clone(&prepared.emitter),
        redactor: Arc::clone(&prepared.redactor),
        index: Arc::clone(&prepared.index),
        policy: Arc::clone(&prepared.policy),
        approver,
        cancel: (*prepared.cancel).clone(),
        memory: prepared.memory.clone(),
    })
    .map_err(anyhow::Error::from)
}

/// Check that a provider can actually answer before doing work.
fn check_provider_ready(prepared: &Prepared) -> Result<()> {
    if prepared.client.provider() == "openai" {
        let env = &prepared.config.model.api_key_env;
        let has_key = std::env::var(env)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let local = prepared.config.model.base_url.contains("localhost")
            || prepared.config.model.base_url.contains("127.0.0.1");
        if !has_key && !local {
            bail!(
                "provider `openai` needs ${env} to be set (base_url = {}). \
                 Use `--provider local` for an offline run, or point --base-url at a local server.",
                prepared.config.model.base_url
            );
        }
    }
    Ok(())
}

/// Print the factual work log once a run finishes.
pub fn print_report(prepared: &Prepared, report: &crate::agent::RunReport) {
    if prepared.emitter.quiet() || prepared.emitter.mode() == OutputMode::Json {
        return;
    }
    let line = |text: String| prepared.emitter.write_line(&text);
    line(String::new());
    line(format!(
        "work log        {} ({})",
        report.session,
        if report.ok { "ok" } else { "incomplete" }
    ));
    line(format!("  mode          {}", report.mode));
    line(format!("  model turns   {}", report.model_turns));
    line(format!("  tool calls    {}", report.tool_calls));
    if report.usage.input_tokens + report.usage.output_tokens > 0 {
        line(format!(
            "  tokens        {} in / {} out",
            report.usage.input_tokens, report.usage.output_tokens
        ));
    }
    line(format!("  duration      {}ms", report.duration_ms));

    if report.patches.is_empty() {
        line("  files         unchanged".to_string());
    } else {
        for file in &report.patches {
            let action = match file.action {
                crate::patch::FileAction::Created => "created",
                crate::patch::FileAction::Modified => "modified",
                crate::patch::FileAction::Deleted => "deleted",
            };
            line(format!(
                "  file          {action} {} (+{} -{})",
                file.path, file.added, file.removed
            ));
        }
    }

    for command in &report.commands {
        line(format!(
            "  command       {} -> {}",
            command.argv.join(" "),
            command.summary()
        ));
    }

    if !report.findings.is_empty() {
        line("  findings".to_string());
        for finding in &report.findings {
            line(format!(
                "    [{}] {} {} - {}",
                finding.severity,
                finding.kind,
                finding.path.clone().unwrap_or_else(|| "-".into()),
                finding.detail
            ));
        }
    }

    if let Some(verification) = &report.verification {
        line(format!(
            "  verification  {}",
            if verification.ok { "passed" } else { "failed" }
        ));
        for step in &verification.steps {
            line(format!(
                "    {} {} -> {}",
                if step.ok { "pass" } else { "FAIL" },
                step.argv.join(" "),
                step.summary
            ));
        }
    }

    for warning in &report.warnings {
        line(format!("  warning       {warning}"));
    }

    let store = SessionStore::new(&prepared.config.metadata_dir());
    line(format!(
        "  transcript    {}",
        store.path_for(&report.session).display()
    ));
}

/// `rai ask` - answer a question using repository search and file reads.
pub async fn ask(cli: &Cli, args: &AskArgs, cancel: &Arc<Cancel>) -> Result<ExitCode> {
    let prepared = prepare(cli, &args.model, Mode::Ask, cancel).await?;
    check_provider_ready(&prepared)?;

    let mut options = RunOptions::new(Mode::Ask, &args.prompt);
    options.session = Some(prepared.session.clone());
    options.resume = args.resume.clone();
    options.quiet = cli.quiet;

    let agent = build_agent(&prepared, args.yes)?;
    let agent = match args.max_tool_calls {
        Some(max) => {
            let mut budgets = agent.budgets();
            budgets.max_tool_calls = max.max(1);
            agent.with_budgets(budgets)
        }
        None => agent,
    };

    let report = agent.run(options).await?;
    print_report(&prepared, &report);
    Ok(exit_code_for(&report))
}

/// `rai edit` - propose and apply patches inside the workspace.
pub async fn edit(cli: &Cli, args: &EditArgs, cancel: &Arc<Cancel>) -> Result<ExitCode> {
    let prepared = prepare(cli, &args.model, Mode::Edit, cancel).await?;
    check_provider_ready(&prepared)?;

    if args.dry_run {
        prepared.note("dry run: patches are validated but not written");
    }

    let mut options = RunOptions::new(Mode::Edit, &args.task);
    options.session = Some(prepared.session.clone());
    options.resume = args.resume.clone();
    options.allow_commands = args.allow_commands || args.verify;
    options.dry_run = args.dry_run;
    options.verify = args.verify;
    options.quiet = cli.quiet;

    let agent = build_agent(&prepared, args.yes)?;
    let agent = match args.max_tool_calls {
        Some(max) => {
            let mut budgets = agent.budgets();
            budgets.max_tool_calls = max.max(1);
            agent.with_budgets(budgets)
        }
        None => agent,
    };

    let report = agent.run(options).await?;
    print_report(&prepared, &report);

    if !report.warnings.is_empty() && report.files_changed.is_empty() {
        bail!("no files were changed");
    }
    Ok(exit_code_for(&report))
}

/// `rai review` - inspect the git diff and report risks.
pub async fn review(cli: &Cli, args: &ReviewArgs, cancel: &Arc<Cancel>) -> Result<ExitCode> {
    let prepared = prepare(cli, &args.model, Mode::Review, cancel).await?;
    let root = prepared.config.workspace_root();

    let (diff, truncated) = git::diff(&root, args.staged, None, 512 * 1024).await?;
    let stats = crate::tools::git_tools::diff_stats(&diff);
    let findings = crate::tools::git_tools::scan_diff(&diff);

    if !prepared.emitter.quiet() && prepared.emitter.mode() == OutputMode::Text {
        let line = |text: String| prepared.emitter.write_line(&text);
        line(String::new());
        line(format!(
            "risk scan       {} changed file(s) ({})",
            stats.len(),
            if args.staged { "staged" } else { "worktree" }
        ));
        for stat in &stats {
            line(format!(
                "  {:<40} +{} -{}",
                stat.path, stat.added, stat.removed
            ));
        }
        if truncated {
            line("  note: diff was truncated before analysis".to_string());
        }
        if findings.is_empty() {
            line("  no rule-based risks found".to_string());
        }
        for finding in &findings {
            line(format!(
                "  [{}] {:<18} {} - {}",
                finding.severity,
                finding.kind,
                finding.path.clone().unwrap_or_else(|| "-".into()),
                finding.detail
            ));
        }
    }

    let high = findings.iter().filter(|f| f.severity == "high").count();

    if args.narrative && !args.offline {
        check_provider_ready(&prepared)?;
        let task = if args.staged {
            "Review the staged changes. Call git_diff with staged = true, then explain the risks you see."
        } else {
            "Review the current working-tree changes, then explain the risks you see."
        };
        let mut options = RunOptions::new(Mode::Review, task);
        options.session = Some(prepared.session.clone());
        options.quiet = cli.quiet;
        let agent = build_agent(&prepared, false)?;
        let report = agent.run(options).await?;
        print_report(&prepared, &report);
        if !report.ok {
            return Ok(ExitCode::from(1));
        }
    }

    Ok(if high > 0 {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    })
}

/// Map a run report onto a process exit code.
fn exit_code_for(report: &crate::agent::RunReport) -> ExitCode {
    if report.ok && report.failed_commands().is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Load configuration without model overrides.
fn load_config(cli: &Cli) -> Result<Config> {
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    let config = Config::discover(cli.config.as_deref(), &cwd)?;
    config.validate()?;
    Ok(config)
}

/// Turn `rai run` arguments into an argv plus a shell flag.
pub fn resolve_command(args: &RunArgs) -> Result<(Vec<String>, bool)> {
    if args.shell {
        return Ok((vec![args.argv.join(" ")], true));
    }
    if args.argv.len() == 1 {
        let single = &args.argv[0];
        if sandbox::has_shell_metacharacters(single) {
            bail!(
                "`{single}` contains shell metacharacters. Pass --shell to allow shell \
                 interpretation (requires [commands].allow_shell = true), or pass the command \
                 as separate arguments."
            );
        }
        if single.contains(char::is_whitespace) {
            let parts = sandbox::split_simple(single);
            if parts.is_empty() {
                bail!("empty command");
            }
            return Ok((parts, false));
        }
    }
    Ok((args.argv.clone(), false))
}

/// `rai run` - execute an approved project command.
pub async fn run(cli: &Cli, args: &RunArgs, cancel: &Arc<Cancel>) -> Result<ExitCode> {
    let config = Arc::new(load_config(cli)?);
    let policy = PolicyEngine::from_config(&config)?;
    let redactor = Arc::new(Redactor::from_env());
    let root = config.workspace_root();

    let (argv, shell) = resolve_command(args)?;
    if shell && !config.commands.allow_shell {
        bail!(
            "shell interpretation is disabled ([commands].allow_shell = false). \
             Either enable it in .rai/config.toml or pass the command as separate arguments."
        );
    }

    let decision = policy.command_decision(&argv);
    let allowed_reason = match decision {
        crate::policy::Decision::Denied { reason } => bail!("refused: {reason}"),
        crate::policy::Decision::Allowed { reason } => reason,
        crate::policy::Decision::NeedsApproval { reason } => {
            let definition = crate::tools::ToolDefinition::local(
                "run",
                "direct command execution from the CLI",
                serde_json::json!({"type": "object"}),
                crate::tools::RiskClass::Command,
            );
            let preview = format!("argv: {}", redactor.clean(&argv.join(" ")));
            let approver = Approver::standard(args.yes, std::io::stderr().is_terminal());
            let outcome = approver.decide(&definition, &preview, &reason);
            if !outcome.allowed {
                bail!("not approved: {}", outcome.reason);
            }
            outcome.reason
        }
    };

    let cwd = match &args.cwd {
        Some(relative) => crate::util::safe_join(&root, relative)?,
        None => root.clone(),
    };
    if !cwd.starts_with(&root) {
        bail!("working directory escapes the workspace root");
    }

    let metadata = config.metadata_dir();
    let session = crate::util::short_id("run-");
    let output = if cli.json {
        OutputMode::Json
    } else {
        OutputMode::Text
    };
    let emitter = Arc::new(
        Emitter::new(output, cli.quiet, session.clone())
            .with_log_file(&metadata.join("logs").join(format!("{session}.jsonl")))
            .mirror_into(Arc::new(SessionStore::new(&metadata))),
    );

    // Record a header so `rai sessions list` shows this command run too.
    let _ = SessionStore::new(&metadata).append(
        &session,
        &crate::session::SessionRecord::Header(crate::session::SessionHeader {
            id: session.clone(),
            ts: crate::util::now_rfc3339(),
            mode: "run".to_string(),
            task: argv.join(" "),
            provider: "n/a".to_string(),
            model: "n/a".to_string(),
            workspace: root.display().to_string(),
        }),
    );

    emitter.emit(crate::events::Event::Phase {
        name: "command".to_string(),
        detail: format!("{} ({allowed_reason})", argv.join(" ")),
    });

    let timeout = args.timeout.unwrap_or(config.commands.timeout_seconds);
    let mut spec = sandbox::CommandSpec::new(argv, cwd)
        .with_timeout(std::time::Duration::from_secs(timeout.max(1)))
        .with_max_output(config.commands.max_output_bytes)
        .with_env(config.commands.allowed_env.clone());
    spec.shell = shell;

    let emitter_for_stream = Arc::clone(&emitter);
    let outcome = sandbox::run(&spec, cancel, &redactor, |kind, chunk| {
        emitter_for_stream.emit(crate::events::Event::ToolStream {
            call_id: "run".to_string(),
            stream: kind.as_str().to_string(),
            chunk: chunk.to_string(),
        });
    })
    .await?;

    emitter.emit(crate::events::Event::CommandResult {
        argv: outcome.argv.clone(),
        exit_code: outcome.exit_code,
        timed_out: outcome.timed_out,
        duration_ms: outcome.duration_ms,
        stdout_bytes: outcome.stdout_bytes,
        stderr_bytes: outcome.stderr_bytes,
        truncated: outcome.truncated,
        redactions: outcome.redactions,
    });

    Ok(match outcome.exit_code {
        Some(0) => ExitCode::SUCCESS,
        Some(code) if (1..256).contains(&code) => ExitCode::from(code as u8),
        Some(_) => ExitCode::from(1),
        None => ExitCode::from(1),
    })
}

/// `rai mcp` - consume tools from configured MCP servers.
pub async fn mcp(cli: &Cli, args: &McpArgs, _cancel: &Arc<Cancel>) -> Result<ExitCode> {
    let config = Arc::new(load_config(cli)?);
    if config.mcp.servers.iter().all(|s| !s.enabled) {
        bail!("no enabled [[mcp.servers]] entries; add one to .rai/config.toml");
    }

    let (clients, problems) = crate::mcp::client::connect_all(&config).await;
    for problem in &problems {
        eprintln!("rai: warning: {problem}");
    }
    if clients.is_empty() {
        bail!("no MCP server could be started");
    }

    let result = match &args.command {
        McpCommand::ListTools => {
            for client in &clients {
                println!("{} ({} tool(s))", client.name(), client.tools().len());
                for tool in client.tools() {
                    println!(
                        "  {}.{}\n      {}",
                        client.name(),
                        tool.name,
                        tool.description.lines().next().unwrap_or("")
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        McpCommand::ListResources => {
            for client in &clients {
                match client.list_resources().await {
                    Ok(value) => {
                        let resources = value
                            .get("resources")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        println!("{} ({} resource(s))", client.name(), resources.len());
                        for resource in resources {
                            println!(
                                "  {}",
                                resource
                                    .get("uri")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("<no uri>")
                            );
                        }
                    }
                    Err(error) => println!("{}: {error}", client.name()),
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        McpCommand::Call { tool, args, yes } => {
            call_mcp_tool(&config, &clients, tool, args, *yes).await
        }
    };

    for client in &clients {
        client.shutdown().await;
    }
    result
}

/// Call one MCP tool, behind the same policy engine as everything else.
async fn call_mcp_tool(
    config: &Config,
    clients: &[Arc<crate::mcp::McpClient>],
    tool: &str,
    json_args: &str,
    yes: bool,
) -> Result<ExitCode> {
    let arguments: serde_json::Value = serde_json::from_str(json_args)
        .with_context(|| format!("--json-args `{json_args}` is not a JSON value"))?;

    let (client, tool_name) = match tool.split_once('.') {
        Some((server, name)) => {
            let client = clients
                .iter()
                .find(|c| c.name() == server)
                .ok_or_else(|| anyhow::anyhow!("no configured MCP server named `{server}`"))?;
            (Arc::clone(client), name.to_string())
        }
        None => {
            let mut matches = clients
                .iter()
                .filter(|c| c.tools().iter().any(|t| t.name == tool))
                .map(Arc::clone)
                .collect::<Vec<_>>();
            match matches.len() {
                0 => bail!("no MCP server exposes a tool named `{tool}`"),
                1 => (matches.remove(0), tool.to_string()),
                _ => bail!("`{tool}` is ambiguous; qualify it as server.tool"),
            }
        }
    };

    let schema = client
        .tools()
        .into_iter()
        .find(|t| t.name == tool_name)
        .map(|t| t.schema)
        .unwrap_or_else(|| serde_json::json!({"type": "object"}));

    // Remote tools are treated as networked capabilities: they reach outside
    // this process, so policy decides whether that is acceptable.
    let definition = crate::tools::ToolDefinition::mcp(
        client.name(),
        tool_name.clone(),
        "MCP tool call",
        schema,
        crate::tools::RiskClass::Network,
    );
    let policy = PolicyEngine::from_config(config)?;
    let redactor = Redactor::from_env();
    match policy.evaluate(&definition, &arguments) {
        crate::policy::Decision::Denied { reason } => bail!("refused: {reason}"),
        crate::policy::Decision::NeedsApproval { reason } => {
            let preview = crate::policy::describe_call(&definition, &arguments, &redactor);
            let approver = Approver::standard(yes, std::io::stderr().is_terminal());
            let outcome = approver.decide(&definition, &preview, &reason);
            if !outcome.allowed {
                bail!("not approved: {}", outcome.reason);
            }
        }
        crate::policy::Decision::Allowed { .. } => {}
    }

    let outcome = client.call_tool(&tool_name, arguments).await?;
    let text = redactor.clean(&outcome.text);
    println!("{text}");
    Ok(if outcome.is_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// `rai mcp-serve` - expose policy-aware project tools over stdio.
pub async fn mcp_serve(cli: &Cli) -> Result<ExitCode> {
    let config = Arc::new(load_config(cli)?);
    crate::mcp::serve(config).await?;
    Ok(ExitCode::SUCCESS)
}

/// The project directory that owns `.rai/` for this invocation.
fn project_dir(cli: &Cli) -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    Ok(match &cli.config {
        Some(path) if path.is_dir() => path.clone(),
        Some(path) => match path.file_name().and_then(|n| n.to_str()) {
            Some(crate::METADATA_DIR) => path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or(cwd),
            _ => path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or(cwd),
        },
        None => cwd,
    })
}

/// `rai init` - write the config and seed project memory.
pub async fn init(cli: &Cli, args: &InitArgs) -> Result<ExitCode> {
    let project = project_dir(cli)?;
    let metadata = project.join(crate::METADATA_DIR);
    std::fs::create_dir_all(metadata.join("sessions"))?;
    std::fs::create_dir_all(metadata.join("indexes"))?;
    std::fs::create_dir_all(metadata.join("logs"))?;

    let config_path = metadata.join("config.toml");
    if config_path.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to overwrite it",
            config_path.display()
        );
    }
    std::fs::write(&config_path, Config::template())?;
    println!("wrote {}", config_path.display());

    let memory_path = metadata.join("memories.md");
    if !memory_path.exists() || args.force {
        let seeded = ProjectMemory::seed_for(&project);
        std::fs::write(&memory_path, seeded.render())?;
        println!("wrote {}", memory_path.display());
    } else {
        println!("kept {}", memory_path.display());
    }

    if !project.join(".gitignore").exists() {
        println!(
            "note: this directory has no .gitignore; consider ignoring `.rai/logs/` and `.rai/sessions/`"
        );
    }
    println!("\nnext: `rai index`, then `rai ask \"what does this project do?\"`");
    Ok(ExitCode::SUCCESS)
}

/// `rai config` - show or validate configuration.
pub async fn config(cli: &Cli, args: &ConfigArgs) -> Result<ExitCode> {
    if args.template {
        print!("{}", Config::template());
        return Ok(ExitCode::SUCCESS);
    }

    let config = load_config(cli)?;
    if args.check {
        println!("configuration is valid");
    }
    print!("{}", config.describe());

    let registry = crate::tools::builtin();
    for mode in [Mode::Ask, Mode::Edit, Mode::Review] {
        println!(
            "\ntools[{}]      {}",
            mode.as_str(),
            registry.describe(mode, false)
        );
    }
    println!(
        "\ntools[{}+commands] {}",
        Mode::Edit.as_str(),
        registry.describe(Mode::Edit, true)
    );
    Ok(ExitCode::SUCCESS)
}

/// `rai index` - build or refresh the repository index.
pub async fn index(cli: &Cli, args: &IndexArgs) -> Result<ExitCode> {
    let config = load_config(cli)?;
    let session = crate::util::short_id("index-");
    let emitter = Emitter::new(OutputMode::Text, cli.quiet, session);
    let index = build_index(&config, &emitter, true).await?;

    if args.stats || !cli.quiet {
        println!("files      {}", index.file_count());
        println!("bytes      {}", index.total_bytes());
        println!("symbols    {}", index.symbol_count());
        println!("binary     {}", index.skipped_binary);
        println!("truncated  {}", index.truncated);
        let languages = index.languages();
        if !languages.is_empty() {
            println!("languages");
            for (language, count) in languages.iter().take(12) {
                println!("  {language:<14} {count}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `rai memory` - inspect and edit project memory.
pub async fn memory(cli: &Cli, args: &MemoryArgs) -> Result<ExitCode> {
    let config = load_config(cli)?;
    let path = config.metadata_dir().join("memories.md");
    let mut memory = ProjectMemory::load(&path);

    match &args.command {
        MemoryCommand::List => {
            println!("{}", path.display());
            let rendered = memory.render();
            if rendered.trim().is_empty() {
                println!("  (empty; `rai memory add commands \"cargo test\"` to record a fact)");
            } else {
                print!("{rendered}");
                if !rendered.ends_with('\n') {
                    println!();
                }
            }
        }
        MemoryCommand::Add { section, text } => {
            memory.add(section, text);
            memory.save()?;
            println!("recorded under [{section}]: {text}");
        }
        MemoryCommand::Forget { needle } => {
            let removed = memory.remove_matching(needle);
            if removed == 0 {
                println!("no memory entry contained `{needle}`");
            } else {
                memory.save()?;
                println!("removed {removed} entry/entries containing `{needle}`");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `rai sessions` - inspect session transcripts.
pub async fn sessions(cli: &Cli, args: &SessionsArgs) -> Result<ExitCode> {
    let config = load_config(cli)?;
    let store = SessionStore::new(&config.metadata_dir());

    match &args.command {
        SessionCommand::List => {
            let sessions = store.list()?;
            if sessions.is_empty() {
                println!("no sessions recorded under {}", store.dir().display());
                return Ok(ExitCode::SUCCESS);
            }
            println!(
                "{:<26} {:<7} {:>9} {:>6} {:>5}  task",
                "id", "mode", "bytes", "events", "msgs"
            );
            for session in sessions {
                println!(
                    "{:<26} {:<7} {:>9} {:>6} {:>5}  {}",
                    session.id,
                    session.mode,
                    session.bytes,
                    session.events,
                    session.messages,
                    crate::util::truncate_line(session.task.trim(), 60)
                );
            }
        }
        SessionCommand::Show { id, raw } => {
            let id = resolve_session_id(&store, id)?;
            let path = store.path_for(&id);
            if !path.is_file() {
                bail!("session `{id}` does not exist");
            }
            if *raw {
                print!("{}", std::fs::read_to_string(&path)?);
                return Ok(ExitCode::SUCCESS);
            }
            println!("{}", path.display());
            for record in store.load(&id)? {
                match record {
                    crate::session::SessionRecord::Header(header) => println!(
                        "header  mode={} provider={} model={} task={}",
                        header.mode,
                        header.provider,
                        header.model,
                        crate::util::truncate_line(&header.task, 80)
                    ),
                    crate::session::SessionRecord::Message(message) => println!(
                        "{:<7} {}",
                        message.role,
                        crate::util::truncate_line(message.text.trim(), 120)
                    ),
                    crate::session::SessionRecord::Event { ts, event } => println!(
                        "event   {:<8} {} {}",
                        event.label(),
                        ts,
                        crate::util::truncate_line(
                            &serde_json::to_string(&event).unwrap_or_default(),
                            140
                        )
                    ),
                    crate::session::SessionRecord::Compaction {
                        dropped_events,
                        truncated_messages,
                        ..
                    } => println!(
                        "compact dropped {dropped_events} event(s), truncated {truncated_messages} message(s)"
                    ),
                }
            }
        }
        SessionCommand::Compact { id, limit_bytes } => {
            let id = resolve_session_id(&store, id)?;
            let limit = limit_bytes.unwrap_or(config.budgets.max_command_output_bytes * 8);
            let report = store.compact(&id, limit, 4096)?;
            println!(
                "{}: {} -> {} bytes (dropped {} event(s), truncated {} message(s))",
                report.session,
                report.before_bytes,
                report.after_bytes,
                report.dropped_events,
                report.truncated_messages
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Accept `latest` as a session id.
fn resolve_session_id(store: &SessionStore, id: &str) -> Result<String> {
    if id.eq_ignore_ascii_case("latest") {
        return store
            .latest()?
            .ok_or_else(|| anyhow::anyhow!("no sessions have been recorded yet"));
    }
    Ok(id.to_string())
}
