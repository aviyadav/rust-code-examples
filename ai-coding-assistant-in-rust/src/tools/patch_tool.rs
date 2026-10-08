//! The single write path into the workspace.
//!
//! Direct file rewriting is easy and unreliable. Patches are reviewable,
//! auditable, and rejectable, so `apply_patch` is the only tool that can change
//! files. Everything else is read-only.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    object_schema, optional_bool, require_str, Tool, ToolContext, ToolDefinition, ToolOutcome,
};
use crate::error::{RaiError, Result};
use crate::git;
use crate::patch::{self, ApplyOptions, FileAction, Patch};

/// Apply a unified diff inside the workspace.
pub struct ApplyPatch;

/// Preview of a patch for logs and approval prompts.
pub fn describe_patch(patch: &Patch) -> String {
    let files: Vec<String> = patch
        .files
        .iter()
        .filter_map(|f| f.target().map(str::to_string))
        .collect();
    let shown = files.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
    if files.len() > 8 {
        format!("{} file(s): {} ...", files.len(), shown)
    } else {
        format!("{} file(s): {}", files.len(), shown)
    }
}

#[async_trait]
impl Tool for ApplyPatch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::local(
            "apply_patch",
            "Apply a unified diff inside the workspace. This is the only way to modify files. Use `--- a/path`, `+++ b/path` headers and `@@` hunks; use `/dev/null` for created or deleted files.",
            object_schema(
                json!({
                    "patch": {
                        "type": "string",
                        "description": "Unified diff text. Fenced ```diff blocks are accepted and unwrapped."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "Validate the patch without writing anything."
                    }
                }),
                &["patch"],
            ),
            super::RiskClass::WorkspaceWrite,
        )
    }

    async fn execute(&self, args: Value, ctx: &ToolContext, _call_id: &str) -> Result<ToolOutcome> {
        if !ctx.mode.allows_write() {
            return Err(RaiError::PolicyDenied {
                tool: "apply_patch".into(),
                reason: format!("mode `{}` does not allow workspace writes", ctx.mode),
            });
        }
        if !ctx.config.workspace.allow_writes {
            return Err(RaiError::PolicyDenied {
                tool: "apply_patch".into(),
                reason: "workspace.allow_writes is false in configuration".into(),
            });
        }

        let raw = require_str(&args, "patch", "apply_patch")?;
        let text = Patch::extract(&raw).unwrap_or_else(|| raw.clone());
        if text.len() > ctx.config.budgets.max_patch_bytes {
            return Err(RaiError::Budget(format!(
                "patch is {} bytes, over the {} byte limit ([budgets].max_patch_bytes)",
                text.len(),
                ctx.config.budgets.max_patch_bytes
            )));
        }

        let parsed = Patch::parse(&text)?;
        let dry_run = optional_bool(&args, "dry_run").unwrap_or(false) || ctx.dry_run;

        // Files that already carry uncommitted changes are reported, not
        // blocked: the user asked for an edit, but they should know.
        let mut dirty: Vec<String> = Vec::new();
        for file in &parsed.files {
            if let Some(path) = file.target() {
                if git::path_is_modified(&ctx.root, path).await == Some(true) {
                    dirty.push(path.to_string());
                }
            }
        }

        let options = ApplyOptions {
            root: ctx.root.clone(),
            allow_writes: ctx.config.workspace.allow_writes,
            max_file_bytes: ctx.config.workspace.max_file_bytes,
            dry_run,
        };
        let parsed_for_task = parsed.clone();
        let applied =
            crate::util::blocking(move || patch::apply(&parsed_for_task, &options)).await?;

        if !dry_run {
            ctx.record_patch(&applied);
        }

        let added: u64 = applied.iter().map(|f| f.added).sum();
        let removed: u64 = applied.iter().map(|f| f.removed).sum();
        ctx.emit(crate::events::Event::PatchApplied {
            dry_run,
            files: applied.iter().map(|f| f.path.clone()).collect(),
            added,
            removed,
            dirty_files: dirty.clone(),
        });

        let mut text = String::new();
        text.push_str(&format!(
            "{} patch(es)\n",
            if dry_run { "validated" } else { "applied" }
        ));
        for file in &applied {
            let verb = match file.action {
                FileAction::Created => "created",
                FileAction::Modified => "modified",
                FileAction::Deleted => "deleted",
            };
            let mut line = format!(
                "{verb} {} (+{} -{}, {} bytes)",
                file.path, file.added, file.removed, file.bytes
            );
            if dry_run {
                line.push_str(" [dry run]");
            }
            if dirty.contains(&file.path) {
                line.push_str(" [file already had uncommitted changes]");
            }
            text.push_str(&line);
            text.push('\n');
        }
        if parsed.repairs > 0 {
            text.push_str(&format!(
                "note: {} hunk line(s) were repaired during parsing\n",
                parsed.repairs
            ));
        }

        let summary = format!(
            "{} {} file(s), +{added} -{removed}",
            if dry_run { "validated" } else { "applied" },
            applied.len()
        );

        Ok(ToolOutcome::new(summary, text).with_data(json!({
            "dry_run": dry_run,
            "files": applied,
            "added": added,
            "removed": removed,
            "already_modified": dirty,
        })))
    }
}

/// Number of files a patch touches.
pub fn patch_file_count(patch: &Patch) -> usize {
    patch.files.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,4 @@\n pub fn a() {}\n+pub fn b() {}\n pub fn c() {}\n";

    #[test]
    fn describe_patch_lists_targets() {
        let patch = Patch::parse(DIFF).unwrap();
        let text = describe_patch(&patch);
        assert!(text.contains("src/lib.rs"), "{text}");
        assert_eq!(patch_file_count(&patch), 1);
    }

    #[test]
    fn created_files_use_dev_null_source() {
        let diff = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,1 @@\n+hello\n";
        let patch = Patch::parse(diff).unwrap();
        assert!(patch.files[0].is_create());
        assert!(describe_patch(&patch).contains("new.txt"));
    }

    #[test]
    fn definition_is_a_workspace_write() {
        let def = ApplyPatch.definition();
        assert_eq!(def.risk, super::super::RiskClass::WorkspaceWrite);
        assert!(!def.retry_safe, "re-applying a patch is not safe");
    }
}
