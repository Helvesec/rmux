//! Preserve every declared divergence field without turning evidence into verdicts.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

use super::Error;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ledger {
    pub schema: u32,
    pub policy: Policy,
    pub entry: Vec<Entry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub oracle: String,
    pub unlisted_divergences_are_bugs: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub id: String,
    pub summary: String,
    pub inventory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_by: Option<String>,
    /// Declared provenance, not files or test symbols in the current checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub historical_evidence: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub tests: Vec<String>,
    #[serde(default)]
    pub deferred: Vec<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

pub(super) fn parse(text: &str) -> Result<Ledger, Error> {
    let ledger: Ledger = toml::from_str(text)
        .map_err(|error| Error::Invalid(format!("invalid divergences.toml: {error}")))?;
    if ledger.schema != 1
        || ledger.policy.oracle != "tmux 3.7b"
        || !ledger.policy.unlisted_divergences_are_bugs
        || ledger.entry.is_empty()
    {
        return Err(Error::Invalid(
            "unsupported or empty divergence ledger".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    for entry in &ledger.entry {
        let canonical_id = entry
            .id
            .strip_prefix("C-D")
            .and_then(|id| id.parse::<u32>().ok())
            .is_some_and(|id| id > 0 && entry.id == format!("C-D{id}"));
        if !canonical_id
            || !ids.insert(&entry.id)
            || entry.summary.trim().is_empty()
            || !matches!(entry.inventory.as_str(), "none" | "bounded" | "deferred")
            || (entry.evidence.is_empty() && entry.tests.is_empty() && entry.deferred.is_empty())
            || !valid_retirement(entry)
        {
            return Err(Error::Invalid(format!(
                "invalid or duplicate divergence entry: {}",
                entry.id
            )));
        }
    }
    Ok(ledger)
}

fn valid_retirement(entry: &Entry) -> bool {
    match &entry.retired_by {
        None => {
            entry.historical_evidence.is_empty()
                && (entry.inventory != "none" || entry.tests.is_empty())
        }
        Some(authority) => {
            let canonical_authority = authority
                .strip_prefix('D')
                .and_then(|number| number.parse::<u32>().ok())
                .is_some_and(|number| number > 0 && *authority == format!("D{number}"));
            canonical_authority
                && entry.inventory == "none"
                && !entry.tests.is_empty()
                && !entry.historical_evidence.is_empty()
                && entry.historical_evidence.iter().all(|reference| {
                    if let Some(commit) = reference.strip_prefix("commit:") {
                        commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
                    } else {
                        relative_file(reference)
                    }
                })
        }
    }
}

fn relative_file(file: &str) -> bool {
    // Use the same portable spelling on Unix and Windows. Path::components
    // normalizes interior dots and repeated separators before we can reject them.
    !file.contains(['\\', ':'])
        && !file.chars().any(char::is_control)
        && file
            .split('/')
            .all(|part| !part.trim().is_empty() && !matches!(part, "." | ".."))
}

impl Ledger {
    pub(super) fn validate_references(&self, root: &Path) -> Result<(), Error> {
        let root = std::fs::canonicalize(root).map_err(|error| super::io_error(root, error))?;
        for entry in &self.entry {
            // Historical evidence was syntax-checked during parsing. It is
            // preserved as declared provenance, never resolved or certified here.
            for reference in entry.evidence.iter().chain(&entry.tests) {
                let file = reference.split("::").next().unwrap_or("");
                if !relative_file(file) || !root.join(file).is_file() {
                    return Err(Error::Invalid(format!(
                        "invalid divergence reference {}: {reference}",
                        entry.id
                    )));
                }
                let path = root.join(file);
                let resolved =
                    std::fs::canonicalize(&path).map_err(|error| super::io_error(&path, error))?;
                if !resolved.starts_with(&root) {
                    return Err(Error::Invalid(format!(
                        "divergence reference escapes source root {}: {reference}",
                        entry.id
                    )));
                }
                if let Some((_, symbol)) = reference.split_once("::") {
                    let text = std::fs::read_to_string(&resolved)
                        .map_err(|error| super::io_error(&resolved, error))?;
                    if symbol.is_empty()
                        || !symbol
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                        || !text.contains(&format!("fn {symbol}("))
                    {
                        return Err(Error::Invalid(format!(
                            "missing divergence test {}: {reference}",
                            entry.id
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}
