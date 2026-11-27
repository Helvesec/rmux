use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rmux_core::command_parser::{CommandParseError, ParsedCommands};
use rmux_proto::{PaneTarget, RmuxError, SourceFileRequest};

use super::aggregate_rmux_errors;

#[cfg(all(test, windows))]
use crate::config_paths::is_windows_null_config_path;

const MAX_SOURCE_CONFIG_BYTES: u64 = crate::config_paths::MAX_CONFIG_FILE_BYTES;
pub(super) const MAX_SOURCE_MATCHED_FILES: usize = crate::config_paths::MAX_CONFIG_PATH_MATCHES;
pub(super) const MAX_SOURCE_AGGREGATE_BYTES: usize =
    crate::config_paths::MAX_CONFIG_AGGREGATE_BYTES;

#[derive(Debug, Default)]
pub(super) struct LoadedSourceFile {
    pub(super) commands: Vec<SourcedParsedCommands>,
    pub(super) stdout: Vec<u8>,
    errors: Vec<RmuxError>,
    read_errors: Vec<RmuxError>,
    parse_errors: bool,
    loaded_file_count: usize,
    matched_file_count: usize,
}

impl LoadedSourceFile {
    pub(super) fn outcome(&self) -> crate::config_paths::ConfigLoadOutcome {
        use crate::config_paths::ConfigLoadOutcome;
        if self.has_errors() {
            ConfigLoadOutcome::Error
        } else if self.loaded_any_file() {
            ConfigLoadOutcome::Loaded
        } else {
            ConfigLoadOutcome::Empty
        }
    }

    pub(super) fn loaded_any_file(&self) -> bool {
        self.loaded_file_count != 0
    }

    pub(super) fn has_errors(&self) -> bool {
        !self.errors.is_empty() || !self.read_errors.is_empty()
    }

    pub(super) fn record_loaded_files(&mut self, count: usize) {
        self.loaded_file_count += count;
    }

    pub(super) fn push_error(&mut self, error: RmuxError) {
        self.errors.push(error);
    }

    pub(super) fn push_read_error(&mut self, error: RmuxError) {
        self.read_errors.push(error);
    }

    pub(super) fn push_parse_error(&mut self, error: RmuxError) {
        self.parse_errors = true;
        self.push_error(error);
    }

    pub(super) fn record_matched_files(&mut self, count: usize) {
        self.matched_file_count = self.matched_file_count.saturating_add(count);
    }

    pub(super) fn selection_failed(&self) -> bool {
        self.matched_file_count == 0 && !self.read_errors.is_empty()
    }

    pub(super) fn take_read_error(&mut self) -> Option<RmuxError> {
        aggregate_rmux_errors(std::mem::take(&mut self.read_errors))
    }

    pub(super) fn take_error(&mut self) -> Option<RmuxError> {
        let mut errors = std::mem::take(&mut self.read_errors);
        errors.extend(std::mem::take(&mut self.errors));
        aggregate_rmux_errors(errors)
    }
}

#[derive(Debug)]
pub(super) struct SourcedParsedCommands {
    pub(super) commands: ParsedCommands,
    pub(super) current_file: Option<String>,
}

#[derive(Debug)]
pub(super) struct SourceInput {
    pub(super) current_file: String,
    pub(super) contents: String,
}

pub(super) struct SourcePathRead {
    pub(super) inputs: Vec<SourceInput>,
    pub(super) error: Option<RmuxError>,
    pub(super) matched_files: usize,
    // Selection keeps a rejected direct entry so queue continuation is stable;
    // only admitted direct reads (or every glob entry) consume the budget.
    pub(super) budgeted_files: usize,
    pub(super) content_bytes: usize,
}

impl SourcePathRead {
    // Reject the entire group, but retain selection and budget accounting for
    // queue disposition and subsequent operands. No partial contents may run.
    fn rejected(error: RmuxError, matched_files: usize, content_bytes: usize) -> Self {
        Self {
            inputs: Vec::new(),
            error: Some(error),
            matched_files,
            budgeted_files: matched_files,
            content_bytes,
        }
    }
}

pub(super) fn source_selection_limit_error() -> RmuxError {
    RmuxError::Server(format!(
        "source-file command exceeds {MAX_SOURCE_MATCHED_FILES} matched files or {MAX_SOURCE_AGGREGATE_BYTES} aggregate bytes"
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceSyntax {
    Rmux,
    Canonical,
    TmuxCompat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceReadPolicy {
    Strict,
    BestEffort,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedSourceFileCommand {
    pub(super) paths: Vec<String>,
    pub(super) quiet: bool,
    pub(super) parse_only: bool,
    pub(super) verbose: bool,
    pub(super) expand_paths: bool,
    pub(super) target: Option<PaneTarget>,
    pub(super) target_missing_canfail: bool,
    pub(super) caller_cwd: Option<PathBuf>,
    pub(super) stdin: Option<String>,
    pub(super) current_file: Option<String>,
    pub(super) syntax: SourceSyntax,
}

impl From<SourceFileRequest> for ParsedSourceFileCommand {
    fn from(request: SourceFileRequest) -> Self {
        Self {
            paths: request.paths,
            quiet: request.quiet,
            parse_only: request.parse_only,
            verbose: request.verbose,
            expand_paths: request.expand_paths,
            target: request.target,
            target_missing_canfail: false,
            caller_cwd: request.caller_cwd,
            stdin: request.stdin,
            current_file: None,
            syntax: SourceSyntax::Rmux,
        }
    }
}

impl ParsedSourceFileCommand {
    pub(super) fn read_policy(&self) -> SourceReadPolicy {
        match self.syntax {
            SourceSyntax::Rmux | SourceSyntax::Canonical => SourceReadPolicy::Strict,
            SourceSyntax::TmuxCompat => SourceReadPolicy::BestEffort,
        }
    }
}

pub(super) fn startup_config_paths(
    selection: &crate::ConfigFileSelection,
) -> crate::config_paths::ResolvedConfigPaths {
    use crate::config_paths::{ConfigPathEncoding, ConfigPathResolver};

    ConfigPathResolver::capture().resolve(selection, ConfigPathEncoding::Utf8)
}

#[cfg(test)]
pub(super) fn default_config_paths() -> Vec<String> {
    startup_config_paths(&crate::ConfigFileSelection::Default)
        .source_paths()
        .0
}

#[cfg(test)]
pub(super) fn default_tmux_fallback_paths() -> Vec<String> {
    startup_config_paths(&crate::ConfigFileSelection::Default)
        .source_paths()
        .1
}

#[cfg(test)]
fn dedupe_existing_source_paths(paths: Vec<String>) -> Vec<String> {
    crate::config_paths::dedupe_existing_paths(paths.into_iter().map(PathBuf::from).collect())
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
pub(super) fn source_inputs_for_path(
    path: &str,
    cwd: Option<&Path>,
    quiet: bool,
    stdin: Option<&str>,
    read_policy: SourceReadPolicy,
) -> Result<Vec<SourceInput>, RmuxError> {
    let read = source_inputs_for_path_with_diagnostics(path, cwd, quiet, stdin, read_policy)?;
    if let Some(error) = read.error {
        return Err(error);
    }
    Ok(read.inputs)
}

pub(super) fn source_inputs_for_path_with_diagnostics(
    path: &str,
    cwd: Option<&Path>,
    quiet: bool,
    stdin: Option<&str>,
    read_policy: SourceReadPolicy,
) -> Result<SourcePathRead, RmuxError> {
    if crate::config_paths::is_null_config_path(Path::new(path)) {
        return Ok(SourcePathRead {
            inputs: vec![SourceInput {
                current_file: path.to_owned(),
                contents: String::new(),
            }],
            error: None,
            matched_files: 1,
            budgeted_files: 1,
            content_bytes: 0,
        });
    }

    if path == "-" {
        let Some(stdin) = stdin else {
            return Err(RmuxError::Server(
                "source-file - requires client stdin".to_owned(),
            ));
        };
        if let Err(error) = reserve_source_contents(0, stdin.len(), path) {
            return Ok(SourcePathRead::rejected(error, 1, stdin.len()));
        }
        return Ok(SourcePathRead {
            inputs: vec![SourceInput {
                current_file: "-".to_owned(),
                contents: stdin.to_owned(),
            }],
            error: None,
            matched_files: 1,
            budgeted_files: 1,
            content_bytes: stdin.len(),
        });
    }

    let pattern = glob_pattern_for_source_path(path, cwd);
    let entries = glob::glob(&pattern).map_err(|error| {
        RmuxError::Server(format!("invalid source-file glob '{path}': {error}"))
    })?;

    let mut read = read_source_entries(
        path,
        entries.map(|entry| {
            entry.map_err(|error| RmuxError::Server(format!("source-file glob failed: {error}")))
        }),
        quiet,
        read_policy,
    );
    if !path.chars().any(|ch| matches!(ch, '*' | '?' | '[' | ']')) && read.error.is_some() {
        // A failed direct read, including decoded-byte admission, costs zero.
        // Retain its selection and diagnostic for the source queue's disposition.
        read.budgeted_files = 0;
        read.content_bytes = 0;
    }
    Ok(read)
}

fn read_source_entries(
    path: &str,
    entries: impl IntoIterator<Item = Result<PathBuf, RmuxError>>,
    quiet: bool,
    read_policy: SourceReadPolicy,
) -> SourcePathRead {
    let mut inputs = Vec::new();
    let mut errors = Vec::new();
    let mut matched_files = 0_usize;
    let mut aggregate_bytes = 0_usize;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                return SourcePathRead::rejected(error, matched_files, aggregate_bytes);
            }
        };
        matched_files = matched_files.saturating_add(1);
        if matched_files > MAX_SOURCE_MATCHED_FILES {
            return SourcePathRead::rejected(
                RmuxError::Server(format!(
                    "source-file glob '{path}' matched too many files (maximum {MAX_SOURCE_MATCHED_FILES})"
                )),
                matched_files,
                aggregate_bytes,
            );
        }
        match read_source_entry(&entry, read_policy) {
            Ok(contents) => {
                aggregate_bytes =
                    match reserve_source_contents(aggregate_bytes, contents.len(), path) {
                        Ok(total) => total,
                        Err(error) => {
                            return SourcePathRead::rejected(
                                error,
                                matched_files,
                                aggregate_bytes.saturating_add(contents.len()),
                            );
                        }
                    };
                inputs.push(SourceInput {
                    current_file: source_entry_display_path(&entry),
                    contents,
                });
            }
            Err(error) if quiet && error.kind() == io::ErrorKind::NotFound => {}
            Err(_) if read_policy == SourceReadPolicy::BestEffort => {}
            Err(error) => errors.push(source_entry_read_error(&entry, &error)),
        }
    }

    if matched_files == 0 {
        if quiet {
            return SourcePathRead {
                inputs: Vec::new(),
                error: None,
                matched_files: 0,
                budgeted_files: 0,
                content_bytes: 0,
            };
        }
        // Keep selection separate from reading: even an unreadable matched
        // file lets the caller queue continue, unlike an entirely empty selection.
        errors.push(no_such_source_file(path));
    }

    SourcePathRead {
        inputs,
        error: aggregate_rmux_errors(errors),
        matched_files,
        budgeted_files: matched_files,
        content_bytes: aggregate_bytes,
    }
}

fn reserve_source_contents(
    current: usize,
    additional: usize,
    path: &str,
) -> Result<usize, RmuxError> {
    crate::config_paths::reserve_config_contents(current, additional).ok_or_else(|| {
        RmuxError::Server(format!(
            "source-file input '{path}' exceeds {MAX_SOURCE_AGGREGATE_BYTES} aggregate bytes"
        ))
    })
}

fn source_io_error_message(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        io::ErrorKind::IsADirectory => "Input/output error".to_owned(),
        _ => error.to_string(),
    }
}

fn source_entry_read_error(entry: &Path, error: &io::Error) -> RmuxError {
    RmuxError::Server(format!(
        "{}: {}",
        source_io_error_message(error),
        source_entry_display_path(entry)
    ))
}

fn read_source_entry(entry: &Path, read_policy: SourceReadPolicy) -> io::Result<String> {
    match read_policy {
        SourceReadPolicy::Strict => crate::config_paths::read_strict_config_path(entry),
        SourceReadPolicy::BestEffort => read_tmux_compat_source_entry(entry),
    }
}

fn read_tmux_compat_source_entry(entry: &Path) -> io::Result<String> {
    let preopen_metadata = fs::metadata(entry)?;
    validate_tmux_compat_regular_metadata(&preopen_metadata)?;

    let file = open_tmux_compat_regular_file(entry)?;
    let metadata = file.metadata()?;
    validate_tmux_compat_regular_metadata(&metadata)?;

    let mut contents = Vec::new();
    let mut reader = file.take(MAX_SOURCE_CONFIG_BYTES + 1);
    reader.read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_SOURCE_CONFIG_BYTES {
        return Err(crate::config_paths::oversized_source_config_error());
    }
    Ok(String::from_utf8_lossy(&contents).into_owned())
}

fn validate_tmux_compat_regular_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tmux fallback config is not a regular file",
        ));
    }
    if metadata.len() > MAX_SOURCE_CONFIG_BYTES {
        return Err(crate::config_paths::oversized_source_config_error());
    }
    Ok(())
}

#[cfg(unix)]
fn open_tmux_compat_regular_file(entry: &Path) -> io::Result<File> {
    use rustix::fs::{open, Mode, OFlags};

    let fd = open(
        entry,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    Ok(File::from(fd))
}

#[cfg(not(unix))]
fn open_tmux_compat_regular_file(entry: &Path) -> io::Result<File> {
    File::open(entry)
}

fn glob_pattern_for_source_path(path: &str, cwd: Option<&Path>) -> String {
    crate::config_paths::source_glob_pattern(Path::new(path), cwd)
}

fn source_entry_display_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().replace('/', "\\")
    }

    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
}

fn no_such_source_file(path: &str) -> RmuxError {
    RmuxError::Message(format!("No such file or directory: {path}"))
}

pub(super) fn source_parse_error_with_line_offset(
    input: &SourceInput,
    error: CommandParseError,
    line_offset: usize,
) -> RmuxError {
    if error.line() == 0 {
        return RmuxError::Server(format!("{}: {}", input.current_file, error.message()));
    }
    let line = error.line().saturating_add(line_offset);
    RmuxError::Server(format!(
        "{}:{}: {}",
        input.current_file,
        line,
        error.message()
    ))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod budget_tests;

#[cfg(test)]
mod direct_budget_tests;
