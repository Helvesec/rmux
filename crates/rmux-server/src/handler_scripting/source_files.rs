use std::io;
use std::path::{Path, PathBuf};

use rmux_core::command_parser::{CommandParseError, ParsedCommands};
use rmux_proto::{PaneTarget, RmuxError, SourceFileRequest};

use super::aggregate_rmux_errors;

#[derive(Debug, Default)]
pub(super) struct LoadedSourceFile {
    pub(super) commands: Vec<SourcedParsedCommands>,
    pub(super) stdout: Vec<u8>,
    errors: Vec<RmuxError>,
}

impl LoadedSourceFile {
    pub(super) fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub(super) fn push_error(&mut self, error: RmuxError) {
        self.errors.push(error);
    }

    pub(super) fn take_error(&mut self) -> Option<RmuxError> {
        aggregate_rmux_errors(std::mem::take(&mut self.errors))
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

#[derive(Debug, Clone)]
pub(super) struct ParsedSourceFileCommand {
    pub(super) paths: Vec<String>,
    pub(super) quiet: bool,
    pub(super) parse_only: bool,
    pub(super) verbose: bool,
    pub(super) expand_paths: bool,
    pub(super) target: Option<PaneTarget>,
    pub(super) caller_cwd: Option<PathBuf>,
    pub(super) stdin: Option<String>,
    pub(super) current_file: Option<String>,
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
            caller_cwd: request.caller_cwd,
            stdin: request.stdin,
            current_file: None,
        }
    }
}

pub(super) fn default_config_paths() -> Vec<String> {
    let mut paths = Vec::new();
    let mut push_unique = |path: String| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    push_unique("/etc/rmux.conf".to_owned());
    if let Some(home) = nonempty_env("HOME") {
        push_unique(format!("{home}/.rmux.conf"));
    }
    if let Some(xdg_config_home) = nonempty_env("XDG_CONFIG_HOME") {
        push_unique(format!("{xdg_config_home}/rmux/rmux.conf"));
    }
    if let Some(home) = nonempty_env("HOME") {
        push_unique(format!("{home}/.config/rmux/rmux.conf"));
    }

    paths
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub(super) fn source_inputs_for_path(
    path: &str,
    cwd: Option<&Path>,
    quiet: bool,
    stdin: Option<&str>,
) -> Result<Vec<SourceInput>, RmuxError> {
    if path == "-" {
        let Some(stdin) = stdin else {
            return Err(RmuxError::Server(
                "source-file - requires client stdin".to_owned(),
            ));
        };
        return Ok(vec![SourceInput {
            current_file: "-".to_owned(),
            contents: stdin.to_owned(),
        }]);
    }

    let pattern = glob_pattern_for_source_path(path, cwd);
    let entries = glob::glob(&pattern)
        .map_err(|error| RmuxError::Server(format!("invalid source-file glob '{path}': {error}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RmuxError::Server(format!("source-file glob failed: {error}")))?;

    if entries.is_empty() {
        if quiet {
            return Ok(Vec::new());
        }
        return Err(no_such_source_file(path));
    }

    let mut inputs = Vec::new();
    for entry in entries {
        match std::fs::read_to_string(&entry) {
            Ok(contents) => inputs.push(SourceInput {
                current_file: entry.to_string_lossy().into_owned(),
                contents,
            }),
            Err(error) if quiet && error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(RmuxError::Server(format!(
                    "{}: {error}",
                    entry.to_string_lossy()
                )));
            }
        }
    }

    Ok(inputs)
}

fn glob_pattern_for_source_path(path: &str, cwd: Option<&Path>) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_string_lossy().into_owned();
    }

    match cwd {
        Some(cwd) => format!(
            "{}/{}",
            glob::Pattern::escape(&cwd.to_string_lossy()),
            path.to_string_lossy()
        ),
        None => path.to_string_lossy().into_owned(),
    }
}

fn no_such_source_file(path: &str) -> RmuxError {
    RmuxError::Message(format!("{path}: No such file or directory"))
}

pub(super) fn source_parse_error(input: &SourceInput, error: CommandParseError) -> RmuxError {
    RmuxError::Server(format!("{}: {}", input.current_file, error.message()))
}
