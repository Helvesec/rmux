use std::collections::HashMap;
#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rmux_core::{EnvironmentStore, OptionStore, PaneId};
use rmux_proto::{OptionName, RmuxError, SessionName};
use rmux_pty::{ChildCommand, PtyChild, PtyMaster, TerminalSize as PtyTerminalSize};
#[cfg(unix)]
use rustix::process::getuid;
use tokio::runtime::Handle;

mod shell_spec;

use shell_spec::ShellSpec;

/// Immutable pane-spawn metadata captured when a pane terminal is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalProfile {
    cwd: PathBuf,
    shell: PathBuf,
    environment: HashMap<String, String>,
}

impl TerminalProfile {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_session(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: &SessionName,
        session_id: u32,
        socket_path: &Path,
        include_terminal_defaults: bool,
        overrides: Option<&[String]>,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        let mut resolved = std::env::vars().collect::<HashMap<_, _>>();
        environment.apply_to_process_environment(Some(session_name), &mut resolved);

        if include_terminal_defaults {
            if let Some(default_terminal) =
                options.resolve(Some(session_name), OptionName::DefaultTerminal)
            {
                resolved.insert("TERM".to_owned(), default_terminal.to_owned());
            }
            resolved.insert("TERM_PROGRAM".to_owned(), "rmux".to_owned());
            resolved.insert(
                "TERM_PROGRAM_VERSION".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            );
            resolved.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        } else {
            resolved.remove("TERM_PROGRAM");
            resolved.remove("TERM_PROGRAM_VERSION");
            resolved.remove("COLORTERM");
        }

        resolved.insert(
            "RMUX".to_owned(),
            format!(
                "{},{},{}",
                socket_path.display(),
                std::process::id(),
                session_id
            ),
        );

        let cwd = resolve_working_directory(requested_cwd)?;
        let shell = resolve_shell_path(options, Some(session_name), &resolved);
        resolved.insert("SHELL".to_owned(), shell.to_string_lossy().into_owned());

        if let Some(overrides) = overrides {
            for (name, value) in parse_environment_assignments(overrides)? {
                resolved.insert(name, value);
            }
        }

        if let Some(pane_id) = pane_id {
            resolved.insert("RMUX_PANE".to_owned(), format!("%{}", pane_id.as_u32()));
        }

        resolved.insert("PWD".to_owned(), cwd.to_string_lossy().into_owned());

        Ok(Self {
            cwd,
            shell,
            environment: resolved,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_run_shell(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: Option<&SessionName>,
        session_id: Option<u32>,
        socket_path: &Path,
        include_terminal_defaults: bool,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        let mut resolved = std::env::vars().collect::<HashMap<_, _>>();
        environment.apply_to_process_environment(session_name, &mut resolved);

        if include_terminal_defaults {
            if let Some(default_terminal) = session_name
                .and_then(|session_name| {
                    options.resolve(Some(session_name), OptionName::DefaultTerminal)
                })
                .or_else(|| options.resolve(None, OptionName::DefaultTerminal))
            {
                resolved.insert("TERM".to_owned(), default_terminal.to_owned());
            }
            resolved.insert("TERM_PROGRAM".to_owned(), "rmux".to_owned());
            resolved.insert(
                "TERM_PROGRAM_VERSION".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            );
            resolved.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        }

        resolved.insert(
            "RMUX".to_owned(),
            format!(
                "{},{},{}",
                socket_path.display(),
                std::process::id(),
                session_id.map_or(-1_i32, |id| i32::try_from(id).unwrap_or(i32::MAX))
            ),
        );

        let cwd = resolve_working_directory(requested_cwd)?;
        let shell = resolve_shell_path(options, session_name, &resolved);
        resolved.insert("SHELL".to_owned(), shell.to_string_lossy().into_owned());
        resolved.insert("PWD".to_owned(), cwd.to_string_lossy().into_owned());

        Ok(Self {
            cwd,
            shell,
            environment: resolved,
        })
    }

    pub(crate) fn environment(&self) -> impl Iterator<Item = (&str, &str)> {
        self.environment
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn shell(&self) -> &Path {
        &self.shell
    }

    pub(crate) fn environment_value(&self, name: &str) -> Option<&str> {
        self.environment.get(name).map(String::as_str)
    }

    pub(crate) fn default_window_name(&self) -> Option<String> {
        self.environment_value("TERM_PROGRAM")
            .filter(|value| *value == "rmux")
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| shell_program_name(&self.shell))
    }

    pub(crate) fn automatic_window_name(&self, command: Option<&[String]>) -> Option<String> {
        if command.is_some() {
            self.runtime_window_name(command)
        } else {
            self.default_window_name()
        }
    }

    pub(crate) fn runtime_window_name(&self, command: Option<&[String]>) -> Option<String> {
        match command {
            Some([single]) => {
                shell_command_window_name(single).or_else(|| shell_program_name(&self.shell))
            }
            Some(argv) if !argv.is_empty() => executable_name(&argv[0]),
            None => shell_program_name(&self.shell),
            Some(_) => shell_program_name(&self.shell),
        }
    }
}

fn shell_command_window_name(command: &str) -> Option<String> {
    let first = command.split_whitespace().next()?;
    executable_name(first)
}

pub(crate) fn spawn_pane_process(
    size: PtyTerminalSize,
    profile: &TerminalProfile,
    command: Option<&[String]>,
) -> Result<(PtyMaster, PtyChild), RmuxError> {
    let mut command = spawn_command(profile, command)
        .size(size)
        .clear_env()
        .current_dir(profile.cwd());

    for (name, value) in profile.environment() {
        command = command.env(name, value);
    }

    let spawned = command
        .spawn()
        .map_err(|error| RmuxError::Server(format!("failed to spawn pane shell: {error}")))?;
    let (master, child) = spawned.into_parts();
    Ok((master, child))
}

fn spawn_command(profile: &TerminalProfile, command: Option<&[String]>) -> ChildCommand {
    let shell = ShellSpec::new(profile.shell());
    match command {
        Some([single]) => shell.command_child(profile.cwd(), single),
        Some(argv) if !argv.is_empty() => ChildCommand::new(&argv[0]).args(&argv[1..]),
        _ => shell.interactive_child(profile.cwd()),
    }
}

pub(crate) fn spawn_hook_command(command: String) -> io::Result<()> {
    let handle = Handle::try_current().map_err(io::Error::other)?;
    let child = hook_command(command).spawn()?;

    handle.spawn_blocking(move || {
        let mut child = child;
        let _ = child.wait();
    });

    Ok(())
}

fn hook_command(command: String) -> Command {
    #[cfg(unix)]
    {
        let mut child = Command::new("sh");
        child.arg("-c").arg(command);
        child
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        child
    }

    #[cfg(windows)]
    {
        let shell = std::env::var_os("POWERSHELL")
            .or_else(|| std::env::var_os("PWsh"))
            .unwrap_or_else(|| OsString::from("powershell.exe"));
        let mut child = Command::new(shell);
        child.arg("-NoProfile").arg("-Command").arg(command);
        child
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        child
    }
}

pub(crate) fn parse_environment_assignments(
    values: &[String],
) -> Result<HashMap<String, String>, RmuxError> {
    let mut environment = HashMap::new();

    for value in values {
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

fn resolve_shell_path(
    options: &OptionStore,
    session_name: Option<&SessionName>,
    environment: &HashMap<String, String>,
) -> PathBuf {
    session_name
        .and_then(|session_name| options.resolve(Some(session_name), OptionName::DefaultShell))
        .or_else(|| options.resolve(None, OptionName::DefaultShell))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(normalize_shell_path)
        .or_else(current_user_login_shell)
        .or_else(|| environment.get("SHELL").map(PathBuf::from))
        .map(normalize_shell_path)
        .unwrap_or_else(default_shell_path)
}

#[cfg(unix)]
fn normalize_shell_path(path: PathBuf) -> PathBuf {
    path
}

#[cfg(windows)]
fn normalize_shell_path(path: PathBuf) -> PathBuf {
    if path.components().count() > 1 {
        return path;
    }

    find_shell_on_path(&path).unwrap_or(path)
}

#[cfg(windows)]
fn find_shell_on_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    if matches!(name.as_str(), "cmd" | "cmd.exe") {
        return env::var_os("COMSPEC").map(PathBuf::from).or_else(|| {
            env::var_os("SystemRoot")
                .map(|root| PathBuf::from(root).join("System32").join("cmd.exe"))
        });
    }
    if matches!(name.as_str(), "powershell" | "powershell.exe") {
        return env::var_os("SystemRoot").map(|root| {
            PathBuf::from(root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        });
    }

    search_path(path)
}

#[cfg(windows)]
fn search_path(path: &Path) -> Option<PathBuf> {
    let path_value = env::var_os("PATH")?;
    let extensions = executable_extensions(path);
    for directory in env::split_paths(&path_value) {
        for extension in &extensions {
            let candidate = directory.join(format!("{}{}", path.to_string_lossy(), extension));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn executable_extensions(path: &Path) -> Vec<String> {
    if path.extension().is_some() {
        return vec![String::new()];
    }

    env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|extension| !extension.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| vec![".COM".to_owned(), ".EXE".to_owned(), ".BAT".to_owned()])
}

#[cfg(unix)]
fn current_user_login_shell() -> Option<PathBuf> {
    let uid = getuid().as_raw();
    fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| passwd_shell_for_uid(line, uid))
}

#[cfg(windows)]
fn current_user_login_shell() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn passwd_shell_for_uid(line: &str, uid: u32) -> Option<PathBuf> {
    let mut fields = line.split(':');
    let _name = fields.next()?;
    let _password = fields.next()?;
    let parsed_uid = fields.next()?.parse::<u32>().ok()?;
    let _gid = fields.next()?;
    let _gecos = fields.next()?;
    let _home = fields.next()?;
    let shell = fields.next()?;
    (parsed_uid == uid && !shell.is_empty()).then(|| PathBuf::from(shell))
}

#[cfg(unix)]
fn default_shell_path() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn default_shell_path() -> PathBuf {
    std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
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
#[path = "terminal/tests.rs"]
mod tests;
