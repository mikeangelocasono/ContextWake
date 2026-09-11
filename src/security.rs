use std::collections::BTreeMap;
use std::path::{Component, Path};

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::error::{ContextWakeError, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RedactionReport {
    pub text: String,
    pub counts: BTreeMap<String, u32>,
}

impl RedactionReport {
    pub fn redacted(&self) -> bool {
        self.counts.values().any(|count| *count > 0)
    }
}

pub fn sanitize_terminal(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut previous_escape = false;
                    for next in chars.by_ref() {
                        if next == '\u{7}' || (previous_escape && next == '\\') {
                            break;
                        }
                        previous_escape = next == '\u{1b}';
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        if matches!(ch, '\n' | '\r' | '\t') || (!ch.is_control() && ch != '\u{7f}') {
            output.push(ch);
        }
    }
    output
}

pub fn validate_local_name(value: &str) -> Result<String> {
    let sanitized = sanitize_terminal(value).trim().to_string();
    if sanitized.is_empty() || sanitized.chars().count() > 64 {
        return Err(ContextWakeError::InvalidData(
            "names must contain between 1 and 64 visible characters".into(),
        ));
    }
    if sanitized.contains(['/', '\\']) || sanitized == "." || sanitized == ".." {
        return Err(ContextWakeError::InvalidData(
            "names cannot contain path separators".into(),
        ));
    }
    Ok(sanitized)
}

pub fn profile_slug(value: &str) -> Result<String> {
    let name = validate_local_name(value)?;
    let slug = name
        .chars()
        .flat_map(char::to_lowercase)
        .map(|ch| if ch.is_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        return Err(ContextWakeError::InvalidData(
            "profile name must contain at least one letter or number".into(),
        ));
    }
    Ok(slug)
}

pub fn validate_external_reference(value: &str) -> Result<String> {
    let value = sanitize_terminal(value).trim().to_string();
    if value.is_empty()
        || value.len() > 160
        || value.starts_with('-')
        || value.contains(['\n', '\r', '\t'])
    {
        return Err(ContextWakeError::InvalidData(
            "external references must be 1-160 single-line characters and cannot begin with '-'"
                .into(),
        ));
    }
    Ok(value)
}

/// Produces a bounded, redacted label for provider- or repository-controlled
/// display metadata. Empty labels are omitted instead of persisted.
pub fn sanitize_untrusted_label(value: &str, max_characters: usize) -> Option<String> {
    let sanitized = sanitize_terminal(value);
    let redacted = SecretScanner::new().redact(&sanitized).text;
    let label = redacted
        .trim()
        .chars()
        .take(max_characters)
        .collect::<String>();
    (!label.is_empty()).then_some(label)
}

pub fn ensure_relative_payload_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ContextWakeError::UnsafePath(path.display().to_string()));
    }
    Ok(())
}

pub fn path_fingerprint(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(path.to_string_lossy().as_bytes());
    hex::encode(digest.finalize())
}

pub fn sha256_bytes(content: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(content);
    hex::encode(digest.finalize())
}

pub struct SecretScanner {
    rules: Vec<(&'static str, Regex)>,
}

impl SecretScanner {
    pub fn new() -> Self {
        let patterns = [
            ("openai_api_key", r"\bsk-[A-Za-z0-9_-]{20,}\b"),
            ("github_token", r"\bgh[pousr]_[A-Za-z0-9]{20,}\b"),
            ("bearer_token", r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}"),
            (
                "assigned_secret",
                r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|password|secret)\b\s*[:=]\s*["']?[A-Za-z0-9._~+/=-]{8,}["']?"#,
            ),
            (
                "private_key",
                r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
            ),
        ];
        let rules = patterns
            .into_iter()
            .map(|(name, pattern)| (name, Regex::new(pattern).expect("valid secret rule")))
            .collect();
        Self { rules }
    }

    pub fn redact(&self, input: &str) -> RedactionReport {
        let mut text = sanitize_terminal(input);
        let mut counts = BTreeMap::new();
        for (name, rule) in &self.rules {
            let count = u32::try_from(rule.find_iter(&text).count()).unwrap_or(u32::MAX);
            if count > 0 {
                text = rule.replace_all(&text, "[REDACTED]").into_owned();
                counts.insert((*name).to_string(), count);
            }
        }
        RedactionReport { text, counts }
    }
}

impl Default for SecretScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_osc_and_control_bytes() {
        let malicious = "safe\x1b[31mred\x1b[0m\x1b]2;title\x07\0done";
        assert_eq!(sanitize_terminal(malicious), "safereddone");
    }

    #[test]
    fn redacts_seeded_secret_corpus() {
        let source =
            "sk-abcdefghijklmnopqrstuvwxyz token=super-secret-value ghp_12345678901234567890";
        let report = SecretScanner::new().redact(source);
        assert!(report.redacted());
        assert!(!report.text.contains("sk-"));
        assert!(!report.text.contains("super-secret-value"));
        assert!(!report.text.contains("ghp_"));
    }

    #[test]
    fn rejects_traversal_payload_paths() {
        assert!(ensure_relative_payload_path(Path::new("../../auth.json")).is_err());
        assert!(ensure_relative_payload_path(Path::new("context.md")).is_ok());
    }

    #[test]
    fn external_references_are_single_line_and_not_option_like() {
        assert_eq!(
            validate_external_reference("anthropic/claude-sonnet").unwrap(),
            "anthropic/claude-sonnet"
        );
        assert!(validate_external_reference("--dangerous").is_err());
        assert!(validate_external_reference("session\nsecond-line").is_err());
        assert!(validate_external_reference("\u{1b}[31m").is_err());
    }

    #[test]
    fn untrusted_labels_are_bounded_terminal_safe_and_redacted() {
        let label = sanitize_untrusted_label(
            "\u{1b}[31mSession sk-abcdefghijklmnopqrstuvwxyz with a long suffix",
            30,
        )
        .expect("label");
        assert!(!label.contains('\u{1b}'));
        assert!(!label.contains("sk-"));
        assert!(label.contains("[REDACTED]"));
        assert!(label.chars().count() <= 30);
        assert_eq!(sanitize_untrusted_label("\u{1b}[31m", 30), None);
    }
}
