//! Shared helpers for the integration tests.
//!
//! Integration tests exercise the same code paths the CLI does: a real temp
//! workspace, the real policy engine, the real tool registry, and a scripted
//! model so runs are deterministic and offline.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use rai::agent::{Agent, AgentParts};
use rai::config::Config;
use rai::events::{Emitter, OutputMode};
use rai::index::{IndexOptions, RepoIndex};
use rai::memory::ProjectMemory;
use rai::model::local::ScriptedModel;
use rai::model::ModelClient;
use rai::policy::{Approver, PolicyEngine};
use rai::redact::Redactor;
use rai::session::{MessageRecord, SessionStore};
use rai::tools::{Mode, ToolContext};
use rai::util::Cancel;
use tempfile::TempDir;

/// A throwaway workspace with helper accessors.
pub struct Workspace {
    dir: TempDir,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            dir: TempDir::new().expect("temp dir"),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root().join(relative)
    }

    pub fn write(&self, relative: &str, body: &str) {
        let path = self.path(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, body).expect("write file");
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path(relative)).expect("read file")
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.path(relative).exists()
    }

    pub fn metadata_dir(&self) -> PathBuf {
        self.path(".rai")
    }

    /// Base configuration pointed at this workspace.
    pub fn config(&self) -> Config {
        let mut config = Config::default();
        config.workspace.root = self.root().to_path_buf();
        config.model.provider = "local".to_string();
        config.workspace.exclude = Vec::new();
        config
    }

    /// Configuration with extra policy, for example `auto_approve`.
    pub fn config_with(&self, edit: impl FnOnce(&mut Config)) -> Config {
        let mut config = self.config();
        edit(&mut config);
        config
    }

    /// Write a `.rai/config.toml` the CLI will discover.
    pub fn write_config(&self, body: &str) {
        self.write(".rai/config.toml", body);
    }

    /// Run a git command in the workspace, panicking on failure.
    pub fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.root())
            .args(args)
            .env("GIT_AUTHOR_NAME", "rai tests")
            .env("GIT_AUTHOR_EMAIL", "tests@example.invalid")
            .env("GIT_COMMITTER_NAME", "rai tests")
            .env("GIT_COMMITTER_EMAIL", "tests@example.invalid")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Initialise a repository with one commit.
    pub fn git_init(&self) {
        self.git(&["init", "-q", "-b", "main"]);
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", "initial"]);
    }

    /// Session messages recorded for `id`.
    pub fn session_messages(&self, id: &str) -> Vec<MessageRecord> {
        SessionStore::new(&self.metadata_dir())
            .messages(id)
            .expect("session messages")
    }
}

/// A deterministic, offline provider that replays a fixed script.
pub fn scripted(json: &str) -> Arc<dyn ModelClient> {
    Arc::new(
        ScriptedModel::from_json(json, Path::new("integration-script.json"))
            .expect("script parses"),
    )
}

/// Build the repository index for a workspace.
pub async fn index_for(workspace: &Workspace) -> Arc<RepoIndex> {
    let index = RepoIndex::build_async(
        workspace.root().to_path_buf(),
        IndexOptions {
            excludes: Vec::new(),
            max_files: 500,
            max_scan_bytes: 256 * 1024,
        },
    )
    .await
    .expect("index builds");
    Arc::new(index)
}

/// A quiet emitter that still records to the workspace log.
pub fn quiet_emitter(workspace: &Workspace) -> Arc<Emitter> {
    Arc::new(
        Emitter::new(OutputMode::Json, true, "test")
            .with_log_file(&workspace.metadata_dir().join("logs").join("test.jsonl")),
    )
}

/// A tool context in the given mode.
pub async fn context(workspace: &Workspace, config: &Config, mode: Mode) -> ToolContext {
    ToolContext {
        root: workspace.root().to_path_buf(),
        config: Arc::new(config.clone()),
        policy: Arc::new(PolicyEngine::from_config(config).expect("policy")),
        index: index_for(workspace).await,
        emitter: quiet_emitter(workspace),
        redactor: Arc::new(Redactor::from_env()),
        cancel: Cancel::new(),
        session: "test".to_string(),
        mode,
        dry_run: false,
        commands: Arc::new(Mutex::new(Vec::new())),
        patches: Arc::new(Mutex::new(Vec::new())),
    }
}

/// An agent wired to a scripted client.
///
/// `approver` decides what happens when policy requires approval; tests use
/// `DenyAll` to prove non-interactive refusal and `AllowAll` to proceed.
pub async fn agent(
    workspace: &Workspace,
    _mode: Mode,
    client: Arc<dyn ModelClient>,
    config: &Config,
    approver: Approver,
) -> (Agent, Cancel) {
    let cancel = Cancel::new();
    let agent = Agent::new(AgentParts {
        client,
        config: Arc::new(config.clone()),
        emitter: quiet_emitter(workspace),
        redactor: Arc::new(Redactor::from_env()),
        index: index_for(workspace).await,
        policy: Arc::new(PolicyEngine::from_config(config).expect("policy")),
        approver,
        cancel: cancel.clone(),
        memory: ProjectMemory::load(&workspace.metadata_dir().join("memories.md")),
    })
    .expect("agent builds");
    (agent, cancel)
}

/// Path of the compiled `rai` binary.
pub fn rai_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rai"))
}

/// Run the CLI inside `workspace` and capture its output.
pub fn cli(workspace: &Workspace, args: &[&str]) -> std::process::Output {
    Command::new(rai_binary())
        .current_dir(workspace.root())
        .args(args)
        .output()
        .expect("rai runs")
}

/// Convenience: stdout as text.
pub fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Convenience: stderr as text.
pub fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A minimal, valid Cargo package.
pub fn write_cargo_package(workspace: &Workspace, source: &str) {
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    workspace.write("src/lib.rs", source);
}
