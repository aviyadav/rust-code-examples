//! `rai` - a local AI coding assistant whose product is its governance layer.
//!
//! The crate is organised as a pipeline:
//!
//! - [`config`] holds the explicit, reviewable trust model.
//! - [`index`] keeps repository context cheap (paths, languages, symbols, git).
//! - [`tools`] is the tool registry with risk classes and schemas.
//! - [`policy`] decides which tool calls may run, and which need approval.
//! - [`patch`] is the only write path into the workspace.
//! - [`sandbox`] runs commands with timeouts, caps, filtering, and redaction.
//! - [`model`] adapts providers behind one internal interface.
//! - [`agent`] runs the loop and enforces budgets.
//! - [`mcp`] both consumes and exposes MCP tools behind the same policy.

pub mod agent;
pub mod cli;
pub mod commands;
pub mod config;
pub mod error;
pub mod events;
pub mod git;
pub mod index;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod patch;
pub mod policy;
pub mod redact;
pub mod render;
pub mod sandbox;
pub mod session;
pub mod tools;
pub mod util;

pub use error::{RaiError, Result};

/// Version string reported by the CLI, the MCP client, and MCP server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// MCP protocol revision this build speaks.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Marker directory name for project metadata.
pub const METADATA_DIR: &str = ".rai";
