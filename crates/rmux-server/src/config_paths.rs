//! Shared, ordered configuration discovery for startup and diagnostics.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::ConfigFileSelection;

/// Encoding supported by the consumer of default configuration paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPathEncoding {
    /// Preserve native environment bytes for diagnostics.
    Native,
    /// Retain startup's existing UTF-8 environment contract.
    ///
    /// The source command engine accepts strings. Invalid environment values
    /// must be omitted, not silently replaced with Unicode replacement bytes.
    Utf8,
}

/// A single environment snapshot used for primary and fallback discovery.
#[derive(Debug, Clone)]
pub struct ConfigPathResolver {
    #[cfg(not(windows))]
    home: Option<OsString>,
    xdg: Option<OsString>,
    #[cfg(windows)]
    userprofile: Option<OsString>,
    #[cfg(windows)]
    appdata: Option<OsString>,
    #[cfg(windows)]
    config_file: Option<OsString>,
    disable_tmux_fallback: bool,
}

/// An ordered selection; only default discovery may offer a tmux fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfigPaths {
    /// Primary candidates, including absent files and explicit repetitions.
    pub primary: Vec<PathBuf>,
    /// Compatibility candidates, used only after an error-free empty load.
    pub fallback: Vec<PathBuf>,
}

/// Whether reading the primary selection permits compatibility discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLoadOutcome {
    /// No primary file was loaded and no read or parse error occurred.
    Empty,
    /// At least one primary file was loaded.
    Loaded,
    /// A primary read or parse error prevents compatibility fallback.
    Error,
}

impl ResolvedConfigPaths {
    /// Compatibility fallback is only eligible after an error-free empty load.
    #[must_use]
    pub fn should_load_fallback(&self, outcome: ConfigLoadOutcome) -> bool {
        outcome == ConfigLoadOutcome::Empty && !self.fallback.is_empty()
    }

    /// The existing source engine's string boundary; native diagnostics do not
    /// pass through this projection.
    #[must_use]
    pub fn source_paths(&self) -> (Vec<String>, Vec<String>) {
        let strings = |paths: &[PathBuf]| {
            paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        };
        (strings(&self.primary), strings(&self.fallback))
    }
}

/// The source engine and passive inspection share the same glob match bound.
pub const MAX_CONFIG_PATH_MATCHES: usize = 256;

pub(crate) const MAX_CONFIG_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_CONFIG_AGGREGATE_BYTES: usize = 32 * 1024 * 1024;

/// A selection-wide budget. Repetitions consume matches again; an overflowing
/// group is rejected whole and ends the selection.
#[derive(Debug, Default)]
pub struct ConfigPathMatchBudget {
    matched_files: usize,
    exhausted: bool,
}

impl ConfigPathMatchBudget {
    /// Reserve a whole group's matches, preserving earlier accepted groups.
    pub fn reserve(&mut self, count: usize) -> Result<(), ConfigPathExpansionError> {
        if self.exhausted || count > MAX_CONFIG_PATH_MATCHES - self.matched_files {
            self.exhausted = true;
            return Err(ConfigPathExpansionError::TooManyMatches);
        }
        self.matched_files += count;
        Ok(())
    }

    /// Whether the selection must stop rather than inspect later arguments.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Expand and reserve one candidate in the current selection's budget.
    /// Direct paths count only after strict reading and decoded-byte admission;
    /// glob entries count even when reading an individual entry fails.
    pub fn expand(&mut self, path: &Path) -> Result<Vec<PathBuf>, ConfigPathExpansionError> {
        if self.exhausted {
            return Err(ConfigPathExpansionError::TooManyMatches);
        }
        let (paths, count) = match expand_config_path_matches(path) {
            Err(ConfigPathExpansionError::TooManyMatches) => {
                self.exhausted = true;
                return Err(ConfigPathExpansionError::TooManyMatches);
            }
            result => result?,
        };
        self.reserve(count)?;
        Ok(paths)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Bounded path expansion failures, without imported configuration values.
pub enum ConfigPathExpansionError {
    /// The path contains an invalid glob pattern.
    InvalidPattern,
    /// A directory needed to enumerate a pattern could not be read.
    Unreadable,
    /// The pattern matched more than the allowed number of files.
    TooManyMatches,
}

/// Expand an inspection candidate with the source engine's glob syntax and
/// ordering. Preserve unmatched candidates so diagnostics can report absence.
pub fn expand_config_path(path: &Path) -> Result<Vec<PathBuf>, ConfigPathExpansionError> {
    ConfigPathMatchBudget::default().expand(path)
}

fn expand_config_path_matches(
    path: &Path,
) -> Result<(Vec<PathBuf>, usize), ConfigPathExpansionError> {
    let Some(value) = path.to_str() else {
        return Ok((
            vec![path.to_path_buf()],
            usize::from(direct_source_config_path_admitted(path)),
        ));
    };
    if !value.chars().any(|ch| matches!(ch, '*' | '?' | '[' | ']')) {
        return Ok((
            vec![path.to_path_buf()],
            usize::from(direct_source_config_path_admitted(path)),
        ));
    }
    let entries = glob::glob(&source_glob_pattern(path, None))
        .map_err(|_| ConfigPathExpansionError::InvalidPattern)?;
    let mut paths = Vec::new();
    for entry in entries {
        if paths.len() == MAX_CONFIG_PATH_MATCHES {
            return Err(ConfigPathExpansionError::TooManyMatches);
        }
        paths.push(entry.map_err(|_| ConfigPathExpansionError::Unreadable)?);
    }
    let matched_files = paths.len();
    if matched_files == 0 {
        paths.push(path.to_path_buf());
    }
    Ok((paths, matched_files))
}

fn direct_source_config_path_admitted(path: &Path) -> bool {
    is_null_config_path(path)
        || read_strict_config_path(path)
            .is_ok_and(|contents| reserve_config_contents(0, contents.len()).is_some())
}

// Count decoded UTF-8 bytes, including lossy replacements, at both admission
// sites. Glob groups and stdin retain the source engine's existing accounting.
pub(crate) fn reserve_config_contents(current: usize, additional: usize) -> Option<usize> {
    current
        .checked_add(additional)
        .filter(|total| *total <= MAX_CONFIG_AGGREGATE_BYTES)
}

// Strict reading and decoding are shared with startup/source-file. Diagnostics
// retain their tighter inspection limits after this bounded eligibility check.
pub(crate) fn read_strict_config_path(entry: &Path) -> io::Result<String> {
    let metadata = fs::metadata(entry)?;
    validate_strict_source_metadata(&metadata)?;

    let file = open_strict_source_entry(entry)?;
    let metadata = file.metadata()?;
    validate_strict_source_metadata(&metadata)?;
    let mut contents = Vec::new();
    let mut reader = file.take(MAX_CONFIG_FILE_BYTES + 1);
    reader.read_to_end(&mut contents)?;
    if contents.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(oversized_source_config_error());
    }
    Ok(String::from_utf8_lossy(&contents).into_owned())
}

fn validate_strict_source_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::IsADirectory,
            "Is a directory",
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source file is not a regular file",
        ));
    }
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(oversized_source_config_error());
    }
    Ok(())
}

#[cfg(unix)]
fn open_strict_source_entry(entry: &Path) -> io::Result<File> {
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
fn open_strict_source_entry(entry: &Path) -> io::Result<File> {
    File::open(entry)
}

pub(crate) fn oversized_source_config_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "source file exceeds 16 MiB")
}

#[cfg(unix)]
pub(crate) fn is_null_config_path(path: &Path) -> bool {
    path == Path::new("/dev/null")
}

#[cfg(windows)]
pub(crate) fn is_null_config_path(path: &Path) -> bool {
    path.to_str().is_some_and(is_windows_null_config_path)
}

#[cfg(windows)]
pub(crate) fn is_windows_null_config_path(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let Some(component) = trimmed.rsplit(['\\', '/']).next() else {
        return false;
    };
    let component = component.trim_end_matches(':');
    let device = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem);
    device.eq_ignore_ascii_case("NUL")
}

pub(crate) fn source_glob_pattern(path: &Path, cwd: Option<&Path>) -> String {
    let spelling = |path: &Path| {
        #[cfg(windows)]
        {
            path.to_string_lossy().replace('\\', "/")
        }
        #[cfg(not(windows))]
        {
            path.to_string_lossy().into_owned()
        }
    };
    if path.is_absolute() {
        return spelling(path);
    }
    match cwd {
        Some(cwd) => format!(
            "{}/{}",
            glob::Pattern::escape(&spelling(cwd)),
            spelling(path)
        ),
        None => spelling(path),
    }
}

impl ConfigPathResolver {
    /// Captures each relevant environment variable once, without changing it.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            #[cfg(not(windows))]
            home: nonempty_env("HOME"),
            xdg: nonempty_env("XDG_CONFIG_HOME"),
            #[cfg(windows)]
            userprofile: nonempty_env("USERPROFILE"),
            #[cfg(windows)]
            appdata: nonempty_env("APPDATA"),
            #[cfg(windows)]
            config_file: nonempty_env("RMUX_CONFIG_FILE"),
            disable_tmux_fallback: nonempty_env("RMUX_DISABLE_TMUX_FALLBACK")
                .and_then(|value| value.into_string().ok())
                .is_some_and(|value| {
                    let value = value.trim();
                    !value.is_empty()
                        && !matches!(
                            value.to_ascii_lowercase().as_str(),
                            "0" | "false" | "no" | "off"
                        )
                }),
        }
    }

    /// Resolves a typed selection without opening or parsing configuration.
    ///
    /// Explicit `-f` paths preserve their bytes, spelling, order and repeats.
    /// Default Unix candidates retain the first spelling of each existing
    /// file identity; missing candidates remain visible for diagnostics.
    #[must_use]
    pub fn resolve(
        &self,
        selection: &ConfigFileSelection,
        encoding: ConfigPathEncoding,
    ) -> ResolvedConfigPaths {
        match selection {
            ConfigFileSelection::Disabled => ResolvedConfigPaths {
                primary: Vec::new(),
                fallback: Vec::new(),
            },
            ConfigFileSelection::Files(files) => ResolvedConfigPaths {
                primary: files.clone(),
                fallback: Vec::new(),
            },
            ConfigFileSelection::Default => {
                let primary = self.defaults("rmux", encoding);
                // Windows startup historically deduplicates only fallback
                // identities; preserve that platform-specific contract.
                #[cfg(not(windows))]
                let primary = dedupe_existing_paths(primary);
                let fallback = if self.disable_tmux_fallback {
                    Vec::new()
                } else {
                    dedupe_existing_paths(self.defaults("tmux", encoding))
                };
                ResolvedConfigPaths { primary, fallback }
            }
        }
    }

    fn defaults(&self, program: &str, encoding: ConfigPathEncoding) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut push = |path| {
            if !paths.contains(&path) {
                paths.push(path);
            }
        };
        let value = |value: &Option<OsString>| {
            value
                .as_ref()
                .filter(|value| encoding == ConfigPathEncoding::Native || value.to_str().is_some())
                .map(PathBuf::from)
        };
        let filename = format!("{program}.conf");
        #[cfg(not(windows))]
        {
            push(PathBuf::from("/etc").join(&filename));
            if let Some(home) = value(&self.home) {
                push(home.join(format!(".{filename}")));
            }
            if let Some(xdg) = value(&self.xdg) {
                push(xdg.join(program).join(&filename));
            }
            if let Some(home) = value(&self.home) {
                push(home.join(".config").join(program).join(&filename));
            }
        }
        #[cfg(windows)]
        {
            if let Some(xdg) = value(&self.xdg) {
                push(xdg.join(program).join(&filename));
            }
            if let Some(home) = value(&self.userprofile) {
                push(home.join(format!(".{filename}")));
            }
            if let Some(appdata) = value(&self.appdata) {
                push(appdata.join(program).join(&filename));
            }
            if program == "rmux" {
                if let Some(config_file) = value(&self.config_file) {
                    push(config_file);
                }
            }
        }
        paths
    }
}

fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

pub(crate) fn dedupe_existing_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::with_capacity(paths.len());
    let mut seen_paths = HashSet::new();
    let mut seen_identity = HashSet::new();
    for path in paths {
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        if let Some(identity) = existing_identity(&path) {
            if !seen_identity.insert(identity) {
                continue;
            }
        }
        deduped.push(path);
    }
    deduped
}

#[cfg(unix)]
fn existing_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn existing_identity(path: &Path) -> Option<String> {
    let path = std::fs::canonicalize(path).ok()?;
    #[cfg(windows)]
    {
        Some(path.to_string_lossy().to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        Some(path.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvVarGuard;

    #[test]
    fn config_resolver_keeps_one_home_xdg_and_fallback_snapshot() {
        let _lock = crate::test_env::lock_blocking();
        let home_name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let _home = EnvVarGuard::set(home_name, Some("snapshot-home-first"));
        let _xdg = EnvVarGuard::set("XDG_CONFIG_HOME", Some("snapshot-xdg-first"));
        let _fallback = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", Some("1"));
        let captured = ConfigPathResolver::capture();
        let _home_changed = EnvVarGuard::set(home_name, Some("snapshot-home-second"));
        let _xdg_changed = EnvVarGuard::set("XDG_CONFIG_HOME", Some("snapshot-xdg-second"));
        let _fallback_changed = EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", Some("0"));
        let paths = captured.resolve(&ConfigFileSelection::Default, ConfigPathEncoding::Native);
        assert!(paths
            .primary
            .contains(&PathBuf::from("snapshot-home-first/.rmux.conf")));
        assert!(paths
            .primary
            .contains(&PathBuf::from("snapshot-xdg-first/rmux/rmux.conf")));
        assert!(paths.fallback.is_empty());
        let current = ConfigPathResolver::capture()
            .resolve(&ConfigFileSelection::Default, ConfigPathEncoding::Native);
        assert!(current
            .primary
            .contains(&PathBuf::from("snapshot-home-second/.rmux.conf")));
        assert!(current
            .primary
            .contains(&PathBuf::from("snapshot-xdg-second/rmux/rmux.conf")));
        assert!(!current.fallback.is_empty());
    }

    #[test]
    fn config_resolver_preserves_explicit_repeats_and_typed_fallback_policy() {
        let resolver = ConfigPathResolver::capture();
        let file = PathBuf::from("config.conf");
        let explicit = resolver.resolve(
            &ConfigFileSelection::Files(vec![file.clone(), file.clone()]),
            ConfigPathEncoding::Native,
        );
        assert_eq!(explicit.primary, [file.clone(), file]);
        assert!(!explicit.should_load_fallback(ConfigLoadOutcome::Empty));
        let defaults = ResolvedConfigPaths {
            primary: vec![],
            fallback: vec!["tmux.conf".into()],
        };
        assert!(defaults.should_load_fallback(ConfigLoadOutcome::Empty));
        assert!(!defaults.should_load_fallback(ConfigLoadOutcome::Loaded));
        assert!(!defaults.should_load_fallback(ConfigLoadOutcome::Error));
        let disabled = resolver.resolve(&ConfigFileSelection::Disabled, ConfigPathEncoding::Native);
        assert!(disabled.primary.is_empty());
        assert!(!disabled.should_load_fallback(ConfigLoadOutcome::Empty));
    }
}
