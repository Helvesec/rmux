use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

#[cfg(all(test, windows))]
use rmux_core::OptionStore;
use rmux_proto::{ProcessCommand, RmuxError};
use rmux_pty::{ChildCommand, PtyChild, PtyMaster, RmuxShell, TerminalSize as PtyTerminalSize};
use tokio::runtime::Handle;

mod profile;
mod shell_resolver;
mod shell_spec;

#[cfg(unix)]
pub(crate) use shell_resolver::is_suitable_shell;
use shell_resolver::resolve_program_path_from;
#[cfg(test)]
use shell_resolver::resolve_shell_path;
#[cfg(windows)]
use shell_resolver::CLIENT_SHELL_ENV;
use shell_spec::ShellSpec;

#[cfg(windows)]
const WINDOWS_BATCH_ARGV_UNSUPPORTED_MESSAGE: &str =
    "process command argv cannot target Windows .cmd or .bat scripts; use shell command mode";

/// Immutable pane-spawn metadata captured when a pane terminal is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalProfile {
    cwd: PathBuf,
    shell: PathBuf,
    raw_environment: Arc<Vec<(OsString, OsString)>>,
}

/// Session-level environment captured from the pane that created a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionBaseEnvironment {
    raw_environment: Vec<(OsString, OsString)>,
}

#[cfg(windows)]
fn suppress_client_shell_environment(
    resolved: &mut HashMap<String, String>,
    mut suppressed_raw_names: HashSet<String>,
) -> HashSet<String> {
    remove_environment_value(resolved, CLIENT_SHELL_ENV);
    suppressed_raw_names.insert(CLIENT_SHELL_ENV.to_owned());
    suppressed_raw_names
}

#[cfg(not(windows))]
fn suppress_client_shell_environment(
    _resolved: &mut HashMap<String, String>,
    suppressed_raw_names: HashSet<String>,
) -> HashSet<String> {
    suppressed_raw_names
}

fn suppress_terminal_program_environment(suppressed_raw_names: &mut HashSet<String>) {
    suppressed_raw_names.insert("TERM_PROGRAM".to_owned());
    suppressed_raw_names.insert("TERM_PROGRAM_VERSION".to_owned());
}

pub(crate) fn base_process_environment() -> HashMap<String, String> {
    environment_from_os_pairs(std::env::vars_os())
}

pub(crate) fn base_process_environment_display_only() -> HashMap<String, String> {
    display_environment_from_os_pairs(std::env::vars_os())
}

fn raw_process_environment(
    raw_base_environment: Option<&[(OsString, OsString)]>,
    resolved: &HashMap<String, String>,
    suppressed_names: &HashSet<String>,
) -> Vec<(OsString, OsString)> {
    let mut raw = match raw_base_environment {
        Some(environment) => environment.to_vec(),
        None => std::env::vars_os().collect(),
    };
    raw.retain(|(name, _)| {
        !suppressed_names
            .iter()
            .any(|suppressed| os_environment_name_eq(name, suppressed))
            && !resolved
                .keys()
                .any(|resolved_name| os_environment_name_eq(name, resolved_name))
    });
    raw.extend(
        resolved
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
    );
    raw.retain(|(name, _)| !rmux_os::is_internal_environment_name(name));
    raw
}

fn environment_from_os_pairs<I>(pairs: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    pairs
        .into_iter()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

fn display_environment_from_os_pairs<I>(pairs: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    pairs
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.into_string().ok()?;
            if value.clone().into_string().is_ok() {
                return None;
            }
            Some((name, display_os_environment_value(&value)))
        })
        .collect()
}

#[cfg(unix)]
fn display_os_environment_value(value: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;

    value
        .as_bytes()
        .iter()
        .map(|byte| match *byte {
            b'\\' => "\\\\".to_owned(),
            0x20..=0x7e => char::from(*byte).to_string(),
            other => format!("\\{other:03o}"),
        })
        .collect()
}

#[cfg(windows)]
fn display_os_environment_value(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

#[cfg(windows)]
fn os_environment_name_eq(left: &OsStr, right: &str) -> bool {
    left.to_string_lossy().eq_ignore_ascii_case(right)
}

#[cfg(not(windows))]
fn os_environment_name_eq(left: &OsStr, right: &str) -> bool {
    left == OsStr::new(right)
}

fn shell_command_window_name(command: &str) -> Option<String> {
    #[cfg(windows)]
    if is_lone_windows_program(command) {
        return executable_name(command);
    }
    // Quoted and unquoted fragments belong to the same program word.
    let mut first = String::new();
    let mut quoted = false;
    for character in command.trim_start().chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => break,
            character => first.push(character),
        }
    }
    executable_name(first)
}

pub(crate) fn spawn_pane_process(
    shell: &dyn RmuxShell,
    size: PtyTerminalSize,
    profile: &TerminalProfile,
    command: Option<&ProcessCommand>,
) -> Result<(PtyMaster, PtyChild), RmuxError> {
    validate_process_command(command)?;
    let mut command = spawn_command(profile, command)?
        .size(size)
        .allow_explicit_job_breakaway()
        .clear_env()
        .current_dir(profile.cwd());

    for (name, value) in profile.raw_environment() {
        command = command.env(name, value);
    }

    let spawned = shell.spawn(command).map_err(|error| {
        RmuxError::spawn_failed(format!(
            "{} shell: {error}",
            rmux_proto::SPAWN_FAILED_MESSAGE_PREFIX
        ))
    })?;
    let (master, child) = spawned.into_parts();
    Ok((master, child))
}

pub(crate) fn validate_process_command(command: Option<&ProcessCommand>) -> Result<(), RmuxError> {
    let empty_argv = matches!(
        command,
        Some(ProcessCommand::Argv(argv)) if argv.is_empty() || argv.first().is_some_and(String::is_empty)
    );
    if empty_argv {
        return Err(RmuxError::empty_process_command());
    }
    #[cfg(windows)]
    if let Some(ProcessCommand::Argv(argv)) = command {
        if let Some(program) = argv.first() {
            reject_windows_batch_argv(Path::new(program))?;
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn validate_windows_process_command_for_profile(
    profile: &TerminalProfile,
    command: Option<&ProcessCommand>,
) -> Result<(), RmuxError> {
    validate_process_command(command)?;
    let _ = spawn_command(profile, command)?;
    Ok(())
}

fn spawn_command(
    profile: &TerminalProfile,
    command: Option<&ProcessCommand>,
) -> Result<ChildCommand, RmuxError> {
    let command = match command {
        // A lone Windows program path, often under "Program Files", would split
        // at its spaces in the shell, so it runs directly.
        #[cfg(windows)]
        Some(ProcessCommand::Shell(command)) if is_lone_windows_program(command) => {
            ChildCommand::new(command)
        }
        Some(ProcessCommand::Shell(command)) => profile.pane_shell_child_command(command),
        Some(ProcessCommand::Argv(argv)) if !argv.is_empty() => {
            let environment = profile.environment_map();
            let program =
                resolve_program_path_from(Path::new(&argv[0]), &environment, profile.cwd());
            #[cfg(windows)]
            reject_windows_batch_argv(&program)?;
            ChildCommand::new(program).args(&argv[1..])
        }
        Some(ProcessCommand::Argv(_)) | Some(_) | None => profile.interactive_child_command(),
    };
    Ok(command)
}

#[cfg(windows)]
fn is_lone_windows_program(command: &str) -> bool {
    let path = Path::new(command);
    path.is_absolute()
        && path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("com")
            })
        && path.is_file()
}

#[cfg(windows)]
fn reject_windows_batch_argv(program: &Path) -> Result<(), RmuxError> {
    if is_windows_batch_script(program) {
        return Err(RmuxError::spawn_failed(
            WINDOWS_BATCH_ARGV_UNSUPPORTED_MESSAGE,
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn is_windows_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "bat" | "cmd"))
        .unwrap_or(false)
}

pub(crate) fn shell_std_command(shell: &Path, cwd: &Path, command: &str) -> Command {
    let mut command = ShellSpec::new(shell).command_std_child(cwd, command);
    configure_hidden_std_helper(&mut command);
    command
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn configure_hidden_std_helper(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_std_helper(_command: &mut Command) {}

#[cfg(test)]
pub(crate) fn spawn_hook_command(command: String) -> io::Result<()> {
    spawn_hook_child(default_hook_command(command)?)
}

pub(crate) fn spawn_hook_command_with_profile(
    command: String,
    profile: &TerminalProfile,
) -> io::Result<()> {
    let mut child = profile.shell_std_command(&command);
    child.current_dir(profile.cwd()).env_clear();
    for (name, value) in profile.environment() {
        child.env(name, value);
    }
    spawn_hook_child(child)
}

fn spawn_hook_child(mut child: Command) -> io::Result<()> {
    let handle = Handle::try_current().map_err(io::Error::other)?;
    child
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = child.spawn()?;

    handle.spawn_blocking(move || {
        let mut child = child;
        let _ = child.wait();
    });

    Ok(())
}

#[cfg(test)]
fn default_hook_command(command: String) -> io::Result<Command> {
    #[cfg(unix)]
    {
        let mut child = Command::new("sh");
        child.arg("-c").arg(command);
        Ok(child)
    }

    #[cfg(windows)]
    {
        let options = OptionStore::new();
        let environment = std::env::vars().collect::<HashMap<_, _>>();
        let cwd = resolve_working_directory(None).map_err(io::Error::other)?;
        let shell = resolve_shell_path(&options, None, &environment);
        let mut child = ShellSpec::new(&shell).command_std_child(&cwd, &command);
        child.current_dir(cwd);
        Ok(child)
    }
}

pub(crate) fn parse_environment_assignments(
    values: &[String],
) -> Result<HashMap<String, String>, RmuxError> {
    let mut environment = HashMap::new();

    for value in values {
        #[cfg(windows)]
        if value.starts_with('=') {
            continue;
        }

        let Some((name, value)) = value.split_once('=') else {
            return Err(RmuxError::Server(format!(
                "environment assignment must be NAME=VALUE: {value}"
            )));
        };
        if name.is_empty() {
            return Err(RmuxError::Server(
                "environment assignment name must not be empty".to_owned(),
            ));
        }
        environment.insert(name.to_owned(), value.to_owned());
    }

    Ok(environment)
}

fn set_environment_value(environment: &mut HashMap<String, String>, name: String, value: String) {
    remove_environment_value(environment, &name);

    environment.insert(name, value);
}

fn remove_environment_value(environment: &mut HashMap<String, String>, name: &str) {
    #[cfg(windows)]
    if let Some(existing) = environment
        .keys()
        .find(|key| key.eq_ignore_ascii_case(name))
        .cloned()
    {
        environment.remove(&existing);
        return;
    }

    environment.remove(name);
}

fn set_raw_environment_value(
    environment: &mut Vec<(OsString, OsString)>,
    name: OsString,
    value: OsString,
) {
    let name_string = name.to_string_lossy().into_owned();
    environment.retain(|(existing, _)| !os_environment_name_eq(existing, &name_string));
    environment.push((name, value));
}

fn resolve_working_directory(requested_cwd: Option<&Path>) -> Result<PathBuf, RmuxError> {
    let requested = requested_cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    for candidate in requested
        .into_iter()
        .chain(std::env::var_os("USERPROFILE").map(PathBuf::from))
        .chain(std::env::var_os("HOME").map(PathBuf::from))
        .chain(std::iter::once(default_working_directory()))
    {
        if candidate.is_dir() {
            return Ok(candidate);
        }
    }

    Err(RmuxError::Server(
        "failed to resolve a working directory".to_owned(),
    ))
}

fn mux_environment_socket_path(socket_path: &Path) -> PathBuf {
    let absolute = if socket_path.is_absolute() {
        socket_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(socket_path))
            .unwrap_or_else(|_| socket_path.to_path_buf())
    };
    canonical_path_or_parent(absolute)
}

fn canonical_path_or_parent(path: PathBuf) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(&path) {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => std::fs::canonicalize(parent)
            .map(|canonical_parent| canonical_parent.join(file_name))
            .unwrap_or(path),
        _ => path,
    }
}

fn default_working_directory() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/")
    }
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\")
    }
}

fn shell_program_name(path: &Path) -> Option<String> {
    executable_name(path.as_os_str())
}

fn executable_name(path: impl AsRef<std::ffi::OsStr>) -> Option<String> {
    let name = Path::new(path.as_ref()).file_name()?.to_string_lossy();
    let trimmed = name.trim_start_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
#[path = "terminal/hook_tests.rs"]
mod hook_tests;
#[cfg(test)]
#[path = "terminal/profile_env_tests.rs"]
mod profile_env_tests;
#[cfg(test)]
#[path = "terminal/tests.rs"]
mod tests;
