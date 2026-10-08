//! CLI surface: init, config, index, memory, sessions, review.
//!
//! These run the real binary against a real workspace, so the argv parsing,
//! configuration discovery, and exit codes are all covered.

mod common;
use common::{cli, stderr, stdout, Workspace};

#[test]
fn help_lists_the_article_surface() {
    let workspace = Workspace::new();
    let output = cli(&workspace, &["--help"]);
    assert!(output.status.success());

    let text = stdout(&output);
    for command in [
        "ask", "edit", "review", "run", "mcp", "init", "config", "index",
    ] {
        assert!(text.contains(command), "missing `{command}` in:\n{text}");
    }
}

#[test]
fn version_is_reported() {
    let workspace = Workspace::new();
    let output = cli(&workspace, &["--version"]);
    assert!(output.status.success());
    assert!(stdout(&output).contains("rai"));
}

#[test]
fn init_writes_configuration_and_memory_without_clobbering() {
    let workspace = Workspace::new();
    workspace.write(
        "Cargo.toml",
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    workspace.write("src/lib.rs", "pub fn ok() {}\n");

    let output = cli(&workspace, &["init"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(workspace.exists(".rai/config.toml"));
    assert!(workspace.exists(".rai/memories.md"));

    // Project memory is seeded from the repository, not invented.
    let memory = workspace.read(".rai/memories.md");
    assert!(memory.contains("cargo test"), "memory:\n{memory}");

    // A second run must not silently overwrite the file.
    let second = cli(&workspace, &["init"]);
    assert!(!second.status.success());
    assert!(stderr(&second).contains("already exists"));

    // ...unless asked.
    let forced = cli(&workspace, &["init", "--force"]);
    assert!(forced.status.success(), "stderr: {}", stderr(&forced));
}

#[test]
fn a_generated_config_is_valid_and_checked() {
    let workspace = Workspace::new();
    assert!(cli(&workspace, &["init"]).status.success());

    let output = cli(&workspace, &["config", "--check"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("configuration is valid"));
    // The tool surface per mode is part of the trust report.
    assert!(text.contains("tools[ask]"));
    assert!(text.contains("tools[edit]"));
    assert!(text.contains("read_file"));
    assert!(text.contains("apply_patch"));
}

#[test]
fn config_template_can_be_printed_without_a_project() {
    let workspace = Workspace::new();
    let output = cli(&workspace, &["config", "--template"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("[model]"));
    assert!(text.contains("[policy]"));
    assert!(text.contains("[budgets]"));
    assert!(text.contains("[[mcp.servers]]"));
}

#[test]
fn index_reports_what_it_indexed_and_persists_it() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub struct Alpha;\npub fn beta() {}\n");
    workspace.write("README.md", "# Title\n");
    workspace.write("target/junk.rs", "pub fn junk() {}\n");

    let output = cli(&workspace, &["index", "--stats"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("files"), "{text}");
    assert!(text.contains("symbols"), "{text}");
    assert!(workspace.exists(".rai/indexes/index.json"));

    // Build directories must not be indexed.
    let index = workspace.read(".rai/indexes/index.json");
    assert!(index.contains("src/lib.rs"));
    assert!(!index.contains("target/junk.rs"));
}

#[test]
fn memory_round_trips_through_the_cli() {
    let workspace = Workspace::new();
    assert!(cli(&workspace, &["init"]).status.success());

    let added = cli(
        &workspace,
        &["memory", "add", "commands", "cargo nextest run"],
    );
    assert!(added.status.success(), "stderr: {}", stderr(&added));

    let listed = cli(&workspace, &["memory", "list"]);
    let text = stdout(&listed);
    assert!(text.contains("cargo nextest run"), "{text}");

    let forgotten = cli(&workspace, &["memory", "forget", "cargo nextest run"]);
    assert!(forgotten.status.success());
    assert!(!workspace
        .read(".rai/memories.md")
        .contains("cargo nextest run"));
}

#[test]
fn ask_runs_offline_and_records_a_session() {
    let workspace = Workspace::new();
    workspace.write(
        "src/policy.rs",
        "pub fn evaluate() -> bool { true }\nstruct Engine;\n",
    );

    let output = cli(
        &workspace,
        &[
            "ask",
            "where is the policy engine defined?",
            "--provider",
            "local",
        ],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    assert!(text.contains("src/policy.rs"), "{text}");

    let sessions = cli(&workspace, &["sessions", "list"]);
    assert!(sessions.status.success(), "stderr: {}", stderr(&sessions));
    assert!(stdout(&sessions).contains("ask"));

    let shown = cli(&workspace, &["sessions", "show", "latest"]);
    assert!(shown.status.success(), "stderr: {}", stderr(&shown));
    assert!(stdout(&shown).contains("header"));
}

#[test]
fn unknown_command_is_rejected() {
    let workspace = Workspace::new();
    let output = cli(&workspace, &["definitely-not-a-command"]);
    assert!(!output.status.success());
}

/// A staged secret in the diff must be reported with a high severity and a
/// non-zero exit code, so `rai review` can gate CI without a model.
#[test]
fn review_reports_risks_from_the_diff_without_a_model() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn ok() {}\n");
    workspace.write_config("[model]\nprovider = \"local\"\nmodel = \"local\"\n");
    workspace.git_init();

    workspace.write(
        "src/lib.rs",
        "pub fn ok() {}\nconst KEY: &str = \"sk-abcdefghijklmnopqrstuvwxyz012345\";\n",
    );

    let output = cli(&workspace, &["review"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "high-severity findings must fail the command: {}{}",
        stdout(&output),
        stderr(&output)
    );

    let text = stdout(&output);
    assert!(text.contains("possible-secret"), "{text}");
    assert!(text.contains("src/lib.rs"), "{text}");
}

#[test]
fn review_reports_nothing_dangerous_for_a_clean_tree() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn ok() {}\n");
    workspace.write_config("[model]\nprovider = \"local\"\nmodel = \"local\"\n");
    workspace.git_init();

    let output = cli(&workspace, &["review"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("no rule-based risks found"));
}

#[test]
fn json_output_is_line_delimited_json() {
    let workspace = Workspace::new();
    workspace.write("src/lib.rs", "pub fn alpha() {}\n");

    let output = cli(
        &workspace,
        &["ask", "what is alpha?", "--provider", "local", "--json"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = stdout(&output);
    let mut records = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let value: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON: {line} ({e})"));
        assert!(value.get("type").is_some(), "missing event tag: {line}");
        assert!(value.get("session").is_some(), "missing session: {line}");
        records += 1;
    }
    assert!(records >= 3, "expected several events, got {records}");
}
