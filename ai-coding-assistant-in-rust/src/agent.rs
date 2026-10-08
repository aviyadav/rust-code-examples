//! The agent loop: model turns, policy checks, tool execution, budgets.
//!
//! The model proposes. The Rust runtime owns policy, state, tool execution,
//! logging, and approval. Budgets are enforced here, not in a prompt, because
//! without budgets "agentic" becomes "unbounded".

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{RaiError, Result};
use crate::events::Emitter;
use crate::index::RepoIndex;
use crate::memory::ProjectMemory;
use crate::model::{Message, ModelClient, ModelRequest, Role, TokenUsage, ToolCall};
use crate::patch::AppliedFile;
use crate::policy::{Approver, Decision, PolicyEngine};
use crate::redact::Redactor;
use crate::sandbox::CommandOutcome;
use crate::session::{MessageRecord, SessionHeader, SessionRecord, SessionStore};
use crate::tools::{builtin, git_tools, Mode, RiskClass, ToolContext, ToolRegistry};
use crate::util::{now_rfc3339, short_id, Cancel};

/// Hard limits on a run.
#[derive(Debug, Clone, Copy)]
pub struct Budgets {
    pub max_tool_calls: u32,
    pub max_model_turns: u32,
    pub max_runtime: Duration,
    /// How many times a failed verification may send the model back to work.
    pub max_verify_rounds: u32,
}

impl Budgets {
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_tool_calls: config.budgets.max_tool_calls,
            max_model_turns: config.budgets.max_model_turns,
            max_runtime: Duration::from_secs(config.budgets.max_runtime_seconds),
            max_verify_rounds: 2,
        }
    }
}

/// Everything one run needs.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub mode: Mode,
    /// Session id to write to. When absent, the resumed id or a fresh one is used.
    pub session: Option<String>,
    pub task: String,
    /// Explicit opt-in that lets `edit` run allowlisted verification commands.
    pub allow_commands: bool,
    /// Validate patches without writing.
    pub dry_run: bool,
    /// Resume a previous conversation.
    pub resume: Option<String>,
    /// Run inferred project commands after an edit and feed failures back.
    pub verify: bool,
    pub quiet: bool,
}

impl RunOptions {
    pub fn new(mode: Mode, task: impl Into<String>) -> Self {
        Self {
            mode,
            session: None,
            task: task.into(),
            allow_commands: false,
            dry_run: false,
            resume: None,
            verify: false,
            quiet: false,
        }
    }
}

/// One verification command that ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationStep {
    pub argv: Vec<String>,
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub summary: String,
}

/// Result of verifying an edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    /// True when every step passed.
    pub ok: bool,
    pub steps: Vec<VerificationStep>,
    pub failure: Option<git_tools::FailureReport>,
}

/// What a run produced. This is the factual work log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub session: String,
    pub mode: Mode,
    pub ok: bool,
    pub tool_calls: u64,
    pub model_turns: u64,
    pub files_changed: Vec<String>,
    pub commands: Vec<CommandOutcome>,
    pub patches: Vec<AppliedFile>,
    pub findings: Vec<git_tools::Finding>,
    pub verification: Option<Verification>,
    pub final_text: String,
    pub usage: TokenUsage,
    pub duration_ms: u64,
    /// Non-fatal problems worth showing the user.
    pub warnings: Vec<String>,
}

impl RunReport {
    /// Paths changed by this run.
    pub fn changed_file_count(&self) -> usize {
        self.files_changed.len()
    }

    /// Commands that exited non-zero.
    pub fn failed_commands(&self) -> Vec<&CommandOutcome> {
        self.commands.iter().filter(|c| !c.success()).collect()
    }
}

/// Collaborators an [`Agent`] needs, injected so tests can substitute them.
pub struct AgentParts {
    pub client: Arc<dyn ModelClient>,
    pub config: Arc<Config>,
    pub emitter: Arc<Emitter>,
    pub redactor: Arc<Redactor>,
    pub index: Arc<RepoIndex>,
    pub policy: Arc<PolicyEngine>,
    pub approver: Approver,
    pub cancel: Cancel,
    pub memory: ProjectMemory,
}

/// The runtime that owns the loop.
pub struct Agent {
    client: Arc<dyn ModelClient>,
    config: Arc<Config>,
    registry: ToolRegistry,
    policy: Arc<PolicyEngine>,
    approver: Approver,
    emitter: Arc<Emitter>,
    cancel: Cancel,
    index: Arc<RepoIndex>,
    memory: ProjectMemory,
    budgets: Budgets,
    sessions: SessionStore,
    root: PathBuf,
    commands: Arc<Mutex<Vec<CommandOutcome>>>,
    patches: Arc<Mutex<Vec<AppliedFile>>>,
}

impl Agent {
    pub fn new(parts: AgentParts) -> Result<Self> {
        let root = parts.config.workspace_root();
        let budgets = Budgets::from_config(&parts.config);
        let sessions = SessionStore::new(&parts.config.metadata_dir());
        Ok(Self {
            client: parts.client,
            config: parts.config,
            registry: builtin(),
            policy: parts.policy,
            approver: parts.approver,
            emitter: parts.emitter,
            cancel: parts.cancel,
            index: parts.index,
            memory: parts.memory,
            budgets,
            sessions,
            root,
            commands: Arc::new(Mutex::new(Vec::new())),
            patches: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// The tool registry in use.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    /// Budgets in force for this run.
    pub fn budgets(&self) -> Budgets {
        self.budgets
    }

    /// Override budgets (used by tests and by `--max-tool-calls` style flags).
    pub fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }

    fn tool_context(&self, mode: Mode, dry_run: bool, session: &str) -> ToolContext {
        ToolContext {
            root: self.root.clone(),
            config: Arc::clone(&self.config),
            policy: Arc::clone(&self.policy),
            index: Arc::clone(&self.index),
            emitter: Arc::clone(&self.emitter),
            redactor: Arc::new(Redactor::from_env()),
            cancel: self.cancel.clone(),
            session: session.to_string(),
            mode,
            dry_run,
            commands: Arc::clone(&self.commands),
            patches: Arc::clone(&self.patches),
        }
    }
}

/// Fresh session id.
pub fn new_session_id(mode: Mode) -> String {
    short_id(&format!("{}-", mode.as_str()))
}

/// Record a conversation message into a session transcript.
pub fn message_record(message: &Message) -> MessageRecord {
    MessageRecord {
        role: message.role.as_str().to_string(),
        text: message.text.clone(),
        tool_calls: message
            .tool_calls
            .iter()
            .map(|call| crate::session::StoredToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            })
            .collect(),
        tool_call_id: message.tool_call_id.clone(),
    }
}

/// Write the session header.
pub fn write_header(
    sessions: &SessionStore,
    session: &str,
    mode: Mode,
    task: &str,
    client: &dyn ModelClient,
    root: &std::path::Path,
) -> Result<()> {
    sessions.append(
        session,
        &SessionRecord::Header(SessionHeader {
            id: session.to_string(),
            ts: now_rfc3339(),
            mode: mode.as_str().to_string(),
            task: task.to_string(),
            provider: client.provider().to_string(),
            model: client.model().to_string(),
            workspace: root.display().to_string(),
        }),
    )
}

/// How many times an identical call may make no progress before the run stops.
///
/// A model that keeps asking for the same denied or failing action is not
/// making progress; the runtime should say so instead of burning the budget.
pub const MAX_IDENTICAL_UNPRODUCTIVE_CALLS: u32 = 2;

/// Marker used by [`refusal_message`], so the loop can recognise a refusal.
pub const REFUSAL_PREFIX: &str = "tool `";

/// True when tool output is a refusal rather than a result.
pub fn is_refusal(text: &str) -> bool {
    text.starts_with(REFUSAL_PREFIX) && text.contains("` was not executed: ")
}

/// True when tool output reports a failed call rather than a result.
pub fn is_tool_failure(text: &str) -> bool {
    text.starts_with(REFUSAL_PREFIX) && text.contains("` failed: ")
}

/// True when a tool result made no progress: it was refused, or it failed.
pub fn is_unproductive(text: &str) -> bool {
    is_refusal(text) || is_tool_failure(text)
}

/// The message a refused call returns to the model.
pub fn refusal_message(tool: &str, reason: &str) -> String {
    format!(
        "{REFUSAL_PREFIX}{tool}` was not executed: {reason}\n\
         Do not retry the same call unchanged. Either gather different context, \
         or explain to the user what approval or configuration would be needed."
    )
}

/// Turn a tool error into model-readable text instead of aborting the run.
pub fn tool_error_text(tool: &str, error: &RaiError) -> String {
    format!(
        "tool `{tool}` failed: {error}\nretry_safe: {}",
        error.retry_safe()
    )
}

/// Decision summary used in the event stream.
pub fn decision_label(decision: &Decision) -> &'static str {
    match decision {
        Decision::Allowed { .. } => "allowed",
        Decision::NeedsApproval { .. } => "requested",
        Decision::Denied { .. } => "denied",
    }
}

/// Risk classes that a mode refuses outright.
pub fn mode_refusal(mode: Mode, risk: RiskClass) -> Option<String> {
    if mode == Mode::Review && risk != RiskClass::ReadOnly {
        return Some("review mode is read-only".to_string());
    }
    if mode == Mode::Ask && !matches!(risk, RiskClass::ReadOnly | RiskClass::Network) {
        return Some("ask mode cannot modify the workspace or run commands".to_string());
    }
    None
}

/// Running counters for one loop.
#[derive(Debug, Default, Clone, Copy)]
pub struct Counters {
    pub tool_calls: u64,
    pub model_turns: u64,
    pub usage: TokenUsage,
}

/// How many times a failed verification may re-enter the loop.
const MAX_VERIFY_ROUNDS: u32 = 2;

impl Agent {
    /// Run one task to completion.
    pub async fn run(&self, options: RunOptions) -> Result<RunReport> {
        let started = std::time::Instant::now();
        let mode = options.mode;
        let session = options
            .session
            .clone()
            .or_else(|| options.resume.clone())
            .unwrap_or_else(|| new_session_id(mode));

        self.emitter.emit(crate::events::Event::Phase {
            name: "session".to_string(),
            detail: format!(
                "{} ({}) provider={} model={} session={session}",
                mode.as_str(),
                mode.description(),
                self.client.provider(),
                self.client.model()
            ),
        });

        // A resumed session already has a header; a new one needs it.
        if !self.sessions.has_header(&session) {
            write_header(
                &self.sessions,
                &session,
                mode,
                &options.task,
                self.client.as_ref(),
                &self.root,
            )?;
        }

        let mut warnings: Vec<String> = Vec::new();
        let mut findings = Vec::new();
        let mut context_block = String::new();

        if mode == Mode::Review {
            self.emitter.emit(crate::events::Event::Phase {
                name: "context".to_string(),
                detail: "reading git diff and scanning for risks".to_string(),
            });
            let (diff, truncated) = crate::git::diff(&self.root, false, None, 200 * 1024).await?;
            let stats = git_tools::diff_stats(&diff);
            findings = git_tools::scan_diff(&diff);
            let mut block = String::new();
            block.push_str(&format!("changed_files: {}\n", stats.len()));
            for stat in &stats {
                block.push_str(&format!(
                    "  {} +{} -{}\n",
                    stat.path, stat.added, stat.removed
                ));
            }
            block.push_str("deterministic_findings:\n");
            if findings.is_empty() {
                block.push_str("  (none)\n");
            }
            for finding in &findings {
                block.push_str(&format!(
                    "  [{}] {} {} - {}\n",
                    finding.severity,
                    finding.kind,
                    finding.path.clone().unwrap_or_else(|| "-".into()),
                    finding.detail
                ));
            }
            if truncated {
                block.push_str("diff_truncated: true\n");
                warnings.push("git diff was truncated before analysis".to_string());
            }
            block.push_str("--- diff ---\n");
            block.push_str(&diff);
            context_block = block;
        }

        let mut messages = self.build_messages(&options, &context_block)?;
        // Record the task itself: without it a resumed session would know what
        // the assistant answered but not what was asked.
        self.sessions.append_message(
            &session,
            &message_record(&Message::user(options.task.clone())),
        )?;
        let ctx = self.tool_context(mode, options.dry_run, &session);
        let mut counters = Counters::default();
        let mut final_text;
        let mut verification: Option<Verification> = None;
        let mut rounds = 0u32;

        loop {
            final_text = self
                .model_loop(&ctx, &options, &mut messages, &session, &mut counters)
                .await?;

            if !(mode == Mode::Edit && options.verify) {
                break;
            }
            self.emitter.emit(crate::events::Event::Phase {
                name: "verify".to_string(),
                detail: "running project verification commands".to_string(),
            });
            let outcome = self.verify(&ctx).await?;
            let passed = outcome.ok;
            let failure = outcome.failure.clone();
            verification = Some(outcome);
            if passed {
                break;
            }
            if rounds >= MAX_VERIFY_ROUNDS {
                warnings.push(format!(
                    "verification still failing after {MAX_VERIFY_ROUNDS} repair round(s)"
                ));
                break;
            }
            rounds += 1;
            let detail = failure
                .as_ref()
                .map(|report| report.summary.clone())
                .unwrap_or_else(|| "verification command failed".to_string());
            self.emitter.emit(crate::events::Event::Phase {
                name: "repair".to_string(),
                detail: format!("{detail} (round {rounds})"),
            });
            messages.push(Message::user(repair_prompt(&detail, failure.as_ref())));
        }

        let files_changed = ctx.changed_files();
        let commands = self.commands.lock().map(|g| g.clone()).unwrap_or_default();
        let patches = self.patches.lock().map(|g| g.clone()).unwrap_or_default();
        let duration_ms = started.elapsed().as_millis() as u64;

        let ok = final_text_error_free(&final_text)
            && counters.tool_calls <= u64::from(self.budgets.max_tool_calls);

        let report = RunReport {
            session: session.clone(),
            mode,
            ok,
            tool_calls: counters.tool_calls,
            model_turns: counters.model_turns,
            files_changed: files_changed.clone(),
            commands,
            patches,
            findings: findings.clone(),
            verification,
            final_text: final_text.clone(),
            usage: counters.usage,
            duration_ms,
            warnings: warnings.clone(),
        };

        self.emitter.emit(crate::events::Event::Done {
            mode: mode.as_str().to_string(),
            ok: report.ok,
            tool_calls: report.tool_calls,
            files_changed: files_changed.len() as u64,
            commands_run: report.commands.len() as u64,
            duration_ms,
        });

        // Keep transcripts bounded without losing the factual work log.
        let limit = self.config.budgets.max_command_output_bytes * 8;
        if self.sessions.needs_compaction(&session, limit) {
            match self.sessions.compact(&session, limit, 4096) {
                Ok(report) => self.emitter.emit(crate::events::Event::Notice {
                    message: format!(
                        "session compacted: {} -> {} bytes ({} event(s) dropped)",
                        report.before_bytes, report.after_bytes, report.dropped_events
                    ),
                }),
                Err(error) => self.emitter.emit(crate::events::Event::Notice {
                    message: format!("session compaction failed: {error}"),
                }),
            }
        }

        Ok(report)
    }

    /// Build the initial conversation.
    fn build_messages(&self, options: &RunOptions, context_block: &str) -> Result<Vec<Message>> {
        let mut messages = vec![Message::system(self.system_prompt(options))];

        if let Some(session) = &options.resume {
            let resumed = self.sessions.resume_messages(session)?;
            if resumed.is_empty() {
                return Err(RaiError::Config(format!(
                    "session `{session}` has no resumable messages"
                )));
            }
            for record in resumed {
                let role = Role::parse(&record.role).unwrap_or(Role::User);
                let calls: Vec<ToolCall> = record
                    .tool_calls
                    .into_iter()
                    .map(|call| ToolCall::new(call.id, call.name, call.arguments))
                    .collect();
                messages.push(Message {
                    role,
                    text: record.text,
                    tool_calls: calls,
                    tool_call_id: record.tool_call_id,
                });
            }
            messages.push(Message::user(options.task.clone()));
        } else {
            let mut task = options.task.clone();
            if !context_block.is_empty() {
                task.push_str("\n\n<runtime_context>\n");
                task.push_str(context_block);
                task.push_str("\n</runtime_context>");
            }
            messages.push(Message::user(task));
        }
        Ok(messages)
    }

    /// The instructions that define the runtime's contract with the model.
    fn system_prompt(&self, options: &RunOptions) -> String {
        let mode = options.mode;
        let mut out = String::new();
        out.push_str(
            "You are rai, a coding assistant running inside a Rust CLI.\n\
             You have no direct access to the machine. You propose tool calls; the runtime \
             validates and executes them, and may refuse a call.\n\n",
        );

        out.push_str("Workspace\n");
        out.push_str(&format!("  root: {}\n", self.root.display()));
        out.push_str(&format!(
            "  indexed files: {} ({} symbols), {} bytes\n",
            self.index.file_count(),
            self.index.symbol_count(),
            self.index.total_bytes()
        ));
        let languages = self.index.languages();
        if !languages.is_empty() {
            let top: Vec<String> = languages
                .iter()
                .take(6)
                .map(|(lang, count)| format!("{lang} ({count})"))
                .collect();
            out.push_str(&format!("  languages: {}\n", top.join(", ")));
        }

        out.push_str(&format!("\nMode: {mode}\n  {}\n", mode.description()));
        out.push_str(&format!(
            "  writes allowed: {}\n  command tools offered: {}\n  approval mode: {}\n",
            if mode.allows_write() {
                "yes, only through apply_patch"
            } else {
                "no"
            },
            if options.allow_commands { "yes" } else { "no" },
            self.policy.approval().as_str()
        ));
        out.push_str(&format!("  policy: {}\n", self.policy.describe()));
        out.push_str(&format!(
            "\nBudgets: at most {} tool calls, {} model turns, {} seconds. \
             Keep patches under {} bytes.\n",
            self.budgets.max_tool_calls,
            self.budgets.max_model_turns,
            self.budgets.max_runtime.as_secs(),
            self.config.budgets.max_patch_bytes
        ));

        out.push_str("\nTools\n");
        out.push_str(
            "  Use the provided functions. Locate code with search_text or list_symbols \
             before reading whole files, and read line ranges for large files.\n",
        );

        let map = self.index.summary(120);
        if !map.is_empty() {
            out.push_str("\nRepository map (paths and symbols, truncated)\n");
            out.push_str(&map);
            out.push('\n');
        }

        let memory = self.memory.prompt_block(30);
        if !memory.is_empty() {
            out.push('\n');
            out.push_str(&memory);
            out.push('\n');
        }

        out.push_str(
            "\nRules\n\
             1. Never claim to have run a command or read a file you did not.\n\
             2. To change files, call apply_patch with a unified diff. Never paste whole file contents.\n\
             3. Keep changes minimal and focused; do not refactor unrelated code.\n\
             4. If a tool call is refused, do not repeat it unchanged; explain what is needed.\n\
             5. If you lack context, request it with a tool call instead of guessing.\n\
             6. Finish with a short factual summary: files changed, commands run, and what remains uncertain.\n",
        );

        if mode.allows_write() {
            out.push_str(
                "\nPatch format\n\
                 --- a/path/to/file.rs\n\
                 +++ b/path/to/file.rs\n\
                 @@ -10,7 +10,8 @@\n\
                  unchanged context line\n\
                 -removed line\n\
                 +added line\n\
                 Use `--- /dev/null` to create a file and `+++ /dev/null` to delete one.\n",
            );
        }

        out
    }
}

/// Text pushed back into the conversation after a failed verification.
fn repair_prompt(detail: &str, failure: Option<&git_tools::FailureReport>) -> String {
    let mut out = String::new();
    out.push_str("Verification failed after your patch.\n");
    out.push_str(&format!("summary: {detail}\n"));
    if let Some(report) = failure {
        if let Some(first) = &report.first_error {
            out.push_str(&format!("first_error: {first}\n"));
        }
        for location in report.locations.iter().take(5) {
            out.push_str(&format!("location: {location}\n"));
        }
        if !report.highlights.is_empty() {
            out.push_str("output:\n");
            for line in report.highlights.iter().take(20) {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out.push_str(
        "Produce a corrected patch against the current file contents, or explain precisely why \
         the failure is unrelated to your change.",
    );
    out
}

/// Whether the final text reports an unrecoverable failure.
fn final_text_error_free(text: &str) -> bool {
    !text.trim_start().starts_with("error:")
}

impl Agent {
    /// One model turn plus the tool calls it requested, repeated until the
    /// model stops asking for tools or a budget stops the loop.
    async fn model_loop(
        &self,
        ctx: &ToolContext,
        options: &RunOptions,
        messages: &mut Vec<Message>,
        session: &str,
        counters: &mut Counters,
    ) -> Result<String> {
        let started = std::time::Instant::now();

        // Identical refusals are counted so a stuck model cannot loop forever.
        let mut refusals: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut stop_after_refusals: Option<String> = None;

        loop {
            self.cancel.check()?;

            if counters.model_turns >= u64::from(self.budgets.max_model_turns) {
                return self.stop(
                    format!(
                        "Stopped: model turn budget ({}) reached.",
                        self.budgets.max_model_turns
                    ),
                    session,
                    counters,
                );
            }
            if started.elapsed() >= self.budgets.max_runtime {
                return self.stop(
                    format!(
                        "Stopped: runtime budget ({}s) reached.",
                        self.budgets.max_runtime.as_secs()
                    ),
                    session,
                    counters,
                );
            }

            counters.model_turns += 1;
            self.emitter.emit(crate::events::Event::Phase {
                name: "model".to_string(),
                detail: format!(
                    "turn {} via {} ({})",
                    counters.model_turns,
                    self.client.provider(),
                    self.client.model()
                ),
            });

            let request = ModelRequest {
                messages: messages.clone(),
                tools: self.registry.specs(options.mode, options.allow_commands),
                max_output_tokens: Some(self.config.model.max_output_tokens),
            };

            let emitter = Arc::clone(&self.emitter);
            let mut sink = |text: &str| {
                emitter.emit(crate::events::Event::ModelDelta {
                    text: text.to_string(),
                });
                emitter.emit_stream_text(text);
            };
            let response = self.client.stream(request, &mut sink).await?;

            if !response.text.is_empty() {
                // Streamed text has no trailing newline of its own.
                self.emitter.emit_stream_text("\n");
            }
            if let Some(usage) = response.usage {
                counters.usage.input_tokens += usage.input_tokens;
                counters.usage.output_tokens += usage.output_tokens;
            }

            let assistant =
                Message::assistant_calls(response.text.clone(), response.tool_calls.clone());
            self.sessions
                .append_message(session, &message_record(&assistant))?;
            messages.push(assistant);

            if response.is_final() {
                return Ok(response.text);
            }

            for call in &response.tool_calls {
                if counters.tool_calls >= u64::from(self.budgets.max_tool_calls) {
                    let notice = format!(
                        "tool call budget ({}) reached; this call was not executed.",
                        self.budgets.max_tool_calls
                    );
                    self.emitter.emit(crate::events::Event::Notice {
                        message: notice.clone(),
                    });
                    let result = Message::tool_result(call.id.clone(), notice);
                    self.sessions
                        .append_message(session, &message_record(&result))?;
                    messages.push(result);
                    continue;
                }

                counters.tool_calls += 1;
                let text = self.execute_tool(ctx, call, options).await;
                if is_unproductive(&text) {
                    let signature = format!("{}|{}", call.name, call.arguments);
                    let seen = refusals.entry(signature).or_insert(0);
                    *seen += 1;
                    if *seen >= MAX_IDENTICAL_UNPRODUCTIVE_CALLS {
                        stop_after_refusals = Some(call.name.clone());
                    }
                }
                let result = Message::tool_result(call.id.clone(), text);
                self.sessions
                    .append_message(session, &message_record(&result))?;
                messages.push(result);
            }

            if let Some(tool) = stop_after_refusals {
                return self.stop(
                    format!(
                        "Stopped: `{tool}` produced the same refused or failing result \
                         {MAX_IDENTICAL_UNPRODUCTIVE_CALLS} times with identical arguments. Change \
                         the approach, or grant the capability in .rai/config.toml (or pass --yes)."
                    ),
                    session,
                    counters,
                );
            }

            if self.cancel.is_cancelled() {
                return Err(RaiError::Cancelled);
            }
        }
    }

    /// Report a budget stop as a normal final answer.
    fn stop(&self, message: String, session: &str, counters: &Counters) -> Result<String> {
        self.emitter.emit(crate::events::Event::Notice {
            message: message.clone(),
        });
        self.emitter.emit(crate::events::Event::Phase {
            name: "stop".to_string(),
            detail: format!(
                "{} tool call(s), {} model turn(s)",
                counters.tool_calls, counters.model_turns
            ),
        });
        let result = Message::assistant(message.clone());
        self.sessions
            .append_message(session, &message_record(&result))?;
        Ok(message)
    }

    /// Evaluate policy, get approval if needed, and run one tool call.
    ///
    /// Failures are returned as text for the model rather than aborting the run:
    /// a rejected patch or a refused command is information, not a crash.
    async fn execute_tool(
        &self,
        ctx: &ToolContext,
        call: &ToolCall,
        options: &RunOptions,
    ) -> String {
        let Some(tool) = self.registry.get(&call.name) else {
            let reason = format!(
                "unknown tool; available in `{}` mode: {}",
                options.mode,
                self.registry.describe(options.mode, options.allow_commands)
            );
            self.emitter.emit(crate::events::Event::Error {
                message: format!("unknown tool `{}`", call.name),
                kind: "unknown_tool".to_string(),
                retry_safe: false,
            });
            return refusal_message(&call.name, &reason);
        };

        let definition = tool.definition();
        let risk = definition.risk.name().to_string();

        if let Some(reason) = mode_refusal(options.mode, definition.risk) {
            self.emitter.emit(crate::events::Event::Approval {
                call_id: call.id.clone(),
                tool: definition.name.clone(),
                risk: risk.clone(),
                decision: "denied".to_string(),
                reason: reason.clone(),
            });
            return refusal_message(&call.name, &reason);
        }

        if !crate::tools::tool_available(options.mode, options.allow_commands, &definition) {
            let reason = format!(
                "`{}` is not part of `{}` mode; command tools require --allow-commands",
                definition.name, options.mode
            );
            self.emitter.emit(crate::events::Event::Approval {
                call_id: call.id.clone(),
                tool: definition.name.clone(),
                risk: risk.clone(),
                decision: "denied".to_string(),
                reason: reason.clone(),
            });
            return refusal_message(&call.name, &reason);
        }

        match self.policy.evaluate(&definition, &call.arguments) {
            crate::policy::Decision::Denied { reason } => {
                self.emitter.emit(crate::events::Event::Approval {
                    call_id: call.id.clone(),
                    tool: definition.name.clone(),
                    risk: risk.clone(),
                    decision: "denied".to_string(),
                    reason: reason.clone(),
                });
                return refusal_message(&call.name, &reason);
            }
            crate::policy::Decision::NeedsApproval { reason } => {
                self.emitter.emit(crate::events::Event::Approval {
                    call_id: call.id.clone(),
                    tool: definition.name.clone(),
                    risk: risk.clone(),
                    decision: "requested".to_string(),
                    reason: reason.clone(),
                });
                let preview =
                    crate::policy::describe_call(&definition, &call.arguments, &ctx.redactor);
                let outcome = self.approver.decide(&definition, &preview, &reason);
                self.emitter.emit(crate::events::Event::Approval {
                    call_id: call.id.clone(),
                    tool: definition.name.clone(),
                    risk: risk.clone(),
                    decision: if outcome.allowed { "allowed" } else { "denied" }.to_string(),
                    reason: outcome.reason.clone(),
                });
                if !outcome.allowed {
                    return refusal_message(&call.name, &outcome.reason);
                }
                if outcome.remember {
                    self.policy.remember(definition.risk);
                }
            }
            crate::policy::Decision::Allowed { reason } => {
                self.emitter.emit(crate::events::Event::Approval {
                    call_id: call.id.clone(),
                    tool: definition.name.clone(),
                    risk: risk.clone(),
                    decision: "allowed".to_string(),
                    reason,
                });
            }
        }

        self.emitter.emit(crate::events::Event::ToolStart {
            call_id: call.id.clone(),
            tool: definition.name.clone(),
            risk,
            args: call.arguments.clone(),
        });

        let started = std::time::Instant::now();
        let outcome = tool.execute(call.arguments.clone(), ctx, &call.id).await;
        let duration_ms = started.elapsed().as_millis() as u64;

        match outcome {
            Ok(outcome) => {
                let (text, truncated) =
                    crate::util::truncate_bytes(&outcome.text, self.tool_result_budget());
                self.emitter.emit(crate::events::Event::ToolFinish {
                    call_id: call.id.clone(),
                    tool: definition.name.clone(),
                    ok: true,
                    duration_ms,
                    summary: outcome.summary.clone(),
                    retry_safe: true,
                });
                if truncated {
                    format!("{text}\n[tool output truncated by the rai runtime]")
                } else {
                    text
                }
            }
            Err(error) => {
                self.emitter.emit(crate::events::Event::ToolFinish {
                    call_id: call.id.clone(),
                    tool: definition.name.clone(),
                    ok: false,
                    duration_ms,
                    summary: error.to_string(),
                    retry_safe: error.retry_safe(),
                });
                self.emitter.emit(crate::events::Event::Error {
                    message: error.to_string(),
                    kind: error.kind().to_string(),
                    retry_safe: error.retry_safe(),
                });
                tool_error_text(&call.name, &error)
            }
        }
    }

    /// How much tool output may enter the conversation.
    fn tool_result_budget(&self) -> usize {
        self.config.budgets.max_command_output_bytes.max(16 * 1024)
    }

    /// Run the cheapest project verification commands after an edit.
    async fn verify(&self, ctx: &ToolContext) -> Result<Verification> {
        let mut commands = crate::tools::command_tool::infer_verification_commands(&self.root);
        if commands.is_empty() {
            self.emitter.emit(crate::events::Event::Notice {
                message: "no verification command could be inferred for this workspace".to_string(),
            });
            return Ok(Verification {
                ok: true,
                steps: Vec::new(),
                failure: None,
            });
        }
        commands.truncate(2);

        let mut steps = Vec::new();
        let mut failure = None;
        let mut ok = true;

        for argv in commands {
            self.cancel.check()?;
            if let crate::policy::Decision::Denied { reason } = self.policy.command_decision(&argv)
            {
                self.emitter.emit(crate::events::Event::Notice {
                    message: format!("skipping verification `{}`: {reason}", argv.join(" ")),
                });
                continue;
            }

            let spec = crate::sandbox::CommandSpec::new(argv.clone(), self.root.clone())
                .with_timeout(std::time::Duration::from_secs(
                    self.config.commands.timeout_seconds,
                ))
                .with_max_output(self.config.budgets.max_command_output_bytes)
                .with_env(self.config.commands.allowed_env.clone());

            let emitter = Arc::clone(&self.emitter);
            let outcome = crate::sandbox::run(&spec, &self.cancel, &ctx.redactor, |kind, chunk| {
                emitter.emit(crate::events::Event::ToolStream {
                    call_id: "verify".to_string(),
                    stream: kind.as_str().to_string(),
                    chunk: chunk.to_string(),
                });
            })
            .await?;

            ctx.record_command(&outcome);
            self.emitter.emit(crate::events::Event::CommandResult {
                argv: outcome.argv.clone(),
                exit_code: outcome.exit_code,
                timed_out: outcome.timed_out,
                duration_ms: outcome.duration_ms,
                stdout_bytes: outcome.stdout_bytes,
                stderr_bytes: outcome.stderr_bytes,
                truncated: outcome.truncated,
                redactions: outcome.redactions,
            });

            let step_ok = outcome.success();
            if !step_ok {
                ok = false;
                failure = Some(git_tools::explain_failure(&outcome.combined()));
            }
            steps.push(VerificationStep {
                argv,
                ok: step_ok,
                exit_code: outcome.exit_code,
                summary: outcome.summary(),
            });
            if !step_ok {
                break;
            }
        }

        Ok(Verification { ok, steps, failure })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_prefixed_text_is_not_a_success() {
        assert!(final_text_error_free("all good"));
        assert!(!final_text_error_free("error: could not complete"));
    }

    #[test]
    fn budgets_come_from_configuration() {
        let mut config = Config::default();
        config.budgets.max_tool_calls = 7;
        config.budgets.max_model_turns = 3;
        let budgets = Budgets::from_config(&config);
        assert_eq!(budgets.max_tool_calls, 7);
        assert_eq!(budgets.max_model_turns, 3);
    }

    #[test]
    fn session_ids_are_prefixed_by_mode() {
        let id = new_session_id(Mode::Edit);
        assert!(id.starts_with("edit-"), "{id}");
    }

    #[test]
    fn mode_refusals_keep_ask_and_review_read_only() {
        assert!(mode_refusal(Mode::Ask, RiskClass::WorkspaceWrite).is_some());
        assert!(mode_refusal(Mode::Ask, RiskClass::Command).is_some());
        assert!(mode_refusal(Mode::Ask, RiskClass::ReadOnly).is_none());
        assert!(mode_refusal(Mode::Review, RiskClass::Command).is_some());
        assert!(mode_refusal(Mode::Edit, RiskClass::WorkspaceWrite).is_none());
    }

    #[test]
    fn repair_prompt_includes_locations() {
        let report =
            git_tools::explain_failure("error[E0308]: mismatched types\n  --> src/main.rs:4:5\n");
        let prompt = repair_prompt(&report.summary, Some(&report));
        assert!(prompt.contains("src/main.rs:4:5"));
        assert!(prompt.contains("corrected patch"));
    }
}
