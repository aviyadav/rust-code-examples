//! The tool boundary: risk classes, schemas, and the registry.
//!
//! Tools are not `fn(Value) -> Value`. Each one carries metadata — name,
//! schema, risk class, execution mode, and retry safety — because that metadata
//! is how the assistant avoids treating `read_file` and `delete_workspace` as
//! morally equivalent JSON-RPC calls.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;
use crate::error::Result;
use crate::events::Emitter;
use crate::index::RepoIndex;
use crate::patch::AppliedFile;
use crate::policy::PolicyEngine;
use crate::redact::Redactor;
use crate::sandbox::CommandOutcome;
use crate::util::Cancel;

pub mod command_tool;
pub mod git_tools;
pub mod patch_tool;
pub mod read_tools;
pub mod web_tools;

/// How dangerous a tool is. Policy is written against these classes, not names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RiskClass {
    /// Listing, reading, inspecting git state.
    ReadOnly,
    /// Creating files or applying patches inside the workspace.
    WorkspaceWrite,
    /// Running tests, builds, formatters, linters.
    Command,
    /// Fetching docs or calling remote APIs.
    Network,
    /// Deleting files, resetting git state, rewriting history.
    Destructive,
    /// Reading secrets, cloud accounts, or private services.
    CredentialSensitive,
}

impl RiskClass {
    /// Every class, in increasing order of risk.
    pub const ALL: [RiskClass; 6] = [
        RiskClass::ReadOnly,
        RiskClass::WorkspaceWrite,
        RiskClass::Command,
        RiskClass::Network,
        RiskClass::Destructive,
        RiskClass::CredentialSensitive,
    ];

    /// Canonical names, as used in configuration files.
    pub const ALL_NAMES: [&'static str; 6] = [
        "ReadOnly",
        "WorkspaceWrite",
        "Command",
        "Network",
        "Destructive",
        "CredentialSensitive",
    ];

    /// Parse a configuration name. Accepts case-insensitive and snake_case
    /// spellings so a config file does not fail on style.
    pub fn parse(value: &str) -> Option<Self> {
        let normalized: String = value
            .trim()
            .chars()
            .filter(|c| *c != '_' && *c != '-' && *c != ' ')
            .flat_map(|c| c.to_lowercase())
            .collect();
        match normalized.as_str() {
            "readonly" => Some(Self::ReadOnly),
            "workspacewrite" => Some(Self::WorkspaceWrite),
            "command" => Some(Self::Command),
            "network" => Some(Self::Network),
            "destructive" => Some(Self::Destructive),
            "credentialsensitive" => Some(Self::CredentialSensitive),
            _ => None,
        }
    }

    /// Canonical name.
    pub fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "ReadOnly",
            Self::WorkspaceWrite => "WorkspaceWrite",
            Self::Command => "Command",
            Self::Network => "Network",
            Self::Destructive => "Destructive",
            Self::CredentialSensitive => "CredentialSensitive",
        }
    }
}

impl std::fmt::Display for RiskClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Where a tool actually runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    /// Implemented in this process.
    Local,
    /// Provided by an MCP server.
    Mcp { server: String },
}

/// Everything the runtime knows about a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments.
    pub schema: Value,
    pub risk: RiskClass,
    pub execution: ExecutionMode,
    /// Whether repeating the call is safe after an ambiguous failure.
    pub retry_safe: bool,
}

impl ToolDefinition {
    /// Define a locally implemented tool.
    pub fn local(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
        risk: RiskClass,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            risk,
            execution: ExecutionMode::Local,
            retry_safe: matches!(risk, RiskClass::ReadOnly | RiskClass::Network),
        }
    }

    /// Define a tool exposed by an MCP server.
    pub fn mcp(
        server: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
        risk: RiskClass,
    ) -> Self {
        let mut def = Self::local(name, description, schema, risk);
        def.execution = ExecutionMode::Mcp {
            server: server.into(),
        };
        def
    }

    /// The schema as presented to a model.
    pub fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.schema.clone(),
        }
    }
}

/// A tool as described to a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// What a tool hands back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    /// Text given to the model.
    pub text: String,
    /// Structured payload for logs and MCP clients.
    pub data: Option<Value>,
    /// One-line summary for the event stream.
    pub summary: String,
    /// Whether the text was cut to fit a budget.
    pub truncated: bool,
}

impl ToolOutcome {
    pub fn new(summary: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            data: None,
            summary: summary.into(),
            truncated: false,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn truncated(mut self) -> Self {
        self.truncated = true;
        self
    }

    /// Mark the outcome truncated when `condition` is true.
    #[must_use]
    pub fn truncated_if(mut self, condition: bool) -> Self {
        self.truncated = self.truncated || condition;
        self
    }
}

/// The workflow mode.
///
/// The CLI owns the workflow, so risk is a property of the command, not of a
/// prompt. `ask` cannot write files, `edit` cannot run arbitrary commands
/// unless explicitly enabled, and `run` never involves a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Ask,
    Edit,
    Review,
    Run,
}

impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "edit" => Some(Self::Edit),
            "review" => Some(Self::Review),
            "run" => Some(Self::Run),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Edit => "edit",
            Self::Review => "review",
            Self::Run => "run",
        }
    }

    /// One-line description used in banners and the system prompt.
    pub fn description(self) -> &'static str {
        match self {
            Self::Ask => "answer questions using repository search and file reads",
            Self::Edit => "propose and apply patches inside the workspace",
            Self::Review => "inspect the git diff and report risks",
            Self::Run => "execute an approved project command",
        }
    }

    /// Whether this mode may write into the workspace.
    pub fn allows_write(self) -> bool {
        matches!(self, Self::Edit)
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a tool is part of a mode's surface.
///
/// `allow_commands` is the explicit `--allow-commands` opt-in that lets `edit`
/// verify its own work; without it, command tools are not even advertised.
pub fn tool_available(mode: Mode, allow_commands: bool, tool: &ToolDefinition) -> bool {
    match mode {
        Mode::Ask => matches!(tool.risk, RiskClass::ReadOnly | RiskClass::Network),
        Mode::Review => matches!(tool.risk, RiskClass::ReadOnly),
        Mode::Edit => match tool.risk {
            RiskClass::ReadOnly | RiskClass::WorkspaceWrite => true,
            RiskClass::Command => allow_commands,
            _ => false,
        },
        Mode::Run => false,
    }
}

/// Shared state handed to every tool call.
///
/// Tools are deliberately incapable of reaching outside this struct: they get
/// the workspace root, the policy engine, the redactor, and the cancellation
/// flag, and nothing else.
pub struct ToolContext {
    pub root: PathBuf,
    pub config: Arc<Config>,
    pub policy: Arc<PolicyEngine>,
    pub index: Arc<RepoIndex>,
    pub emitter: Arc<Emitter>,
    pub redactor: Arc<Redactor>,
    pub cancel: Cancel,
    pub session: String,
    pub mode: Mode,
    /// Validate patches without touching the workspace.
    pub dry_run: bool,
    pub commands: Arc<std::sync::Mutex<Vec<CommandOutcome>>>,
    pub patches: Arc<std::sync::Mutex<Vec<AppliedFile>>>,
}

impl ToolContext {
    /// Resolve a model-supplied path inside the workspace.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf> {
        if Path::new(raw).is_absolute() && !self.config.workspace.allow_absolute_paths {
            return Err(crate::error::RaiError::AbsolutePath(raw.to_string()));
        }
        crate::util::safe_join(&self.root, raw)
    }

    /// Record a command in the run transcript.
    pub fn record_command(&self, outcome: &CommandOutcome) {
        if let Ok(mut guard) = self.commands.lock() {
            guard.push(outcome.clone());
        }
    }

    /// Record applied patch files.
    pub fn record_patch(&self, files: &[AppliedFile]) {
        if let Ok(mut guard) = self.patches.lock() {
            guard.extend(files.iter().cloned());
        }
    }

    /// Number of commands run so far in this session.
    pub fn command_count(&self) -> usize {
        self.commands.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// Workspace-relative paths changed so far, de-duplicated and sorted.
    pub fn changed_files(&self) -> Vec<String> {
        let mut files: Vec<String> = self
            .patches
            .lock()
            .map(|g| g.iter().map(|f| f.path.clone()).collect())
            .unwrap_or_default();
        files.sort();
        files.dedup();
        files
    }

    /// Emit an event on the session stream.
    pub fn emit(&self, event: crate::events::Event) {
        self.emitter.emit(event);
    }
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("root", &self.root)
            .field("mode", &self.mode)
            .field("dry_run", &self.dry_run)
            .field("session", &self.session)
            .finish()
    }
}

/// One executable capability.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Static metadata: name, description, schema, risk, execution mode.
    fn definition(&self) -> ToolDefinition;

    /// Execute the call. Implementations must validate arguments themselves:
    /// the model is not trusted to send well-formed input.
    async fn execute(&self, args: Value, ctx: &ToolContext, call_id: &str) -> Result<ToolOutcome>;
}

/// Name-addressable set of tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool, returning `self` for chaining.
    #[must_use]
    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.definition().name;
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn definition(&self, name: &str) -> Option<ToolDefinition> {
        self.tools.get(name).map(|t| t.definition())
    }

    /// Definitions for every registered tool, ordered by name.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// True when `name` exists and belongs to the mode's surface.
    pub fn available(&self, name: &str, mode: Mode, allow_commands: bool) -> bool {
        self.definition(name)
            .map(|def| tool_available(mode, allow_commands, &def))
            .unwrap_or(false)
    }

    /// Tool specs advertised to a model for one mode.
    pub fn specs(&self, mode: Mode, allow_commands: bool) -> Vec<ToolSpec> {
        self.definitions()
            .into_iter()
            .filter(|def| tool_available(mode, allow_commands, def))
            .map(|def| def.spec())
            .collect()
    }

    /// Human-readable tool surface, used by `rai config show` and banners.
    pub fn describe(&self, mode: Mode, allow_commands: bool) -> String {
        self.definitions()
            .into_iter()
            .filter(|def| tool_available(mode, allow_commands, def))
            .map(|def| format!("{} ({})", def.name, def.risk))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Every locally implemented tool.
///
/// Tools that are not part of a mode's surface are still registered: the mode
/// filter decides what the model is told about, and policy decides what may
/// actually run.
pub fn builtin() -> ToolRegistry {
    ToolRegistry::new()
        .with(Arc::new(read_tools::ListFiles))
        .with(Arc::new(read_tools::ReadFile))
        .with(Arc::new(read_tools::SearchText))
        .with(Arc::new(read_tools::ListSymbols))
        .with(Arc::new(git_tools::GitStatus))
        .with(Arc::new(git_tools::GitDiff))
        .with(Arc::new(git_tools::SummarizeDiff))
        .with(Arc::new(git_tools::ExplainFailure))
        .with(Arc::new(patch_tool::ApplyPatch))
        .with(Arc::new(command_tool::RunCommand))
        .with(Arc::new(command_tool::RunTests))
        .with(Arc::new(web_tools::FetchDocs))
}

/// Parse a required string argument.
pub fn require_str(args: &Value, key: &str, tool: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| crate::error::RaiError::InvalidArguments {
            tool: tool.to_string(),
            problem: format!("missing required string argument `{key}`"),
        })
}

/// Parse an optional string argument.
pub fn optional_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
}

/// Parse an optional non-negative integer argument.
pub fn optional_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| v.as_u64()).map(|v| v as usize)
}

/// Parse an optional boolean argument.
pub fn optional_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

/// Parse a required array of strings.
pub fn require_string_array(args: &Value, key: &str, tool: &str) -> Result<Vec<String>> {
    let array = args.get(key).and_then(|v| v.as_array()).ok_or_else(|| {
        crate::error::RaiError::InvalidArguments {
            tool: tool.to_string(),
            problem: format!("missing required array argument `{key}`"),
        }
    })?;
    let values: Vec<String> = array
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if values.is_empty() || values.len() != array.len() {
        return Err(crate::error::RaiError::InvalidArguments {
            tool: tool.to_string(),
            problem: format!("`{key}` must be a non-empty array of strings"),
        });
    }
    Ok(values)
}

/// Convenience schema builder for object arguments.
pub fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_class_names_round_trip() {
        for name in RiskClass::ALL_NAMES {
            let parsed = RiskClass::parse(name).expect("parses");
            assert_eq!(parsed.name(), name);
        }
        assert_eq!(RiskClass::parse("read_only"), Some(RiskClass::ReadOnly));
        assert_eq!(
            RiskClass::parse("workspace-write"),
            Some(RiskClass::WorkspaceWrite)
        );
        assert_eq!(RiskClass::parse("nonsense"), None);
    }

    #[test]
    fn ask_mode_hides_write_and_command_tools() {
        let registry = builtin();
        let specs = registry.specs(Mode::Ask, false);
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"apply_patch"));
        assert!(!names.contains(&"run_command"));
    }

    #[test]
    fn edit_mode_offers_patch_but_not_commands_by_default() {
        let registry = builtin();
        let names: Vec<String> = registry
            .specs(Mode::Edit, false)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"apply_patch".to_string()));
        assert!(!names.contains(&"run_command".to_string()));

        let with_commands: Vec<String> = registry
            .specs(Mode::Edit, true)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(with_commands.contains(&"run_command".to_string()));
    }

    #[test]
    fn review_mode_is_read_only() {
        let registry = builtin();
        for def in registry.definitions() {
            if tool_available(Mode::Review, false, &def) {
                assert_eq!(def.risk, RiskClass::ReadOnly, "{} leaked", def.name);
            }
        }
    }

    #[test]
    fn run_mode_has_no_model_tools() {
        let registry = builtin();
        assert!(registry.specs(Mode::Run, true).is_empty());
    }

    #[test]
    fn registry_lookup_reports_availability() {
        let registry = builtin();
        assert!(registry.available("apply_patch", Mode::Edit, false));
        assert!(!registry.available("apply_patch", Mode::Ask, false));
        assert!(!registry.available("does_not_exist", Mode::Edit, true));
    }

    #[test]
    fn argument_helpers_reject_bad_input() {
        assert!(require_str(&json!({}), "path", "read_file").is_err());
        assert!(require_str(&json!({"path": "  "}), "path", "read_file").is_err());
        assert_eq!(
            require_str(&json!({"path": "src/lib.rs"}), "path", "read_file").unwrap(),
            "src/lib.rs"
        );
        assert!(require_string_array(&json!({"argv": []}), "argv", "run_command").is_err());
        assert!(
            require_string_array(&json!({"argv": ["cargo", 1]}), "argv", "run_command").is_err()
        );
        assert_eq!(optional_usize(&json!({"limit": 5}), "limit"), Some(5));
        assert_eq!(
            optional_bool(&json!({"dry_run": true}), "dry_run"),
            Some(true)
        );
    }
}
