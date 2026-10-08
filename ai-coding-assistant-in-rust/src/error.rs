//! Domain errors.
//!
//! `anyhow` is used at the CLI boundary, but every internal module returns a
//! typed error so failures keep enough detail to debug the next session.

use std::path::PathBuf;

use thiserror::Error;

/// Result alias used across the crate.
pub type Result<T> = std::result::Result<T, RaiError>;

#[derive(Debug, Error)]
pub enum RaiError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("invalid config value `{key}` in {path}: {problem}")]
    ConfigKey {
        key: String,
        path: PathBuf,
        problem: String,
    },

    #[error("policy denied `{tool}`: {reason}")]
    PolicyDenied { tool: String, reason: String },

    #[error("approval denied for `{tool}`: {reason}")]
    ApprovalDenied { tool: String, reason: String },

    #[error("unknown tool `{0}`")]
    UnknownTool(String),

    #[error("invalid arguments for `{tool}`: {problem}")]
    InvalidArguments { tool: String, problem: String },

    #[error("path `{path}` is not inside the workspace root `{root}`")]
    PathEscape { path: String, root: String },

    #[error("path `{0}` must be relative to the workspace root")]
    AbsolutePath(String),

    #[error("`{path}` is not valid UTF-8 text (binary or unsupported encoding)")]
    NotText { path: String },

    #[error("`{path}` is {size} bytes, over the {limit} byte limit")]
    FileTooLarge { path: String, size: u64, limit: u64 },

    #[error("patch error: {0}")]
    Patch(String),

    #[error("command `{command}` timed out after {seconds}s")]
    CommandTimeout { command: String, seconds: u64 },

    #[error("command could not start: {command}: {problem}")]
    CommandSpawn { command: String, problem: String },

    #[error("budget exceeded: {0}")]
    Budget(String),

    #[error("run cancelled by user")]
    Cancelled,

    #[error("model error: {0}")]
    Model(String),

    #[error("mcp error: {0}")]
    Mcp(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl RaiError {
    /// Whether repeating the exact same operation is safe.
    ///
    /// Read-only failures are retry-safe; anything that may have already
    /// touched the workspace or a remote system is not.
    pub fn retry_safe(&self) -> bool {
        match self {
            RaiError::Config(_)
            | RaiError::ConfigKey { .. }
            | RaiError::UnknownTool(_)
            | RaiError::InvalidArguments { .. }
            | RaiError::AbsolutePath(_)
            | RaiError::PathEscape { .. }
            | RaiError::NotText { .. }
            | RaiError::FileTooLarge { .. }
            | RaiError::PolicyDenied { .. }
            | RaiError::ApprovalDenied { .. } => false,
            RaiError::Patch(_)
            | RaiError::Model(_)
            | RaiError::Mcp(_)
            | RaiError::CommandSpawn { .. }
            | RaiError::Io(_)
            | RaiError::Json(_) => true,
            RaiError::CommandTimeout { .. } | RaiError::Budget(_) | RaiError::Cancelled => false,
        }
    }

    /// Stable machine-readable kind, used in logs and the MCP error payload.
    pub fn kind(&self) -> &'static str {
        match self {
            RaiError::Config(_) => "config",
            RaiError::ConfigKey { .. } => "config_key",
            RaiError::PolicyDenied { .. } => "policy_denied",
            RaiError::ApprovalDenied { .. } => "approval_denied",
            RaiError::UnknownTool(_) => "unknown_tool",
            RaiError::InvalidArguments { .. } => "invalid_arguments",
            RaiError::PathEscape { .. } => "path_escape",
            RaiError::AbsolutePath(_) => "absolute_path",
            RaiError::NotText { .. } => "not_text",
            RaiError::FileTooLarge { .. } => "file_too_large",
            RaiError::Patch(_) => "patch",
            RaiError::CommandTimeout { .. } => "command_timeout",
            RaiError::CommandSpawn { .. } => "command_spawn",
            RaiError::Budget(_) => "budget",
            RaiError::Cancelled => "cancelled",
            RaiError::Model(_) => "model",
            RaiError::Mcp(_) => "mcp",
            RaiError::Io(_) => "io",
            RaiError::Json(_) => "json",
        }
    }
}
