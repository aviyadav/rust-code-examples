//! Secret redaction.
//!
//! Credential-sensitive values must never reach the model, the terminal, or the
//! on-disk transcript. Redaction runs on command output and on every log line.

use std::collections::BTreeSet;

use regex::Regex;
use serde::{Deserialize, Serialize};

const MASK: &str = "***REDACTED***";

/// Matches environment variable names whose *values* are treated as secrets.
fn secret_env_name_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(api[_-]?key|secret|token|password|passwd|credential|private[_-]?key|auth)",
        )
        .expect("valid regex")
    })
}

/// Literal token shapes that are unambiguous credentials.
fn token_patterns() -> &'static [Regex] {
    static RE: std::sync::OnceLock<Vec<Regex>> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        [
            r"sk-[A-Za-z0-9_\-]{16,}",
            r"ghp_[A-Za-z0-9]{20,}",
            r"github_pat_[A-Za-z0-9_]{20,}",
            r"AKIA[0-9A-Z]{16}",
            r"xox[baprs]-[A-Za-z0-9\-]{10,}",
            r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("valid regex"))
        .collect()
    })
}

/// `key = value` / `key: value` assignments where the value looks like a secret.
fn assignment_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b([A-Za-z0-9_\-]*(?:api[_-]?key|secret|token|password|passwd|credential)[A-Za-z0-9_\-]*)\s*[:=]\s*["']?([A-Za-z0-9_\-\./+=]{8,})["']?"#,
        )
        .expect("valid regex")
    })
}

/// Redacts known secret values and credential-shaped strings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Redactor {
    /// Exact values harvested from the environment that must never leak.
    secrets: BTreeSet<String>,
}

impl Redactor {
    /// Build a redactor from the current process environment.
    ///
    /// Only variables whose *name* looks credential-related are harvested, and
    /// short values are ignored to avoid masking ordinary words.
    pub fn from_env() -> Self {
        let mut secrets = BTreeSet::new();
        for (key, value) in std::env::vars() {
            if !secret_env_name_re().is_match(&key) {
                continue;
            }
            let trimmed = value.trim();
            if trimmed.len() >= 8 {
                secrets.insert(trimmed.to_string());
            }
        }
        Self { secrets }
    }

    /// Build a redactor with no environmental secrets (used in tests).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Register one literal secret value.
    pub fn with_secret(mut self, secret: impl Into<String>) -> Self {
        let secret = secret.into();
        if secret.len() >= 8 {
            self.secrets.insert(secret);
        }
        self
    }

    /// Number of registered literal secrets.
    pub fn secret_count(&self) -> usize {
        self.secrets.len()
    }

    /// True when the text contains any registered literal secret.
    pub fn contains_secret(&self, text: &str) -> bool {
        self.secrets.iter().any(|s| text.contains(s.as_str()))
    }

    /// Redact the text, returning the sanitized string and the number of
    /// substitutions applied.
    ///
    /// `allow` lists substrings that must be preserved even inside an
    /// assignment (used to keep schema keys like `OPENAI_API_KEY` legible while
    /// masking their values).
    pub fn redact(&self, text: &str) -> (String, usize) {
        let mut count = 0usize;
        let mut out = text.to_string();

        // Longest first so a value that contains another is masked whole.
        for secret in self.secrets.iter().rev() {
            if out.contains(secret.as_str()) {
                count += out.matches(secret.as_str()).count();
                out = out.replace(secret.as_str(), MASK);
            }
        }

        for re in token_patterns() {
            if re.is_match(&out) {
                count += re.find_iter(&out).count();
                out = re.replace_all(&out, MASK).to_string();
            }
        }

        let re = assignment_re();
        if re.is_match(&out) {
            let hits = re.find_iter(&out).count();
            count += hits;
            out = re
                .replace_all(&out, |caps: &regex::Captures<'_>| {
                    format!("{}={}", &caps[1], MASK)
                })
                .to_string();
        }

        (out, count)
    }

    /// Redact and count in one step, returning only the string.
    pub fn clean(&self, text: &str) -> String {
        self.redact(text).0
    }
}

/// True when an environment variable name looks credential-related.
///
/// Used by the command sandbox: secret-shaped names never reach a child
/// process, even when configuration tries to allow them.
pub fn is_secret_env_name(name: &str) -> bool {
    secret_env_name_re().is_match(name)
}

/// True when text contains a credential-shaped string.
pub fn looks_like_secret(text: &str) -> bool {
    token_patterns().iter().any(|re| re.is_match(text)) || assignment_re().is_match(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_literal_secrets() {
        let r = Redactor::empty().with_secret("super-secret-value");
        let (out, n) = r.redact("token is super-secret-value here");
        assert_eq!(n, 1);
        assert!(!out.contains("super-secret-value"));
        assert!(out.contains(MASK));
    }

    #[test]
    fn masks_known_token_shapes() {
        let r = Redactor::empty();
        let (out, n) = r.redact("OPENAI=sk-abcdefghijklmnopqrstuvwxyz0123456789 done");
        assert!(n >= 1);
        assert!(!out.contains("sk-abcdefghijklmnopqrstuvwxyz0123456789"));
    }

    #[test]
    fn masks_assignment_values_but_keeps_key() {
        let r = Redactor::empty();
        let (out, _) = r.redact("DATABASE_PASSWORD=hunter2hunter2");
        assert!(out.starts_with("DATABASE_PASSWORD="));
        assert!(out.contains(MASK));
        assert!(!out.contains("hunter2hunter2"));
    }

    #[test]
    fn ignores_short_values() {
        let r = Redactor::empty().with_secret("abc");
        assert_eq!(r.secret_count(), 0);
    }

    #[test]
    fn does_not_mangle_ordinary_text() {
        let r = Redactor::empty();
        let (out, n) = r.redact("the assistant reads files and runs tests");
        assert_eq!(n, 0);
        assert_eq!(out, "the assistant reads files and runs tests");
    }
}
