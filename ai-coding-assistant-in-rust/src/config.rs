//! Configuration: explicit, validated, and honest about risk.
//!
//! The assistant must not silently grow capabilities. Every new command class,
//! writable root, environment passthrough, or provider is declared here and
//! validated before a run starts. Unknown keys are rejected so typos in a trust
//! setting fail loudly instead of silently falling back to a default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{RaiError, Result};
use crate::tools::RiskClass;
use crate::util::normalize;
use crate::METADATA_DIR;

/// Approval policy for tool calls that are not automatically allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalMode {
    /// Anything not explicitly allowed is denied.
    Deny,
    /// Anything not explicitly allowed asks the user.
    Prompt,
    /// Anything not explicitly denied is allowed. For CI and scripted runs.
    Auto,
}

impl ApprovalMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "deny" => Some(Self::Deny),
            "prompt" => Some(Self::Prompt),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Prompt => "prompt",
            Self::Auto => "auto",
        }
    }
}

/// Which model backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Any OpenAI-compatible `/chat/completions` endpoint.
    Openai,
    /// Offline heuristic planner. No network, deterministic.
    Local,
    /// Deterministic replay of a scripted response list. Test/demo only.
    Scripted,
}

impl Provider {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" | "openai-compatible" | "compatible" => Some(Self::Openai),
            "local" | "offline" => Some(Self::Local),
            "scripted" | "script" => Some(Self::Scripted),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Local => "local",
            Self::Scripted => "scripted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    /// `openai`, `local`, or `scripted`.
    pub provider: String,
    pub model: String,
    /// Base URL for OpenAI-compatible endpoints (OpenAI, Ollama, LM Studio, ...).
    pub base_url: String,
    /// Environment variable holding the API key. Never a literal key.
    pub api_key_env: String,
    pub max_output_tokens: u32,
    pub temperature: f32,
    /// Stream model output when the provider supports it.
    pub stream: bool,
    /// Path to a scripted response file, when `provider = "scripted"`.
    pub script: Option<PathBuf>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "local".to_string(),
            model: "local-heuristic".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            max_output_tokens: 2048,
            temperature: 0.0,
            stream: true,
            script: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Workspace root, relative to the project root that owns `.rai/`.
    pub root: PathBuf,
    pub allow_writes: bool,
    pub allow_absolute_paths: bool,
    pub max_file_bytes: u64,
    /// Extra directory names or globs excluded from indexing and search.
    pub exclude: Vec<String>,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            allow_writes: true,
            allow_absolute_paths: false,
            max_file_bytes: 1_048_576,
            exclude: vec![
                "target".into(),
                "node_modules".into(),
                "dist".into(),
                "build".into(),
                ".venv".into(),
                "venv".into(),
                "__pycache__".into(),
                ".git".into(),
                METADATA_DIR.into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandsConfig {
    /// Command prefixes that may run without an approval prompt.
    pub auto_allow: Vec<String>,
    /// Command prefixes that are always refused. Checked before `auto_allow`.
    pub deny: Vec<String>,
    pub timeout_seconds: u64,
    pub max_output_bytes: usize,
    /// Extra environment variables to pass through, beyond the safe baseline.
    pub allowed_env: Vec<String>,
    /// Permit shell interpretation (`sh -c` / `cmd /C`). Off by default.
    pub allow_shell: bool,
    pub redact_output: bool,
}

impl Default for CommandsConfig {
    fn default() -> Self {
        Self {
            auto_allow: vec![
                "cargo check".into(),
                "cargo test".into(),
                "cargo fmt --check".into(),
                "cargo clippy".into(),
                "cargo build".into(),
                "git status".into(),
                "git diff".into(),
            ],
            deny: vec![
                "git push".into(),
                "git reset --hard".into(),
                "git clean".into(),
                "rm -rf".into(),
                "shutdown".into(),
            ],
            timeout_seconds: 300,
            max_output_bytes: 65_536,
            allowed_env: vec![],
            allow_shell: false,
            redact_output: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    /// `deny`, `prompt`, or `auto`.
    pub approval: String,
    /// Risk classes that run without an approval prompt.
    pub auto_approve: Vec<String>,
    /// Allow destructive tools even when approved interactively.
    pub allow_destructive: bool,
    /// Allow credential-sensitive tools.
    pub allow_credential_sensitive: bool,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            approval: "prompt".into(),
            auto_approve: vec!["ReadOnly".into()],
            allow_destructive: false,
            allow_credential_sensitive: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BudgetConfig {
    pub max_tool_calls: u32,
    pub max_runtime_seconds: u64,
    pub max_patch_bytes: usize,
    pub max_command_output_bytes: usize,
    pub max_model_turns: u32,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_tool_calls: 40,
            max_runtime_seconds: 900,
            max_patch_bytes: 512_000,
            max_command_output_bytes: 65_536,
            max_model_turns: 12,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchConfig {
    pub prefer_ripgrep: bool,
    pub max_results: usize,
    pub max_indexed_files: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            prefer_ripgrep: true,
            max_results: 50,
            max_indexed_files: 20_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    /// Only `stdio` is implemented.
    pub transport: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub enabled: bool,
    pub timeout_seconds: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: "stdio".into(),
            command: String::new(),
            args: vec![],
            env: BTreeMap::new(),
            enabled: true,
            timeout_seconds: 30,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub servers: Vec<McpServerConfig>,
}

/// The whole configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where the config was loaded from, when it came from disk.
    #[serde(skip)]
    pub source: Option<PathBuf>,
    /// Directory that `workspace.root` is resolved against.
    #[serde(skip)]
    pub base_dir: PathBuf,

    pub model: ModelConfig,
    pub workspace: WorkspaceConfig,
    pub commands: CommandsConfig,
    pub policy: PolicyConfig,
    pub budgets: BudgetConfig,
    pub search: SearchConfig,
    pub mcp: McpConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source: None,
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            model: ModelConfig::default(),
            workspace: WorkspaceConfig::default(),
            commands: CommandsConfig::default(),
            policy: PolicyConfig::default(),
            budgets: BudgetConfig::default(),
            search: SearchConfig::default(),
            mcp: McpConfig::default(),
        }
    }
}

impl Config {
    /// Load configuration.
    ///
    /// Order: explicit `--config`, then `.rai/config.toml` found by walking up
    /// from `cwd`, then built-in defaults. Environment overrides are applied
    /// last so CI can pin behaviour without editing files.
    pub fn discover(explicit: Option<&Path>, cwd: &Path) -> Result<Self> {
        let path = match explicit {
            Some(p) => {
                let candidate = if p.is_dir() {
                    p.join(METADATA_DIR).join("config.toml")
                } else {
                    p.to_path_buf()
                };
                if !candidate.exists() {
                    return Err(RaiError::Config(format!(
                        "config file not found: {}",
                        candidate.display()
                    )));
                }
                Some(normalize(&candidate))
            }
            None => find_config_upwards(cwd),
        };

        let mut config = match &path {
            Some(path) => Self::load_file(path)?,
            None => Self {
                base_dir: normalize(cwd),
                ..Self::default()
            },
        };

        config.source = path;
        config.apply_env()?;
        config.validate()?;
        Ok(config)
    }

    /// Parse and validate a specific config file.
    pub fn load_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| RaiError::Config(format!("cannot read {}: {e}", path.display())))?;
        let mut config: Config = toml::from_str(&text)
            .map_err(|e| RaiError::Config(format!("cannot parse {}: {e}", path.display())))?;
        config.base_dir = project_root_for(path);
        config.source = Some(normalize(path));
        Ok(config)
    }

    /// Apply `RAI_*` environment overrides.
    pub fn apply_env(&mut self) -> Result<()> {
        if let Ok(v) = std::env::var("RAI_PROVIDER") {
            self.model.provider = v;
        }
        if let Ok(v) = std::env::var("RAI_MODEL") {
            self.model.model = v;
        }
        if let Ok(v) = std::env::var("RAI_BASE_URL") {
            self.model.base_url = v;
        }
        if let Ok(v) = std::env::var("RAI_API_KEY_ENV") {
            self.model.api_key_env = v;
        }
        if let Ok(v) = std::env::var("RAI_SCRIPT") {
            self.model.script = Some(PathBuf::from(v));
        }
        if let Ok(v) = std::env::var("RAI_APPROVAL") {
            self.policy.approval = v;
        }
        if let Ok(v) = std::env::var("RAI_WORKSPACE") {
            self.workspace.root = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("RAI_ALLOW_WRITES") {
            self.workspace.allow_writes = matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
        }
        Ok(())
    }

    /// Resolve the workspace root to an absolute, lexically normalized path.
    pub fn workspace_root(&self) -> PathBuf {
        let root = &self.workspace.root;
        let joined = if root.is_absolute() {
            root.clone()
        } else {
            self.base_dir.join(root)
        };
        normalize(&joined)
    }

    /// The `.rai` metadata directory for this project.
    pub fn metadata_dir(&self) -> PathBuf {
        self.workspace_root().join(METADATA_DIR)
    }

    pub fn provider(&self) -> Result<Provider> {
        Provider::parse(&self.model.provider).ok_or_else(|| {
            RaiError::Config(format!(
                "unknown model provider `{}` (expected openai, local, or scripted)",
                self.model.provider
            ))
        })
    }

    pub fn approval(&self) -> Result<ApprovalMode> {
        ApprovalMode::parse(&self.policy.approval).ok_or_else(|| {
            RaiError::Config(format!(
                "unknown approval mode `{}` (expected deny, prompt, or auto)",
                self.policy.approval
            ))
        })
    }

    /// Risk classes that do not require an approval prompt.
    pub fn auto_approve(&self) -> Result<Vec<RiskClass>> {
        self.policy
            .auto_approve
            .iter()
            .map(|name| {
                RiskClass::parse(name).ok_or_else(|| {
                    RaiError::Config(format!(
                        "unknown risk class `{name}` in [policy].auto_approve (expected {})",
                        RiskClass::ALL_NAMES.join(", ")
                    ))
                })
            })
            .collect()
    }

    /// Validate cross-field invariants before any work starts.
    pub fn validate(&self) -> Result<()> {
        let _ = self.provider()?;
        let _ = self.approval()?;
        let _ = self.auto_approve()?;

        let root = self.workspace_root();
        if !root.exists() {
            return Err(RaiError::Config(format!(
                "workspace root does not exist: {}",
                root.display()
            )));
        }
        if !root.is_dir() {
            return Err(RaiError::Config(format!(
                "workspace root is not a directory: {}",
                root.display()
            )));
        }
        if self.workspace.max_file_bytes == 0 {
            return Err(RaiError::Config(
                "[workspace].max_file_bytes must be greater than 0".into(),
            ));
        }
        if self.budgets.max_tool_calls == 0 {
            return Err(RaiError::Config(
                "[budgets].max_tool_calls must be greater than 0".into(),
            ));
        }
        if self.budgets.max_model_turns == 0 {
            return Err(RaiError::Config(
                "[budgets].max_model_turns must be greater than 0".into(),
            ));
        }
        if self.budgets.max_runtime_seconds == 0 {
            return Err(RaiError::Config(
                "[budgets].max_runtime_seconds must be greater than 0".into(),
            ));
        }
        if self.commands.timeout_seconds == 0 {
            return Err(RaiError::Config(
                "[commands].timeout_seconds must be greater than 0".into(),
            ));
        }
        if self.commands.max_output_bytes == 0 {
            return Err(RaiError::Config(
                "[commands].max_output_bytes must be greater than 0".into(),
            ));
        }
        if let Some(script) = &self.model.script {
            if self.provider()? == Provider::Scripted {
                let resolved = if script.is_absolute() {
                    script.clone()
                } else {
                    self.base_dir.join(script)
                };
                if !resolved.exists() {
                    return Err(RaiError::Config(format!(
                        "[model].script not found: {}",
                        resolved.display()
                    )));
                }
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for server in &self.mcp.servers {
            if server.name.trim().is_empty() {
                return Err(RaiError::Config(
                    "[[mcp.servers]] requires a non-empty name".into(),
                ));
            }
            if !seen.insert(server.name.clone()) {
                return Err(RaiError::Config(format!(
                    "duplicate MCP server name `{}`",
                    server.name
                )));
            }
            if server.transport != "stdio" {
                return Err(RaiError::Config(format!(
                    "MCP server `{}` uses unsupported transport `{}` (only stdio is implemented)",
                    server.name, server.transport
                )));
            }
            if server.command.trim().is_empty() {
                return Err(RaiError::Config(format!(
                    "MCP server `{}` requires a command",
                    server.name
                )));
            }
        }

        for entry in &self.commands.auto_allow {
            if entry.trim().is_empty() {
                return Err(RaiError::Config(
                    "[commands].auto_allow contains an empty entry".into(),
                ));
            }
        }
        Ok(())
    }

    /// Render the human-facing summary used by `rai config show`.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "config file     {}\n",
            self.source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<built-in defaults>".to_string())
        ));
        out.push_str(&format!("project root    {}\n", self.base_dir.display()));
        out.push_str(&format!(
            "workspace root  {}\n",
            self.workspace_root().display()
        ));
        out.push_str(&format!(
            "model           provider={} model={} base_url={} stream={}\n",
            self.model.provider, self.model.model, self.model.base_url, self.model.stream
        ));
        out.push_str(&format!(
            "writes          allow_writes={} allow_absolute_paths={} max_file_bytes={}\n",
            self.workspace.allow_writes,
            self.workspace.allow_absolute_paths,
            self.workspace.max_file_bytes
        ));
        out.push_str(&format!(
            "approval        mode={} auto_approve=[{}]\n",
            self.policy.approval,
            self.policy.auto_approve.join(", ")
        ));
        out.push_str(&format!(
            "commands        timeout={}s max_output={} shell={} auto_allow=[{}]\n",
            self.commands.timeout_seconds,
            self.commands.max_output_bytes,
            self.commands.allow_shell,
            self.commands.auto_allow.join("; ")
        ));
        if !self.commands.deny.is_empty() {
            out.push_str(&format!(
                "                deny=[{}]\n",
                self.commands.deny.join("; ")
            ));
        }
        out.push_str(&format!(
            "budgets         tool_calls={} model_turns={} runtime={}s patch_bytes={}\n",
            self.budgets.max_tool_calls,
            self.budgets.max_model_turns,
            self.budgets.max_runtime_seconds,
            self.budgets.max_patch_bytes
        ));
        out.push_str(&format!(
            "search          ripgrep={} max_results={}\n",
            self.search.prefer_ripgrep, self.search.max_results
        ));
        out.push_str(&format!("mcp servers     {}\n", self.mcp.servers.len()));
        for server in &self.mcp.servers {
            out.push_str(&format!(
                "                {} {} {} {} (enabled={})\n",
                server.name,
                server.command,
                server.args.join(" "),
                server.transport,
                server.enabled
            ));
        }
        out
    }

    /// The commented template written by `rai init`.
    pub fn template() -> String {
        TEMPLATE.to_string()
    }
}

/// Find `.rai/config.toml` walking up from `start`.
pub fn find_config_upwards(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start.to_path_buf());
    while let Some(current) = dir {
        let candidate = current.join(METADATA_DIR).join("config.toml");
        if candidate.is_file() {
            return Some(normalize(&candidate));
        }
        dir = current.parent().map(|p| p.to_path_buf());
    }
    None
}

/// The project root that owns a config file: the parent of `.rai/` when the
/// config lives there, otherwise the config file's own directory.
fn project_root_for(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    if parent.file_name().and_then(|n| n.to_str()) == Some(METADATA_DIR) {
        parent
            .parent()
            .map(normalize)
            .unwrap_or_else(|| normalize(Path::new(".")))
    } else {
        normalize(parent)
    }
}

const TEMPLATE: &str = r#"# rai project configuration.
#
# This file is the trust boundary. Everything the assistant is allowed to do
# should be visible here, and nothing should grow silently.

[model]
# openai | local | scripted
#   openai   - any OpenAI-compatible /chat/completions endpoint
#   local    - offline heuristic planner, no network, deterministic
#   scripted - replay a fixed response list (tests and demos)
provider = "local"
model = "local-heuristic"
# Works with OpenAI, Ollama (http://localhost:11434/v1),
# LM Studio (http://localhost:1234/v1), and other compatible servers.
base_url = "https://api.openai.com/v1"
# Name of the environment variable holding the key. Never a literal key.
api_key_env = "OPENAI_API_KEY"
max_output_tokens = 2048
temperature = 0.0
stream = true

[workspace]
root = "."
allow_writes = true
allow_absolute_paths = false
max_file_bytes = 1048576
exclude = ["target", "node_modules", "dist", "build", ".venv", "venv", "__pycache__", ".git", ".rai"]

[commands]
# Command prefixes that may run without an approval prompt. Matched on argv.
auto_allow = [
  "cargo check",
  "cargo test",
  "cargo fmt --check",
  "cargo clippy",
  "cargo build",
  "git status",
  "git diff",
]
# Always refused, checked before auto_allow.
deny = ["git push", "git reset --hard", "git clean", "rm -rf", "shutdown"]
timeout_seconds = 300
max_output_bytes = 65536
# Extra environment variables to pass to child processes.
allowed_env = []
# Shell interpretation (sh -c / cmd /C). Off by default; argv execution is
# preferred because it removes quoting and metacharacter ambiguity.
allow_shell = false
redact_output = true

[policy]
# deny   - anything not explicitly allowed is refused
# prompt - anything not explicitly allowed asks the user
# auto   - anything not explicitly denied is allowed (CI and scripted runs)
approval = "prompt"
# Risk classes that never prompt: ReadOnly, WorkspaceWrite, Command, Network,
# Destructive, CredentialSensitive.
auto_approve = ["ReadOnly"]
allow_destructive = false
allow_credential_sensitive = false

[budgets]
max_tool_calls = 40
max_model_turns = 12
max_runtime_seconds = 900
max_patch_bytes = 512000
max_command_output_bytes = 65536

[search]
prefer_ripgrep = true
max_results = 50
max_indexed_files = 20000

# [[mcp.servers]]
# name = "git"
# transport = "stdio"
# command = "uvx"
# args = ["mcp-server-git"]
# enabled = true
# timeout_seconds = 30
# [mcp.servers.env]
# GIT_DIR = "."
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rai-cfg-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn template_parses_and_validates() {
        let dir = temp_dir("template");
        let rai = dir.join(METADATA_DIR);
        std::fs::create_dir_all(&rai).unwrap();
        let path = rai.join("config.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(Config::template().as_bytes()).unwrap();
        drop(f);

        let config = Config::load_file(&path).unwrap();
        assert_eq!(config.base_dir, normalize(&dir));
        assert_eq!(config.workspace_root(), normalize(&dir));
        config.validate().unwrap();
        assert_eq!(config.provider().unwrap(), Provider::Local);
        assert_eq!(config.approval().unwrap(), ApprovalMode::Prompt);
        assert_eq!(config.auto_approve().unwrap(), vec![RiskClass::ReadOnly]);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let dir = temp_dir("unknown");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[workspace]\nallow_write = true\n").unwrap();
        let err = Config::load_file(&path).unwrap_err();
        assert!(err.to_string().contains("cannot parse"), "{err}");
    }

    #[test]
    fn bad_approval_mode_is_a_clear_error() {
        let dir = temp_dir("badapproval");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[policy]\napproval = \"maybe\"\n").unwrap();
        let mut config = Config::load_file(&path).unwrap();
        config.base_dir = dir.clone();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("unknown approval mode"), "{err}");
    }

    #[test]
    fn missing_workspace_root_fails_validation() {
        let dir = temp_dir("missingroot");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[workspace]\nroot = \"does-not-exist\"\n").unwrap();
        let config = Config::load_file(&path).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("workspace root does not exist"));
    }

    #[test]
    fn discovery_walks_up_to_the_project_root() {
        let dir = temp_dir("discover");
        let rai = dir.join(METADATA_DIR);
        std::fs::create_dir_all(&rai).unwrap();
        std::fs::write(rai.join("config.toml"), "[workspace]\nroot = \".\"\n").unwrap();
        let nested = dir.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();

        let config = Config::discover(None, &nested).unwrap();
        assert_eq!(config.workspace_root(), normalize(&dir));
        assert_eq!(
            config.source.as_ref().unwrap(),
            &normalize(&rai.join("config.toml"))
        );
    }

    #[test]
    fn duplicate_mcp_server_names_are_rejected() {
        let dir = temp_dir("mcpservers");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[[mcp.servers]]
name = "git"
command = "uvx"
args = ["mcp-server-git"]

[[mcp.servers]]
name = "git"
command = "uvx"
args = ["other"]
"#,
        )
        .unwrap();
        let config = Config::load_file(&path).unwrap();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("duplicate MCP server name"));
    }
}
