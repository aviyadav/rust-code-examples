//! Project memory: explicit, editable, inspectable.
//!
//! Project memory is not hidden long-term state. It lives in
//! `.rai/memories.md`, is injected into the system prompt, and can be read and
//! corrected by the user at any time.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::util::{now_rfc3339, truncate_line};

/// Sections that carry runnable commands.
pub const COMMAND_SECTIONS: &[&str] = &["commands", "verification"];

/// A parsed `memories.md`.
#[derive(Debug, Clone, Default)]
pub struct ProjectMemory {
    path: PathBuf,
    preamble: Vec<String>,
    sections: Vec<(String, Vec<String>)>,
}

impl ProjectMemory {
    /// Load memory from a path, returning an empty memory when absent.
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut memory = Self::parse(&text);
        memory.path = path.to_path_buf();
        memory
    }

    /// Parse memory file contents.
    pub fn parse(text: &str) -> Self {
        let mut preamble = Vec::new();
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        for line in text.lines() {
            if let Some(title) = line.strip_prefix("## ") {
                sections.push((title.trim().to_string(), Vec::new()));
                continue;
            }
            if let Some(item) = line.strip_prefix("- ") {
                if let Some(last) = sections.last_mut() {
                    last.1.push(item.trim().to_string());
                    continue;
                }
            }
            if line.trim().is_empty() {
                continue;
            }
            if sections.is_empty() {
                preamble.push(line.to_string());
            }
        }
        Self {
            path: PathBuf::new(),
            preamble,
            sections,
        }
    }

    /// Path this memory was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Items in one section.
    pub fn section(&self, name: &str) -> Vec<String> {
        self.sections
            .iter()
            .find(|(title, _)| title.eq_ignore_ascii_case(name))
            .map(|(_, items)| items.clone())
            .unwrap_or_default()
    }

    /// All section titles.
    pub fn section_names(&self) -> Vec<String> {
        self.sections.iter().map(|(t, _)| t.clone()).collect()
    }

    /// Every recorded fact, as `section: item`.
    pub fn facts(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (title, items) in &self.sections {
            for item in items {
                out.push(format!("{title}: {item}"));
            }
        }
        out
    }

    /// Append one fact to a section, creating it when needed.
    pub fn add(&mut self, section: &str, item: &str) {
        let section = section.trim();
        let item = item.trim();
        if let Some(entry) = self
            .sections
            .iter_mut()
            .find(|(title, _)| title.eq_ignore_ascii_case(section))
        {
            if !entry.1.iter().any(|existing| existing == item) {
                entry.1.push(item.to_string());
            }
            return;
        }
        self.sections
            .push((section.to_string(), vec![item.to_string()]));
    }

    /// Remove every fact containing `needle`. Returns how many were removed.
    pub fn remove_matching(&mut self, needle: &str) -> usize {
        let mut removed = 0;
        for (_, items) in self.sections.iter_mut() {
            let before = items.len();
            items.retain(|item| !item.contains(needle));
            removed += before - items.len();
        }
        self.sections.retain(|(_, items)| !items.is_empty());
        removed
    }

    /// Render back to Markdown.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.preamble {
            out.push_str(line);
            out.push('\n');
        }
        for (title, items) in &self.sections {
            out.push_str(&format!("\n## {title}\n\n"));
            for item in items {
                out.push_str(&format!("- {item}\n"));
            }
        }
        out
    }

    /// Persist to disk.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut body = format!(
            "<!-- Project memory for rai. Human editable. Injected into every prompt. -->\n<!-- last updated {} -->\n",
            now_rfc3339()
        );
        body.push_str(&self.render());
        std::fs::write(&self.path, body)?;
        Ok(())
    }

    /// A command recorded in memory, matched by leading word.
    ///
    /// `memory.command("test")` returns the first recorded item whose first
    /// token is `test` (for example `test: cargo test --workspace`).
    pub fn command(&self, prefix: &str) -> Option<String> {
        for section in COMMAND_SECTIONS {
            for item in self.section(section) {
                let value = item.rsplit_once(':').map(|(_, v)| v).unwrap_or(&item);
                let value = value.trim();
                let head = value.split_whitespace().next().unwrap_or("");
                let item_head = item.split(':').next().unwrap_or("").trim();
                if item_head.eq_ignore_ascii_case(prefix) || head.eq_ignore_ascii_case(prefix) {
                    return Some(value.to_string());
                }
            }
        }
        None
    }

    /// Seed memory from repository facts that are cheap to detect.
    pub fn seed_for(root: &Path) -> Self {
        let mut memory = Self {
            path: root.join(".rai/memories.md"),
            preamble: vec![
                "# Project memory".to_string(),
                String::new(),
                "Facts below are injected into every model prompt. Keep them short and true."
                    .to_string(),
            ],
            sections: Vec::new(),
        };

        let mut commands = Vec::new();
        if root.join("Cargo.toml").exists() {
            commands.push("build: cargo build".to_string());
            commands.push("test: cargo test".to_string());
            commands.push("quick-check: cargo check".to_string());
            commands.push("format: cargo fmt".to_string());
            commands.push("lint: cargo clippy --all-targets".to_string());
            memory.add("layout", "Rust crate/workspace; sources under `src/`.");
        }
        if root.join("package.json").exists() {
            // Infer the package manager from whichever lockfile is present.
            let test = if root.join("pnpm-lock.yaml").exists() {
                "pnpm test"
            } else if root.join("yarn.lock").exists() {
                "yarn test"
            } else {
                "npm test"
            };
            commands.push(format!("test: {test}"));
            commands.push("build: npm run build".to_string());
            memory.add("layout", "Node package; sources under `src/`.");
        }
        if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
            commands.push("test: python -m pytest".to_string());
            memory.add("layout", "Python package.");
        }
        if root.join("go.mod").exists() {
            commands.push("test: go test ./...".to_string());
            commands.push("build: go build ./...".to_string());
            memory.add("layout", "Go module.");
        }
        if root.join("Makefile").exists() {
            memory.add("layout", "Makefile present; prefer `make` targets.");
        }
        for command in commands {
            memory.add("commands", &command);
        }
        memory.add(
            "conventions",
            "Patches are the only write path; keep diffs small and reviewable.",
        );
        memory
    }

    /// Compact context block for the system prompt.
    pub fn prompt_block(&self, max_items: usize) -> String {
        let facts = self.facts();
        if facts.is_empty() {
            return String::new();
        }
        let mut out = String::from("Project memory (user editable, may be stale - verify):\n");
        for fact in facts.iter().take(max_items) {
            out.push_str(&format!("- {}\n", truncate_line(fact, 200)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_sections_and_items() {
        let memory = ProjectMemory::parse(
            "# Project memory\n\n## commands\n\n- test: cargo test\n- format: cargo fmt\n\n## layout\n\n- Rust crate\n",
        );
        assert_eq!(memory.section("commands").len(), 2);
        assert_eq!(memory.command("test").unwrap(), "cargo test");
        assert_eq!(memory.section("layout")[0], "Rust crate");
    }

    #[test]
    fn add_is_idempotent() {
        let mut memory = ProjectMemory::parse("## commands\n\n- test: cargo test\n");
        memory.add("commands", "test: cargo test");
        assert_eq!(memory.section("commands").len(), 1);
        memory.add("commands", "lint: cargo clippy");
        assert_eq!(memory.section("commands").len(), 2);
    }

    #[test]
    fn remove_matching_drops_items() {
        let mut memory =
            ProjectMemory::parse("## commands\n\n- test: cargo test\n- lint: cargo clippy\n");
        assert_eq!(memory.remove_matching("clippy"), 1);
        assert_eq!(memory.section("commands"), vec!["test: cargo test"]);
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(".rai/memories.md");
        let mut memory = ProjectMemory::load(&path);
        memory.path = path.clone();
        memory.add("commands", "test: cargo test");
        memory.save().unwrap();
        let reloaded = ProjectMemory::load(&path);
        assert_eq!(reloaded.command("test").unwrap(), "cargo test");
    }

    #[test]
    fn seeds_rust_repo_commands() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
        let memory = ProjectMemory::seed_for(dir.path());
        assert_eq!(memory.command("test").unwrap(), "cargo test");
        assert!(memory.command("format").is_some());
    }

    #[test]
    fn prompt_block_is_bounded() {
        let mut memory = ProjectMemory::default();
        for i in 0..50 {
            memory.add("commands", &format!("c{i}: echo {i}"));
        }
        let block = memory.prompt_block(5);
        assert_eq!(block.lines().count(), 6);
    }
}
