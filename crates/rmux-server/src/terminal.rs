use std::collections::HashMap;
use std::io;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rmux_core::{EnvironmentStore, OptionStore, PaneId};
use rmux_proto::{OptionName, RmuxError, SessionName};
use rmux_pty::{ChildCommand, PtyChild, PtyMaster, TerminalSize as PtyTerminalSize};
use tokio::runtime::Handle;

mod shell_resolver;
mod shell_spec;

use shell_resolver::{resolve_program_path, resolve_shell_path};
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

        if let Some(overrides) = overrides {
            for (name, value) in parse_environment_assignments(overrides)? {
                set_environment_value(&mut resolved, name, value);
            }
        }

        let cwd = resolve_working_directory(requested_cwd)?;
        let shell = resolve_shell_path(options, Some(session_name), &resolved);
        set_environment_value(
            &mut resolved,
            "SHELL".to_owned(),
            shell.to_string_lossy().into_owned(),
        );

        if let Some(pane_id) = pane_id {
            resolved.insert("RMUX_PANE".to_owned(), format!("%{}", pane_id.as_u32()));
        }

        set_environment_value(
            &mut resolved,
            "PWD".to_owned(),
            cwd.to_string_lossy().into_owned(),
        );

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
        set_environment_value(
            &mut resolved,
            "SHELL".to_owned(),
            shell.to_string_lossy().into_owned(),
        );
        set_environment_value(
            &mut resolved,
            "PWD".to_owned(),
            cwd.to_string_lossy().into_owned(),
        );

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

    pub(crate) fn shell_command(&self, command: &str) -> tokio::process::Command {
        shell_tokio_command(&self.shell, &self.cwd, command)
    }

    pub(crate) fn shell_std_command(&self, command: &str) -> Command {
        shell_std_command(&self.shell, &self.cwd, command)
    }

    pub(crate) fn shell_child_command(&self, command: &str) -> ChildCommand {
        shell_child_command(&self.shell, &self.cwd, command)
    }

    pub(crate) fn interactive_child_command(&self) -> ChildCommand {
        ShellSpec::new(&self.shell).interactive_child(&self.cwd)
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
    match command {
        Some([single]) => profile.shell_child_command(single),
        Some(argv) if !argv.is_empty() => {
            let program = resolve_program_path(Path::new(&argv[0]), &profile.environment);
            ChildCommand::new(program).args(&argv[1..])
        }
        _ => profile.interactive_child_command(),
    }
}

pub(crate) fn shell_child_command(shell: &Path, cwd: &Path, command: &str) -> ChildCommand {
    ShellSpec::new(shell).command_child(cwd, command)
}

pub(crate) fn shell_tokio_command(
    shell: &Path,
    cwd: &Path,
    command: &str,
) -> tokio::process::Command {
    let mut command = ShellSpec::new(shell).command_tokio_child(cwd, command);
    configure_hidden_tokio_helper(&mut command);
    command
}

pub(crate) fn shell_std_command(shell: &Path, cwd: &Path, command: &str) -> Command {
    let mut command = ShellSpec::new(shell).command_std_child(cwd, command);
    configure_hidden_std_helper(&mut command);
    command
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn configure_hidden_tokio_helper(command: &mut tokio::process::Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn configure_hidden_tokio_helper(_command: &mut tokio::process::Command) {}

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
    #[cfg(windows)]
    if let Some(existing) = environment
        .keys()
        .find(|key| key.eq_ignore_ascii_case(&name))
        .cloned()
    {
        environment.remove(&existing);
    }

    environment.insert(name, value);
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
#[path = "terminal/hook_tests.rs"]
mod hook_tests;
#[cfg(test)]
#[path = "terminal/profile_env_tests.rs"]
mod profile_env_tests;
#[cfg(test)]
#[path = "terminal/tests.rs"]
mod tests;
