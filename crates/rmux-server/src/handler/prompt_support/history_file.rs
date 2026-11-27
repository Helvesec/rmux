//! Persists the prompt history across a normal daemon stop and restart.
//!
//! `history-file` was registered as a server option with no consumer, so the
//! in-memory store always restarted empty. This module is that consumer: it
//! loads once after the initial configuration and saves once at a normal stop,
//! using the existing store, its limit and its types.
//!
//! Every I/O failure here is non-fatal and internal. A missing file, a refused
//! permission or an absent parent must not add a new error to `set-option`,
//! startup or shutdown, so the diagnostics are debug-level and the caller
//! always continues.

use std::path::{Path, PathBuf};

use rmux_proto::OptionName;
use tracing::debug;

use super::super::RequestHandler;
use super::PromptType;

/// Entries are stored oldest first, in the canonical prompt-type order.
pub(super) type HistoryEntries = Vec<(PromptType, String)>;

const DEFAULT_PROMPT_HISTORY_LIMIT: usize = 100;

/// Resolves the configured value to the path this daemon will use.
pub(super) fn resolve_path(value: &str) -> Option<PathBuf> {
    resolve_path_from(value, home_directory().as_deref())
}

/// Resolves against an explicit home so the rule is testable without mutating
/// the process environment.
///
/// Returns `None` when the option is empty, when the value is a relative path
/// that RMUX deliberately ignores, or when `~/` has no home to resolve against.
/// `None` means no I/O at all, never an error.
///
/// Unix accepts a leading `/`. Windows accepts what the platform itself calls
/// absolute - drive-qualified or UNC - and nothing else: a path relative to a
/// drive is not promoted and no POSIX path is converted.
fn resolve_path_from(value: &str, home: Option<&Path>) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    if let Some(suffix) = tilde_suffix(value) {
        return home.map(|home| append_to_home(home, suffix));
    }
    Path::new(value).is_absolute().then(|| PathBuf::from(value))
}

/// `~/` only, plus `~\` on Windows. A bare `~` or `~user` is not expanded: this
/// consumer adds no glob, variable or `~user` handling of its own.
///
/// The suffix keeps its leading separator because the home and the suffix are
/// concatenated, not joined; see [`append_to_home`].
fn tilde_suffix(value: &str) -> Option<&str> {
    let rest = value.strip_prefix('~')?;
    let separated = rest.starts_with('/') || (cfg!(windows) && rest.starts_with('\\'));
    separated.then_some(rest)
}

/// Concatenates the home and everything after the `~`, the way tmux builds the
/// path (`status.c:80`, `"%s%s"` of the home and `history_file + 1`).
///
/// `Path::join` cannot be used here: a suffix that starts with a separator is a
/// rooted path, so joining `~//history` would resolve to `/history` and joining
/// `~\history` on Windows would resolve to `<drive>\history`. Either one sends
/// the read, the creation and the truncation to a different file than the one
/// the operator configured.
fn append_to_home(home: &Path, suffix: &str) -> PathBuf {
    let mut path = home.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

/// The daemon's own home. Windows prefers `HOME` then `USERPROFILE`, matching
/// the priority `handler_scripting/parser_context.rs` already uses, and treats
/// an empty variable as absent.
fn home_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        for name in ["HOME", "USERPROFILE"] {
            if let Some(value) = std::env::var_os(name) {
                if !value.is_empty() {
                    return Some(PathBuf::from(value));
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
}

/// Renders `type:entry` lines, each terminated by LF.
pub(super) fn encode(entries: &HistoryEntries) -> String {
    let mut rendered = String::new();
    for (prompt_type, entry) in entries {
        rendered.push_str(prompt_type.label());
        rendered.push(':');
        rendered.push_str(entry);
        rendered.push('\n');
    }
    rendered
}

/// Parses `type:entry` lines.
///
/// Only the first colon separates the two fields, so an entry keeps any colon
/// it contains. A line whose prefix is absent or unknown is kept whole as a
/// `Command` entry rather than dropped, which is what makes an older or
/// foreign file readable instead of fatal. The final LF is optional.
pub(super) fn decode(contents: &str) -> HistoryEntries {
    // A file with no bytes holds no entry; a file holding just the terminator
    // holds one empty line, which tmux imports as an empty `Command`
    // (`status.c:126-138` reads a length-1 line and `status.c:1789` accepts the
    // empty entry). Testing emptiness before stripping keeps the two apart.
    if contents.is_empty() {
        return Vec::new();
    }
    let body = contents.strip_suffix('\n').unwrap_or(contents);
    body.split('\n')
        .map(|line| match line.split_once(':') {
            Some((tag, entry)) => match PromptType::parse(tag) {
                Some(prompt_type) => (prompt_type, entry.to_owned()),
                None => (PromptType::Command, line.to_owned()),
            },
            None => (PromptType::Command, line.to_owned()),
        })
        .collect()
}

impl RequestHandler {
    /// Loads the configured history once, after the initial configuration.
    ///
    /// A later `set-option` or `source-file` never reloads: the value is read
    /// here and the store keeps whatever it then accumulates.
    pub(crate) async fn load_prompt_history_file(&self) {
        self.import_prompt_history_file().await;
        // Publish only after every load outcome has finished. Cancellation
        // before this point must not let shutdown overwrite an unread file.
        self.prompt_history.lock().await.file_initialized = true;
    }

    async fn import_prompt_history_file(&self) {
        let (path, limit) = {
            let state = self.state.lock().await;
            let configured = state
                .options
                .resolve(None, OptionName::HistoryFile)
                .unwrap_or_default()
                .to_owned();
            let limit = state
                .options
                .resolve(None, OptionName::PromptHistoryLimit)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(DEFAULT_PROMPT_HISTORY_LIMIT);
            (resolve_path(&configured), limit)
        };
        let Some(path) = path else {
            return;
        };

        // Read outside every global lock: this is the daemon's own file and a
        // slow or missing filesystem must not hold the handler state.
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) => {
                debug!(
                    path = %path.display(),
                    "history-file was not read: {error}"
                );
                return;
            }
        };

        let entries = decode(&contents);
        let mut history = self.prompt_history.lock().await;
        history.import(&entries, limit);
    }

    /// Saves the history once, at a normal stop, to the current value.
    ///
    /// Changing the option while running does not reload the new file, but the
    /// stop writes to it; the previous file is left untouched. An empty value
    /// disables the write.
    pub(crate) async fn save_prompt_history_file(&self) {
        let path = {
            let state = self.state.lock().await;
            let configured = state
                .options
                .resolve(None, OptionName::HistoryFile)
                .unwrap_or_default()
                .to_owned();
            resolve_path(&configured)
        };
        let Some(path) = path else {
            return;
        };

        let entries = {
            let history = self.prompt_history.lock().await;
            if !history.file_initialized {
                return;
            }
            history.export()
        };

        // Create or truncate, never mkdir: an absent parent is one of the
        // non-fatal cases, not a directory this consumer invents.
        if let Err(error) = std::fs::write(&path, encode(&entries)) {
            debug!(
                path = %path.display(),
                "history-file was not written: {error}"
            );
        }
    }
}

#[cfg(test)]
#[path = "../../handler_prompt/history_file/tests.rs"]
mod tests;
