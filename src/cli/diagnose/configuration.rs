//! A bounded, read-only projection of the existing source parser.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use rmux_core::command_parser::{
    CommandArgument, CommandParser, ParsedCommands, SOURCE_FILE_MAX_COMMAND_BYTES,
};
use rmux_server::config_paths::{
    ConfigLoadOutcome, ConfigPathExpansionError, ConfigPathMatchBudget, ResolvedConfigPaths,
};

use super::{redact_path, render_feature_list};

const SCOPE: &str = "static commands; runtime expansion and includes are not evaluated";
const TRUST: &str = "loading configuration may execute commands; diagnose never executes them";
const MAX_TOTAL_BYTES: usize = 8 * SOURCE_FILE_MAX_COMMAND_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Compatibility {
    files: Vec<FileDiagnostic>,
    pub(super) divergences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileDiagnostic {
    path: String,
    status: FileStatus,
    commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileStatus {
    StaticOnly,
    Missing,
    Unreadable,
    NotRegular,
    TooLarge,
    BudgetExhausted,
    InvalidUtf8,
    ParseError,
    InvalidPattern,
    TooManyMatches,
}

impl FileStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::StaticOnly => "static-only",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::NotRegular => "not-regular",
            Self::TooLarge => "too-large",
            Self::BudgetExhausted => "budget-exhausted",
            Self::InvalidUtf8 => "invalid-utf8",
            Self::ParseError => "parse-error",
            Self::InvalidPattern => "invalid-pattern",
            Self::TooManyMatches => "too-many-matches",
        }
    }
}

pub(super) fn collect(paths: &ResolvedConfigPaths) -> Compatibility {
    let mut budget = MAX_TOTAL_BYTES;
    let mut matches = ConfigPathMatchBudget::default();
    let mut files = inspect_paths(&paths.primary, &mut budget, &mut matches);
    // Match startup's fallback condition: no primary file and no read error.
    if files.iter().all(|file| file.status == FileStatus::Missing)
        && paths.should_load_fallback(ConfigLoadOutcome::Empty)
    {
        files.extend(inspect_paths(&paths.fallback, &mut budget, &mut matches));
    }
    Compatibility {
        files,
        divergences: divergence_ids(),
    }
}

// Only the embedded, reviewed registry supplies IDs. Imported configuration
// cannot replace it. This is the product inventory, not an applicability
// claim about commands we deliberately do not execute or expand.
const DIVERGENCE_REGISTRY: &str =
    include_str!("../../../tests/reference/tmux_compat/divergences.toml");

fn divergence_ids() -> Vec<String> {
    // Parsing the 23 KB registry with the toml crate was a measurable share
    // of every `diagnose` run. The registry is a compile-time constant, so a
    // scan of its top-level `[[entry]]` keys is enough; a unit test pins it
    // to the TOML parse.
    let mut ids = Vec::new();
    let mut entry: Option<(Option<&str>, Option<&str>)> = None;
    for line in DIVERGENCE_REGISTRY.lines().chain(std::iter::once("[end]")) {
        let line = line.trim();
        if line.starts_with('[') {
            if let Some((Some(id), Some("bounded" | "deferred"))) = entry.take() {
                ids.push(id.to_owned());
            }
            if line == "[[entry]]" {
                entry = Some((None, None));
            }
            continue;
        }
        let (Some(fields), Some((key, value))) = (entry.as_mut(), line.split_once('=')) else {
            continue;
        };
        let value = value
            .trim()
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'));
        match key.trim() {
            "id" => fields.0 = value,
            "inventory" => fields.1 = value,
            _ => {}
        }
    }
    ids
}

fn inspect_paths(
    paths: &[PathBuf],
    budget: &mut usize,
    matches: &mut ConfigPathMatchBudget,
) -> Vec<FileDiagnostic> {
    let mut files = Vec::new();
    for path in paths {
        if matches.is_exhausted() {
            break;
        }
        files.extend(inspect_selection(path, budget, matches));
    }
    files
}

fn inspect_selection(
    path: &Path,
    budget: &mut usize,
    matches: &mut ConfigPathMatchBudget,
) -> Vec<FileDiagnostic> {
    match matches.expand(path) {
        Ok(paths) => paths.iter().map(|path| inspect(path, budget)).collect(),
        Err(error) => vec![FileDiagnostic {
            path: redact_path(path),
            status: match error {
                ConfigPathExpansionError::InvalidPattern => FileStatus::InvalidPattern,
                ConfigPathExpansionError::Unreadable => FileStatus::Unreadable,
                ConfigPathExpansionError::TooManyMatches => FileStatus::TooManyMatches,
            },
            commands: Vec::new(),
        }],
    }
}

fn inspect(path: &Path, budget: &mut usize) -> FileDiagnostic {
    let mut diagnostic = FileDiagnostic {
        path: redact_path(path),
        status: FileStatus::StaticOnly,
        commands: Vec::new(),
    };
    let contents = match read_regular_file(path, budget) {
        Ok(contents) => contents,
        Err(status) => {
            diagnostic.status = status;
            return diagnostic;
        }
    };
    // No process environment, aliases from a daemon, file inclusion, execution,
    // or argument values cross this boundary. Incomplete analysis is explicit.
    let parser = CommandParser::new().with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
    match parser.parse_source_file(&contents) {
        Ok(parsed) => {
            let mut commands = BTreeSet::new();
            collect_commands(&parsed, &mut commands);
            diagnostic.commands = commands.into_iter().collect();
        }
        // Parser errors can contain configuration values; never print them.
        Err(_) => diagnostic.status = FileStatus::ParseError,
    }
    diagnostic
}

fn read_regular_file(path: &Path, budget: &mut usize) -> Result<String, FileStatus> {
    let metadata = fs::metadata(path).map_err(read_status)?;
    if !metadata.is_file() {
        return Err(FileStatus::NotRegular);
    }
    if metadata.len() > SOURCE_FILE_MAX_COMMAND_BYTES as u64 {
        return Err(FileStatus::TooLarge);
    }
    if *budget == 0 {
        return Err(FileStatus::BudgetExhausted);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A file replaced by a FIFO/device after metadata must not block.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(path).map_err(read_status)?;
    if !file.metadata().map_err(read_status)?.is_file() {
        return Err(FileStatus::NotRegular);
    }
    let limit = (*budget).min(SOURCE_FILE_MAX_COMMAND_BYTES);
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(read_status)?;
    *budget = budget.saturating_sub(bytes.len());
    if bytes.len() > limit {
        return Err(FileStatus::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| FileStatus::InvalidUtf8)
}

fn read_status(error: std::io::Error) -> FileStatus {
    if error.kind() == std::io::ErrorKind::NotFound {
        FileStatus::Missing
    } else {
        FileStatus::Unreadable
    }
}

fn collect_commands(parsed: &ParsedCommands, commands: &mut BTreeSet<String>) {
    for command in parsed.commands() {
        // parse_source_file validated these against the shared command table.
        commands.insert(command.name().to_owned());
        for argument in command.arguments() {
            if let CommandArgument::Commands(nested) = argument {
                collect_commands(nested, commands);
            }
        }
    }
}

impl Compatibility {
    pub(super) fn render_human(&self) -> String {
        let mut output = format!("  scope: {SCOPE}\n  trust: {TRUST}\n");
        for file in &self.files {
            output.push_str(&format!(
                "  - {}: {}\n    commands: {}\n",
                file.path,
                file.status.as_str(),
                render_feature_list(&file.commands)
            ));
        }
        output
    }

    pub(super) fn render_json(&self, registry: &str) -> String {
        let files: Vec<_> = self
            .files
            .iter()
            .map(|file| {
                serde_json::json!({
                    "path": file.path, "status": file.status.as_str(), "commands": file.commands,
                })
            })
            .collect();
        serde_json::json!({
            "registry": registry, "scope": SCOPE, "trust": TRUST, "configurations": files,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{divergence_ids, DIVERGENCE_REGISTRY};

    #[test]
    fn divergence_scan_matches_the_parsed_registry() {
        let registry: toml::Table = DIVERGENCE_REGISTRY
            .parse()
            .expect("embedded compatibility registry");
        let parsed: Vec<String> = registry["entry"]
            .as_array()
            .expect("registry entries")
            .iter()
            .filter(|entry| matches!(entry["inventory"].as_str(), Some("bounded" | "deferred")))
            .map(|entry| entry["id"].as_str().expect("registry ID").to_owned())
            .collect();
        assert!(!parsed.is_empty(), "registry lists active divergences");
        assert_eq!(divergence_ids(), parsed);
    }
}
