//! Policy: what the model may do, and what needs a human.
//!
//! Tool execution is the trust boundary, so it is not a transport detail. Risk
//! classes, a command allowlist, and an explicit approval mode decide whether a
//! call runs. The model never gets a raw socket to the machine.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::config::{ApprovalMode, Config};
use crate::error::Result;
use crate::redact::Redactor;
use crate::sandbox::{matches_allowlist, matches_denylist};
use crate::tools::{RiskClass, ToolDefinition};

/// Outcome of evaluating one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run it now.
    Allowed { reason: String },
    /// A human must confirm before it runs.
    NeedsApproval { reason: String },
    /// Never run it.
    Denied { reason: String },
}

impl Decision {
    pub fn allowed(reason: impl Into<String>) -> Self {
        Self::Allowed {
            reason: reason.into(),
        }
    }

    pub fn needs_approval(reason: impl Into<String>) -> Self {
        Self::NeedsApproval {
            reason: reason.into(),
        }
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Self::Denied {
            reason: reason.into(),
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed { .. })
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Self::Denied { .. })
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::Allowed { reason } | Self::NeedsApproval { reason } | Self::Denied { reason } => {
                reason
            }
        }
    }

    /// Stable label used in events and logs.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Allowed { .. } => "allowed",
            Self::NeedsApproval { .. } => "requested",
            Self::Denied { .. } => "denied",
        }
    }
}

/// Decides whether tool calls may run.
#[derive(Debug, Clone)]
pub struct PolicyEngine {
    approval: ApprovalMode,
    auto_approve: BTreeSet<RiskClass>,
    allow_writes: bool,
    allow_shell: bool,
    allow_destructive: bool,
    allow_credential_sensitive: bool,
    auto_allow: Vec<String>,
    deny: Vec<String>,
    /// Risk classes approved interactively with "always" during this run.
    session_approved: Arc<Mutex<BTreeSet<RiskClass>>>,
}

impl PolicyEngine {
    /// Build the engine from validated configuration.
    pub fn from_config(config: &Config) -> Result<Self> {
        Ok(Self {
            approval: config.approval()?,
            auto_approve: config.auto_approve()?.into_iter().collect(),
            allow_writes: config.workspace.allow_writes,
            allow_shell: config.commands.allow_shell,
            allow_destructive: config.policy.allow_destructive,
            allow_credential_sensitive: config.policy.allow_credential_sensitive,
            auto_allow: config.commands.auto_allow.clone(),
            deny: config.commands.deny.clone(),
            session_approved: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    pub fn approval(&self) -> ApprovalMode {
        self.approval
    }

    pub fn allow_writes(&self) -> bool {
        self.allow_writes
    }

    /// Record a risk class approved for the rest of this run.
    pub fn remember(&self, risk: RiskClass) {
        if let Ok(mut guard) = self.session_approved.lock() {
            guard.insert(risk);
        }
    }

    fn is_pre_approved(&self, risk: RiskClass) -> bool {
        if self.auto_approve.contains(&risk) {
            return true;
        }
        self.session_approved
            .lock()
            .map(|g| g.contains(&risk))
            .unwrap_or(false)
    }

    /// Evaluate a tool call.
    ///
    /// Command tools are checked against `[commands]`: the deny list wins, then
    /// the allow list auto-approves.
    pub fn evaluate(&self, tool: &ToolDefinition, args: &Value) -> Decision {
        // 1. Hard denials from configuration.
        match tool.risk {
            RiskClass::Destructive if !self.allow_destructive => {
                return Decision::denied(format!(
                    "`{}` is destructive and [policy].allow_destructive is false",
                    tool.name
                ))
            }
            RiskClass::CredentialSensitive if !self.allow_credential_sensitive => {
                return Decision::denied(format!(
                    "`{}` is credential-sensitive and [policy].allow_credential_sensitive is false",
                    tool.name
                ))
            }
            RiskClass::WorkspaceWrite if !self.allow_writes => {
                return Decision::denied(
                    "workspace writes are disabled by [workspace].allow_writes = false",
                )
            }
            _ => {}
        }

        // 2. Command tools consult the allow and deny lists on real argv.
        if tool.risk == RiskClass::Command {
            if let Some(argv) = command_argv(args) {
                return self.command_decision(&argv);
            }
        }

        // 3. Read-only work is routine.
        if tool.risk == RiskClass::ReadOnly {
            return Decision::allowed(format!("{} is read-only", tool.name));
        }

        // 4. Everything else follows the approval mode.
        if self.is_pre_approved(tool.risk) {
            return Decision::allowed(format!(
                "{} is auto-approved for risk class {}",
                tool.name, tool.risk
            ));
        }
        match self.approval {
            ApprovalMode::Auto => Decision::allowed(format!(
                "approval mode is auto and {} is not denied",
                tool.name
            )),
            ApprovalMode::Deny => Decision::denied(format!(
                "approval mode is deny and {} is not auto-approved",
                tool.name
            )),
            ApprovalMode::Prompt => Decision::needs_approval(format!(
                "{} has risk class {} and is not auto-approved",
                tool.name, tool.risk
            )),
        }
    }

    /// Decide on a concrete command line.
    pub fn command_decision(&self, argv: &[String]) -> Decision {
        if argv.is_empty() {
            return Decision::denied("empty command".to_string());
        }
        if let Some(hit) = matches_denylist(argv, &self.deny) {
            return Decision::denied(format!("command matches [commands].deny entry `{hit}`"));
        }
        if let Some(hit) = self
            .auto_allow
            .iter()
            .find(|entry| matches_allowlist(argv, entry))
        {
            return Decision::allowed(format!(
                "command matches [commands].auto_allow entry `{hit}`"
            ));
        }
        if self.is_pre_approved(RiskClass::Command) {
            return Decision::allowed("command risk class is auto-approved");
        }
        match self.approval {
            ApprovalMode::Auto => Decision::allowed("approval mode is auto"),
            ApprovalMode::Deny => Decision::denied(
                "command is not in [commands].auto_allow and approval mode is deny",
            ),
            ApprovalMode::Prompt => Decision::needs_approval(
                "command is not in [commands].auto_allow, so it needs approval",
            ),
        }
    }

    /// Refuse shell interpretation unless configuration allows it.
    pub fn check_shell(&self, shell: bool) -> Decision {
        if shell && !self.allow_shell {
            return Decision::denied(
                "shell interpretation is disabled ([commands].allow_shell = false)",
            );
        }
        Decision::allowed("structured argv")
    }

    /// Human-readable summary for `rai config show` and startup banners.
    pub fn describe(&self) -> String {
        format!(
            "approval={} auto_approve=[{}] allow_writes={} allow_shell={} allow_destructive={}",
            self.approval.as_str(),
            self.auto_approve
                .iter()
                .map(|r| r.name())
                .collect::<Vec<_>>()
                .join(", "),
            self.allow_writes,
            self.allow_shell,
            self.allow_destructive
        )
    }
}

/// Extract `argv` from tool arguments, when present.
pub fn command_argv(args: &Value) -> Option<Vec<String>> {
    let array = args.get("argv")?.as_array()?;
    let argv: Vec<String> = array
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if argv.is_empty() {
        None
    } else {
        Some(argv)
    }
}

/// How an approval prompt is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approver {
    /// Non-interactive: refuse anything that needs approval.
    DenyAll,
    /// `--yes`: allow anything that needs approval. Explicit configuration
    /// denials (deny list, `allow_writes = false`) are still enforced by
    /// [`PolicyEngine`] and never reach an approver.
    AllowAll,
    /// Ask on the terminal.
    Interactive,
}

impl Approver {
    /// Choose an approver from CLI/environment state.
    pub fn standard(assume_yes: bool, interactive: bool) -> Self {
        if assume_yes {
            Self::AllowAll
        } else if interactive {
            Self::Interactive
        } else {
            Self::DenyAll
        }
    }

    pub fn is_interactive(self) -> bool {
        matches!(self, Self::Interactive)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::DenyAll => "deny-all",
            Self::AllowAll => "allow-all",
            Self::Interactive => "interactive",
        }
    }

    /// Resolve an approval request.
    pub fn decide(
        self,
        tool: &ToolDefinition,
        args_preview: &str,
        reason: &str,
    ) -> ApprovalOutcome {
        match self {
            Self::AllowAll => ApprovalOutcome {
                allowed: true,
                reason: format!("approved by --yes ({reason})"),
                remember: false,
            },
            Self::DenyAll => ApprovalOutcome {
                allowed: false,
                reason: format!(
                    "not running interactively; re-run with --yes or add `{}` to [policy].auto_approve ({reason})",
                    tool.risk.name()
                ),
                remember: false,
            },
            Self::Interactive => prompt(tool, args_preview, reason),
        }
    }
}

/// The result of an approval decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalOutcome {
    pub allowed: bool,
    pub reason: String,
    /// Whether the user chose to approve this risk class for the whole run.
    pub remember: bool,
}

/// Ask the user on the terminal. Unknown answers deny.
fn prompt(tool: &ToolDefinition, args_preview: &str, reason: &str) -> ApprovalOutcome {
    use std::io::{BufRead, IsTerminal, Write};

    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return ApprovalOutcome {
            allowed: false,
            reason: format!("stdin is not a terminal, so approval cannot be requested ({reason})"),
            remember: false,
        };
    }

    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "\napproval required: {} (risk {})",
        tool.name,
        tool.risk.name()
    );
    let _ = writeln!(err, "  {reason}");
    if !args_preview.is_empty() {
        let _ = writeln!(err, "  {args_preview}");
    }
    let _ = write!(err, "  [y]es / [n]o / [a]lways for this risk class: ");
    let _ = err.flush();

    let mut answer = String::new();
    if stdin.lock().read_line(&mut answer).is_err() {
        return ApprovalOutcome {
            allowed: false,
            reason: "could not read an answer".into(),
            remember: false,
        };
    }
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalOutcome {
            allowed: true,
            reason: "approved by user".into(),
            remember: false,
        },
        "a" | "always" => ApprovalOutcome {
            allowed: true,
            reason: format!(
                "approved by user for all {} calls this run",
                tool.risk.name()
            ),
            remember: true,
        },
        _ => ApprovalOutcome {
            allowed: false,
            reason: "denied by user".into(),
            remember: false,
        },
    }
}

/// Redacted, single-line, bounded rendering of a call for the approval prompt.
pub fn describe_call(tool: &ToolDefinition, args: &Value, redactor: &Redactor) -> String {
    let raw = serde_json::to_string(args).unwrap_or_default();
    let clean = redactor.clean(&raw);
    let mut text = crate::util::truncate_line(&clean, 300);
    if text.len() >= 300 {
        text.push_str("...");
    }
    format!("{} {}", tool.name, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::tools::RiskClass;
    use serde_json::json;

    fn engine(approval: &str) -> PolicyEngine {
        let mut config = Config::default();
        config.policy.approval = approval.to_string();
        config.policy.auto_approve = vec!["ReadOnly".into()];
        config.validate().ok();
        PolicyEngine::from_config(&config).unwrap()
    }

    fn tool(name: &str, risk: RiskClass) -> ToolDefinition {
        ToolDefinition::local(name, "test tool", json!({"type": "object"}), risk)
    }

    #[test]
    fn read_only_runs_without_approval() {
        let decision = engine("deny").evaluate(&tool("read_file", RiskClass::ReadOnly), &json!({}));
        assert!(decision.is_allowed(), "{decision:?}");
    }

    #[test]
    fn destructive_is_denied_by_default() {
        let decision = engine("auto").evaluate(&tool("rm_rf", RiskClass::Destructive), &json!({}));
        assert!(decision.is_denied());
        assert!(decision.reason().contains("allow_destructive"));
    }

    #[test]
    fn credential_sensitive_is_denied_by_default() {
        let decision = engine("auto").evaluate(
            &tool("read_secret_store", RiskClass::CredentialSensitive),
            &json!({}),
        );
        assert!(decision.is_denied());
    }

    #[test]
    fn allowlisted_command_runs_in_prompt_mode() {
        let decision = engine("prompt").evaluate(
            &tool("run_command", RiskClass::Command),
            &json!({"argv": ["cargo", "test", "--lib"]}),
        );
        assert!(decision.is_allowed(), "{decision:?}");
    }

    #[test]
    fn deny_list_beats_allow_list() {
        let mut config = Config::default();
        config.commands.auto_allow.push("git push".into());
        config.policy.approval = "auto".into();
        let engine = PolicyEngine::from_config(&config).unwrap();
        let decision = engine.evaluate(
            &tool("run_command", RiskClass::Command),
            &json!({"argv": ["git", "push", "origin", "main"]}),
        );
        assert!(decision.is_denied(), "{decision:?}");
    }

    #[test]
    fn unknown_command_needs_approval_but_auto_mode_allows() {
        let argv = json!({"argv": ["python", "scripts/deploy.py"]});
        let prompt = engine("prompt").evaluate(&tool("run_command", RiskClass::Command), &argv);
        assert!(
            matches!(prompt, Decision::NeedsApproval { .. }),
            "{prompt:?}"
        );

        let deny = engine("deny").evaluate(&tool("run_command", RiskClass::Command), &argv);
        assert!(deny.is_denied());

        let auto = engine("auto").evaluate(&tool("run_command", RiskClass::Command), &argv);
        assert!(auto.is_allowed());
    }

    #[test]
    fn workspace_write_needs_approval_in_prompt_mode() {
        let decision = engine("prompt").evaluate(
            &tool("apply_patch", RiskClass::WorkspaceWrite),
            &json!({"patch": "..."}),
        );
        assert!(matches!(decision, Decision::NeedsApproval { .. }));
    }

    #[test]
    fn writes_blocked_when_allow_writes_is_false() {
        let mut config = Config::default();
        config.workspace.allow_writes = false;
        config.policy.approval = "auto".into();
        let engine = PolicyEngine::from_config(&config).unwrap();
        let decision = engine.evaluate(&tool("apply_patch", RiskClass::WorkspaceWrite), &json!({}));
        assert!(decision.is_denied());
        assert!(decision.reason().contains("allow_writes"));
    }

    #[test]
    fn remembered_risk_class_is_approved_next_time() {
        let engine = engine("prompt");
        let def = tool("apply_patch", RiskClass::WorkspaceWrite);
        assert!(matches!(
            engine.evaluate(&def, &json!({})),
            Decision::NeedsApproval { .. }
        ));
        engine.remember(RiskClass::WorkspaceWrite);
        assert!(engine.evaluate(&def, &json!({})).is_allowed());
    }

    #[test]
    fn shell_requires_configuration() {
        let engine = engine("auto");
        assert!(engine.check_shell(true).is_denied());
        assert!(engine.check_shell(false).is_allowed());
    }

    #[test]
    fn non_interactive_approver_denies_with_a_hint() {
        let def = tool("apply_patch", RiskClass::WorkspaceWrite);
        let outcome = Approver::DenyAll.decide(&def, "", "needs approval");
        assert!(!outcome.allowed);
        assert!(outcome.reason.contains("--yes"));
    }

    #[test]
    fn assume_yes_approver_allows() {
        let def = tool("apply_patch", RiskClass::WorkspaceWrite);
        let outcome = Approver::AllowAll.decide(&def, "", "needs approval");
        assert!(outcome.allowed);
        assert!(!outcome.remember);
    }

    #[test]
    fn approval_preview_is_redacted() {
        let redactor = Redactor::empty().with_secret("sk-live-0123456789abcdef");
        let def = tool("run_command", RiskClass::Command);
        let preview = describe_call(
            &def,
            &json!({"argv": ["curl", "-H", "Authorization: Bearer sk-live-0123456789abcdef"]}),
            &redactor,
        );
        assert!(!preview.contains("sk-live-0123456789abcdef"));
    }
}
