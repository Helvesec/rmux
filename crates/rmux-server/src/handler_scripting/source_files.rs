use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rmux_core::command_parser::{CommandParseError, ParsedCommands};
use rmux_proto::{PaneTarget, RmuxError, SourceFileRequest};

use super::aggregate_rmux_errors;

const MAX_SOURCE_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
const DISABLE_TMUX_FALLBACK_ENV: &str = "RMUX_DISABLE_TMUX_FALLBACK";

#[derive(Debug, Default)]
pub(super) struct LoadedSourceFile {
    pub(super) commands: Vec<SourcedParsedCommands>,
    pub(super) stdout: Vec<u8>,
    errors: Vec<RmuxError>,
    parse_errors: bool,
    loaded_file_count: usize,
}

impl LoadedSourceFile {
    pub(super) fn loaded_any_file(&self) -> bool {
        self.loaded_file_count != 0
    }

    pub(super) fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub(super) fn record_loaded_files(&mut self, count: usize) {
        self.loaded_file_count += count;
    }

    pub(super) fn push_error(&mut self, error: RmuxError) {
        self.errors.push(error);
    }

    pub(super) fn push_parse_error(&mut self, error: RmuxError) {
        self.parse_errors = true;
        self.push_error(error);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceSyntax {
    Rmux,
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
            SourceSyntax::Rmux => SourceReadPolicy::Strict,
            SourceSyntax::TmuxCompat => SourceReadPolicy::BestEffort,
        }
    }
}

pub(super) fn default_config_paths() -> Vec<String> {
    #[cfg(windows)]
    {
        windows_default_config_paths()
    }
    #[cfg(not(windows))]
    {
        unix_default_config_paths()
    }
}

pub(super) fn default_tmux_fallback_paths() -> Vec<String> {
    if env_flag_enabled(DISABLE_TMUX_FALLBACK_ENV) {
        return Vec::new();
    }

    let paths = {
        #[cfg(windows)]
        {
            windows_tmux_fallback_paths()
        }
        #[cfg(not(windows))]
        {
            unix_tmux_fallback_paths()
        }
    };

    dedupe_existing_source_paths(paths)
}

fn dedupe_existing_source_paths(paths: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::with_capacity(paths.len());
    let mut seen_text = HashSet::new();
    let mut seen_identity = HashSet::new();

    for path in paths {
        if !seen_text.insert(path.clone()) {
            continue;
        }
        if let Some(identity) = existing_source_path_identity(Path::new(&path)) {
            if !seen_identity.insert(identity) {
                continue;
            }
        }
        deduped.push(path);
    }

    deduped
}

#[cfg(unix)]
fn existing_source_path_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path).ok()?;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn existing_source_path_identity(path: &Path) -> Option<String> {
    let canonical = fs::canonicalize(path).ok()?;
    #[cfg(windows)]
    {
        Some(canonical.to_string_lossy().to_ascii_lowercase())
    }
    #[cfg(not(windows))]
    {
        Some(canonical.to_string_lossy().into_owned())
    }
}

#[cfg(not(windows))]
fn unix_default_config_paths() -> Vec<String> {
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

#[cfg(not(windows))]
fn unix_tmux_fallback_paths() -> Vec<String> {
    let mut paths = Vec::new();
    let mut push_unique = |path: String| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    push_unique("/etc/tmux.conf".to_owned());
    if let Some(home) = nonempty_env("HOME") {
        push_unique(format!("{home}/.tmux.conf"));
    }
    if let Some(xdg_config_home) = nonempty_env("XDG_CONFIG_HOME") {
        push_unique(format!("{xdg_config_home}/tmux/tmux.conf"));
    }
    if let Some(home) = nonempty_env("HOME") {
        push_unique(format!("{home}/.config/tmux/tmux.conf"));
    }

    paths
}

#[cfg(windows)]
fn windows_default_config_paths() -> Vec<String> {
    let mut paths = Vec::new();
    let mut push_unique = |path: PathBuf| {
        let path = path.to_string_lossy().into_owned();
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    if let Some(xdg_config_home) = nonempty_env("XDG_CONFIG_HOME") {
        push_unique(
            PathBuf::from(xdg_config_home)
                .join("rmux")
                .join("rmux.conf"),
        );
    }
    if let Some(userprofile) = nonempty_env("USERPROFILE") {
        let userprofile = PathBuf::from(userprofile);
        push_unique(userprofile.join(".rmux.conf"));
    }
    if let Some(appdata) = nonempty_env("APPDATA") {
        push_unique(PathBuf::from(appdata).join("rmux").join("rmux.conf"));
    }
    if let Some(config_file) = nonempty_env("RMUX_CONFIG_FILE") {
        push_unique(PathBuf::from(config_file));
    }

    paths
}

#[cfg(windows)]
fn windows_tmux_fallback_paths() -> Vec<String> {
    let mut paths = Vec::new();
    let mut push_unique = |path: PathBuf| {
        let path = path.to_string_lossy().into_owned();
        if !paths.contains(&path) {
            paths.push(path);
        }
    };

    if let Some(xdg_config_home) = nonempty_env("XDG_CONFIG_HOME") {
        push_unique(
            PathBuf::from(xdg_config_home)
                .join("tmux")
                .join("tmux.conf"),
        );
    }
    if let Some(userprofile) = nonempty_env("USERPROFILE") {
        push_unique(PathBuf::from(userprofile).join(".tmux.conf"));
    }
    if let Some(appdata) = nonempty_env("APPDATA") {
        push_unique(PathBuf::from(appdata).join("tmux").join("tmux.conf"));
    }

    paths
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_flag_enabled(name: &str) -> bool {
    let Ok(value) = std::env::var(name) else {
        return false;
    };
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

pub(super) fn source_inputs_for_path(
    path: &str,
    cwd: Option<&Path>,
    quiet: bool,
    stdin: Option<&str>,
    read_policy: SourceReadPolicy,
) -> Result<Vec<SourceInput>, RmuxError> {
    #[cfg(unix)]
    if is_unix_null_config_path(path) {
        return Ok(vec![SourceInput {
            current_file: path.to_owned(),
            contents: String::new(),
        }]);
    }

    #[cfg(windows)]
    if is_windows_null_config_path(path) {
        return Ok(vec![SourceInput {
            current_file: path.to_owned(),
            contents: String::new(),
        }]);
    }

    if path == "-" {
        let Some(stdin) = stdin else {
            return Err(RmuxError::Server(
                "source-file - requires client stdin".to_owned(),
            ));
        };
        return Ok(vec![SourceInput {
            current_file: "-".to_owned(),
            contents: strip_utf8_bom(stdin.to_owned()),
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

    let skip_glob_directories = source_path_has_glob_metachars(path);
    let mut inputs = Vec::new();
    for entry in entries {
        if skip_glob_directories && source_entry_is_directory(&entry) {
            continue;
        }
        match read_source_entry(&entry, read_policy) {
            Ok(contents) => inputs.push(SourceInput {
                current_file: source_entry_display_path(&entry),
                contents: strip_utf8_bom(contents),
            }),
            Err(error) if quiet && error.kind() == io::ErrorKind::NotFound => {}
            Err(_) if read_policy == SourceReadPolicy::BestEffort => {}
            Err(error) => {
                return Err(RmuxError::Server(format!(
                    "{}: {}",
                    source_entry_display_path(&entry),
                    source_io_error_message(&error)
                )));
            }
        }
    }

    Ok(inputs)
}

fn source_path_has_glob_metachars(path: &str) -> bool {
    path.chars().any(|ch| matches!(ch, '*' | '?' | '[' | ']'))
}

fn source_entry_is_directory(entry: &Path) -> bool {
    fs::metadata(entry)
        .map(|metadata| metadata.file_type().is_dir())
        .unwrap_or(false)
}

fn source_io_error_message(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "No such file or directory".to_owned(),
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        io::ErrorKind::IsADirectory => "Is a directory".to_owned(),
        _ => error.to_string(),
    }
}

fn read_source_entry(entry: &Path, read_policy: SourceReadPolicy) -> io::Result<String> {
    match read_policy {
        SourceReadPolicy::Strict => read_limited_source_entry(entry),
        SourceReadPolicy::BestEffort => read_tmux_compat_source_entry(entry),
    }
}

fn read_limited_source_entry(entry: &Path) -> io::Result<String> {
    let metadata = fs::metadata(entry)?;
    validate_strict_source_metadata(&metadata)?;

    let file = open_strict_source_entry(entry)?;
    let metadata = file.metadata()?;
    validate_strict_source_metadata(&metadata)?;
    let mut contents = String::new();
    let mut reader = file.take(MAX_SOURCE_CONFIG_BYTES + 1);
    reader.read_to_string(&mut contents)?;
    if contents.len() as u64 > MAX_SOURCE_CONFIG_BYTES {
        return Err(oversized_source_config_error());
    }
    Ok(contents)
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
    if metadata.len() > MAX_SOURCE_CONFIG_BYTES {
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

fn read_tmux_compat_source_entry(entry: &Path) -> io::Result<String> {
    let preopen_metadata = fs::metadata(entry)?;
    validate_tmux_compat_regular_metadata(&preopen_metadata)?;

    let file = open_tmux_compat_regular_file(entry)?;
    let metadata = file.metadata()?;
    validate_tmux_compat_regular_metadata(&metadata)?;

    let mut contents = String::new();
    let mut reader = file.take(MAX_SOURCE_CONFIG_BYTES + 1);
    reader.read_to_string(&mut contents)?;
    if contents.len() as u64 > MAX_SOURCE_CONFIG_BYTES {
        return Err(oversized_source_config_error());
    }
    Ok(contents)
}

fn validate_tmux_compat_regular_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "tmux fallback config is not a regular file",
        ));
    }
    if metadata.len() > MAX_SOURCE_CONFIG_BYTES {
        return Err(oversized_source_config_error());
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

fn oversized_source_config_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "source file exceeds 16 MiB")
}

fn strip_utf8_bom(mut contents: String) -> String {
    if contents.starts_with('\u{feff}') {
        contents.replace_range(..'\u{feff}'.len_utf8(), "");
    }
    contents
}

#[cfg(unix)]
fn is_unix_null_config_path(path: &str) -> bool {
    Path::new(path) == Path::new("/dev/null")
}

#[cfg(windows)]
fn is_windows_null_config_path(path: &str) -> bool {
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

fn glob_pattern_for_source_path(path: &str, cwd: Option<&Path>) -> String {
    let path = Path::new(path);
    if path.is_absolute() {
        return path_to_glob_pattern(path);
    }

    match cwd {
        Some(cwd) => format!(
            "{}/{}",
            glob::Pattern::escape(&path_to_glob_pattern(cwd)),
            path_to_glob_pattern(path)
        ),
        None => path_to_glob_pattern(path),
    }
}

fn path_to_glob_pattern(path: &Path) -> String {
    #[cfg(windows)]
    {
        path.to_string_lossy().replace('\\', "/")
    }

    #[cfg(not(windows))]
    {
        path.to_string_lossy().into_owned()
    }
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
    RmuxError::Message(format!("{path}: No such file or directory"))
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
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(windows)]
    use super::glob_pattern_for_source_path;

    use super::{source_inputs_for_path, strip_utf8_bom, LoadedSourceFile, SourceReadPolicy};
    use rmux_proto::RmuxError;

    #[test]
    fn strips_utf8_bom_from_source_text() {
        assert_eq!(
            strip_utf8_bom("\u{feff}set -g status off".to_owned()),
            "set -g status off"
        );
        assert_eq!(
            strip_utf8_bom("set -g status off".to_owned()),
            "set -g status off"
        );
    }

    #[test]
    fn source_file_stdin_strips_utf8_bom() {
        let inputs = source_inputs_for_path(
            "-",
            None,
            false,
            Some("\u{feff}set -g status off"),
            SourceReadPolicy::Strict,
        )
        .expect("stdin source should load");

        assert_eq!(inputs[0].contents, "set -g status off");
    }

    #[test]
    fn source_file_path_strips_utf8_bom() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rmux-source-bom-{}-{unique}.conf",
            std::process::id()
        ));
        std::fs::write(&path, "\u{feff}set -g status-left ok").expect("write source file");

        let inputs = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::Strict,
        )
        .expect("file source should load");
        let _ = std::fs::remove_file(&path);

        assert_eq!(inputs[0].contents, "set -g status-left ok");
    }

    #[test]
    fn tmux_best_effort_source_skips_oversized_files() {
        let path = temp_source_path("oversized-tmux-fallback");
        let contents = "x".repeat((super::MAX_SOURCE_CONFIG_BYTES + 1) as usize);
        std::fs::write(&path, contents).expect("write oversized source file");

        let inputs = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::BestEffort,
        )
        .expect("best-effort tmux source should skip oversized files");
        let _ = std::fs::remove_file(&path);

        assert!(inputs.is_empty());
    }

    #[test]
    fn strict_source_rejects_oversized_files() {
        let path = temp_source_path("oversized-strict-source");
        let contents = "x".repeat((super::MAX_SOURCE_CONFIG_BYTES + 1) as usize);
        std::fs::write(&path, contents).expect("write oversized source file");

        let error = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::Strict,
        )
        .expect_err("strict source should reject oversized files");
        let _ = std::fs::remove_file(&path);

        assert!(
            error.to_string().contains("source file exceeds 16 MiB"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn strict_source_accepts_multi_megabyte_configs_below_limit() {
        let path = temp_source_path("large-strict-source");
        let contents = format!(
            "{}set -g status off\n",
            "# large config padding\n".repeat(80_000)
        );
        assert!(
            contents.len() as u64 > 1024 * 1024,
            "test fixture should exceed the old 1 MiB limit"
        );
        assert!(
            contents.len() as u64 <= super::MAX_SOURCE_CONFIG_BYTES,
            "test fixture should stay below the current source-file limit"
        );
        std::fs::write(&path, contents.clone()).expect("write large source file");

        let inputs = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::Strict,
        )
        .expect("strict source should accept multi-megabyte configs below the hard limit");
        let _ = std::fs::remove_file(&path);

        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].contents, contents);
    }

    #[test]
    fn strict_source_rejects_directories_with_directory_error() {
        let path = temp_source_path("directory-strict-source");
        std::fs::create_dir(&path).expect("create directory source entry");

        let error = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::Strict,
        )
        .expect_err("strict source should reject directories");
        let _ = std::fs::remove_dir(&path);

        assert!(
            error.to_string().contains("Is a directory"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn strict_source_glob_skips_directories_and_reads_regular_entries() {
        let root = temp_source_path("glob-with-directory-strict-source");
        std::fs::create_dir(&root).expect("create glob source root");
        let first = root.join("a.conf");
        let second = root.join("b.conf");
        let directory = root.join("nested");
        std::fs::write(&first, "set -g @a yes\n").expect("write first source");
        std::fs::write(&second, "set -g @b yes\n").expect("write second source");
        std::fs::create_dir(&directory).expect("create directory glob match");

        let inputs = source_inputs_for_path(
            &format!("{}/*", root.to_string_lossy()),
            None,
            false,
            None,
            SourceReadPolicy::Strict,
        )
        .expect("strict glob source should skip matched directories");
        let _ = std::fs::remove_file(&first);
        let _ = std::fs::remove_file(&second);
        let _ = std::fs::remove_dir(&directory);
        let _ = std::fs::remove_dir(&root);

        let contents = inputs
            .iter()
            .map(|input| input.contents.as_str())
            .collect::<Vec<_>>();
        assert_eq!(inputs.len(), 2);
        assert!(contents.contains(&"set -g @a yes\n"));
        assert!(contents.contains(&"set -g @b yes\n"));
    }

    #[cfg(unix)]
    #[test]
    fn strict_source_rejects_fifo_without_blocking() {
        let path = temp_source_path("fifo-strict-source");
        create_test_fifo(&path);

        let error = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::Strict,
        )
        .expect_err("strict source should reject fifo");
        let _ = std::fs::remove_file(&path);

        assert!(
            error
                .to_string()
                .contains("source file is not a regular file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn tmux_best_effort_source_skips_non_regular_files() {
        let path = temp_source_path("non-regular-tmux-fallback");
        std::fs::create_dir(&path).expect("create non-regular source entry");

        let inputs = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::BestEffort,
        )
        .expect("best-effort tmux source should skip non-regular files");
        let _ = std::fs::remove_dir(&path);

        assert!(inputs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn tmux_best_effort_source_skips_fifo_without_blocking() {
        let path = temp_source_path("fifo-tmux-fallback");
        create_test_fifo(&path);

        let inputs = source_inputs_for_path(
            &path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::BestEffort,
        )
        .expect("best-effort tmux source should skip fifo");
        let _ = std::fs::remove_file(&path);

        assert!(inputs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn tmux_best_effort_source_skips_symlink_to_fifo_without_blocking() {
        let fifo_path = temp_source_path("symlink-target-fifo-tmux-fallback");
        let symlink_path = temp_source_path("symlink-tmux-fallback");
        create_test_fifo(&fifo_path);
        std::os::unix::fs::symlink(&fifo_path, &symlink_path).expect("create source symlink");

        let inputs = source_inputs_for_path(
            &symlink_path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::BestEffort,
        )
        .expect("best-effort tmux source should skip symlink to fifo");
        let _ = std::fs::remove_file(&symlink_path);
        let _ = std::fs::remove_file(&fifo_path);

        assert!(inputs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn tmux_best_effort_source_accepts_symlink_to_regular_file() {
        let target_path = temp_source_path("symlink-target-regular-tmux-fallback");
        let symlink_path = temp_source_path("regular-symlink-tmux-fallback");
        std::fs::write(&target_path, "set -g status off\n").expect("write target config");
        std::os::unix::fs::symlink(&target_path, &symlink_path).expect("create source symlink");

        let inputs = source_inputs_for_path(
            &symlink_path.to_string_lossy(),
            None,
            false,
            None,
            SourceReadPolicy::BestEffort,
        )
        .expect("best-effort tmux source should accept regular-file symlinks");
        let _ = std::fs::remove_file(&symlink_path);
        let _ = std::fs::remove_file(&target_path);

        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].contents, "set -g status off\n");
    }

    #[cfg(unix)]
    #[test]
    fn fallback_path_dedupe_collapses_symlinked_entries() {
        let target_path = temp_source_path("fallback-dedupe-target");
        let symlink_path = temp_source_path("fallback-dedupe-symlink");
        let missing_path = temp_source_path("fallback-dedupe-missing");
        std::fs::write(&target_path, "set -g status off\n").expect("write target config");
        std::os::unix::fs::symlink(&target_path, &symlink_path).expect("create source symlink");

        let deduped = super::dedupe_existing_source_paths(vec![
            target_path.to_string_lossy().into_owned(),
            symlink_path.to_string_lossy().into_owned(),
            missing_path.to_string_lossy().into_owned(),
        ]);
        let _ = std::fs::remove_file(&symlink_path);
        let _ = std::fs::remove_file(&target_path);

        assert_eq!(
            deduped,
            vec![
                target_path.to_string_lossy().into_owned(),
                missing_path.to_string_lossy().into_owned()
            ],
            "fallback discovery must not source the same real file twice"
        );
    }

    #[test]
    fn loaded_source_file_tracks_errors_for_fallback_gating() {
        let mut loaded = LoadedSourceFile::default();
        assert!(!loaded.loaded_any_file());
        assert!(!loaded.has_errors());

        loaded.push_error(RmuxError::Server("permission denied".to_owned()));

        assert!(!loaded.loaded_any_file());
        assert!(loaded.has_errors());
    }

    #[cfg(windows)]
    #[test]
    fn windows_relative_source_file_uses_glob_safe_separators() {
        let pattern = glob_pattern_for_source_path(
            "nested\\*.conf",
            Some(std::path::Path::new(r"C:\Users\RMUXUser\rmux")),
        );

        assert_eq!(pattern, "C:/Users/RMUXUser/rmux/nested/*.conf");
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_source_file_uses_forward_slashes() {
        let pattern = glob_pattern_for_source_path(r"C:\Users\RMUXUser\rmux\config.conf", None);

        assert_eq!(pattern, "C:/Users/RMUXUser/rmux/config.conf");
    }

    #[cfg(windows)]
    #[test]
    fn windows_null_device_config_paths_are_ignored() {
        assert!(super::is_windows_null_config_path("NUL"));
        assert!(super::is_windows_null_config_path("nul:"));
        assert!(super::is_windows_null_config_path(r"C:\tmp\NUL"));
        assert!(super::is_windows_null_config_path(r"C:\tmp\NUL.conf"));
        assert!(super::is_windows_null_config_path(r"\\.\NUL"));
        assert!(!super::is_windows_null_config_path(r"C:\tmp\null.conf"));
        assert!(!super::is_windows_null_config_path(r"C:\tmp\nulled"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_dev_null_config_path_is_empty() {
        let inputs =
            source_inputs_for_path("/dev/null", None, false, None, SourceReadPolicy::Strict)
                .expect("/dev/null should behave like an empty config file");

        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].current_file, "/dev/null");
        assert!(inputs[0].contents.is_empty());
    }

    fn temp_source_path(label: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rmux-source-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn create_test_fifo(path: &std::path::Path) {
        let output = std::process::Command::new("mkfifo")
            .arg(path)
            .output()
            .expect("run mkfifo");
        assert!(
            output.status.success(),
            "mkfifo failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
