use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use rmux_core::{EnvironmentStore, OptionStore, PaneId};
use rmux_proto::{AttachShellCommand, OptionName, ProcessCommand, RmuxError, SessionName};
use rmux_pty::{ChildCommand, PtyChild, PtyMaster, TerminalSize as PtyTerminalSize};
use tokio::runtime::Handle;

mod shell_resolver;
mod shell_spec;

#[cfg(windows)]
use shell_resolver::cmd_shell_path;
#[cfg(unix)]
pub(crate) use shell_resolver::is_suitable_shell;
#[cfg(windows)]
use shell_resolver::CLIENT_SHELL_ENV;
use shell_resolver::{resolve_program_path_from, resolve_shell_path};
use shell_spec::ShellSpec;

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

impl SessionBaseEnvironment {
    pub(crate) fn from_profile(profile: &TerminalProfile) -> Self {
        Self {
            raw_environment: profile.raw_environment.as_ref().clone(),
        }
    }

    fn environment_map(&self) -> HashMap<String, String> {
        environment_from_os_pairs(self.raw_environment.iter().cloned())
    }

    fn raw_environment(&self) -> &[(OsString, OsString)] {
        &self.raw_environment
    }
}

impl TerminalProfile {
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_session(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: &SessionName,
        session_id: u32,
        socket_path: &Path,
        spawn_environment: Option<&HashMap<String, String>>,
        include_terminal_defaults: bool,
        overrides: Option<&[String]>,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        Self::for_session_with_environment(
            environment,
            options,
            session_name,
            session_id,
            socket_path,
            None,
            spawn_environment,
            None,
            include_terminal_defaults,
            overrides,
            pane_id,
            requested_cwd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_session_with_base_environment(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: &SessionName,
        session_id: u32,
        socket_path: &Path,
        base_environment: Option<&SessionBaseEnvironment>,
        spawn_environment: Option<&HashMap<String, String>>,
        include_terminal_defaults: bool,
        overrides: Option<&[String]>,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        let base_environment_map = base_environment.map(SessionBaseEnvironment::environment_map);
        Self::for_session_with_environment(
            environment,
            options,
            session_name,
            session_id,
            socket_path,
            base_environment_map.as_ref(),
            spawn_environment,
            base_environment.map(SessionBaseEnvironment::raw_environment),
            include_terminal_defaults,
            overrides,
            pane_id,
            requested_cwd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_initial_session_pane(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: &SessionName,
        session_id: u32,
        socket_path: &Path,
        spawn_environment: Option<&HashMap<String, String>>,
        raw_base_environment: Option<&[(OsString, OsString)]>,
        include_terminal_defaults: bool,
        overrides: Option<&[String]>,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        Self::for_session_with_environment(
            environment,
            options,
            session_name,
            session_id,
            socket_path,
            spawn_environment,
            None,
            raw_base_environment,
            include_terminal_defaults,
            overrides,
            pane_id,
            requested_cwd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn for_session_with_environment(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: &SessionName,
        session_id: u32,
        socket_path: &Path,
        base_environment: Option<&HashMap<String, String>>,
        spawn_environment: Option<&HashMap<String, String>>,
        raw_base_environment: Option<&[(OsString, OsString)]>,
        include_terminal_defaults: bool,
        overrides: Option<&[String]>,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        let mut resolved = base_environment
            .cloned()
            .unwrap_or_else(base_process_environment);
        let include_implicit_globals = base_environment.is_none();
        if base_environment.is_some() {
            environment.apply_to_process_environment_without_implicit_globals(
                Some(session_name),
                &mut resolved,
            );
        } else {
            environment.apply_to_process_environment(Some(session_name), &mut resolved);
        }
        if let Some(spawn_environment) = spawn_environment {
            for (name, value) in spawn_environment {
                set_environment_value(&mut resolved, name.clone(), value.clone());
            }
        }

        Self::from_resolved_environment(
            resolved,
            raw_base_environment,
            environment
                .suppressed_process_environment_names(Some(session_name), include_implicit_globals),
            options,
            session_name,
            session_id,
            socket_path,
            include_terminal_defaults,
            overrides,
            pane_id,
            requested_cwd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_resolved_environment(
        mut resolved: HashMap<String, String>,
        raw_base_environment: Option<&[(OsString, OsString)]>,
        mut suppressed_raw_names: HashSet<String>,
        options: &OptionStore,
        session_name: &SessionName,
        session_id: u32,
        socket_path: &Path,
        include_terminal_defaults: bool,
        overrides: Option<&[String]>,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        if include_terminal_defaults {
            if let Some(default_terminal) = options
                .resolve(Some(session_name), OptionName::DefaultTerminal)
                .or_else(|| options.resolve(None, OptionName::DefaultTerminal))
            {
                set_environment_value(
                    &mut resolved,
                    "TERM".to_owned(),
                    default_terminal.to_owned(),
                );
            }
            set_environment_value(&mut resolved, "TERM_PROGRAM".to_owned(), "rmux".to_owned());
            set_environment_value(
                &mut resolved,
                "TERM_PROGRAM_VERSION".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            );
        } else {
            remove_environment_value(&mut resolved, "TERM_PROGRAM");
            remove_environment_value(&mut resolved, "TERM_PROGRAM_VERSION");
            suppress_terminal_program_environment(&mut suppressed_raw_names);
        }

        let mux_socket_path = mux_environment_socket_path(socket_path);
        let mux_env = format!(
            "{},{},{}",
            mux_socket_path.display(),
            std::process::id(),
            session_id
        );
        set_environment_value(&mut resolved, "RMUX".to_owned(), mux_env.clone());
        set_environment_value(&mut resolved, "TMUX".to_owned(), mux_env);
        crate::tmux_shim::apply_tmux_shim_environment(&mut resolved, socket_path);

        if let Some(overrides) = overrides {
            for (name, value) in parse_environment_assignments(overrides)? {
                set_environment_value(&mut resolved, name, value);
            }
        }

        let cwd = resolve_working_directory(requested_cwd)?;
        let shell = resolve_shell_path(options, Some(session_name), &resolved);
        let suppressed_raw_names =
            suppress_client_shell_environment(&mut resolved, suppressed_raw_names);
        set_environment_value(
            &mut resolved,
            "SHELL".to_owned(),
            shell.to_string_lossy().into_owned(),
        );

        if let Some(pane_id) = pane_id {
            let pane_env = format!("%{}", pane_id.as_u32());
            set_environment_value(&mut resolved, "RMUX_PANE".to_owned(), pane_env.clone());
            set_environment_value(&mut resolved, "TMUX_PANE".to_owned(), pane_env);
        }

        set_environment_value(
            &mut resolved,
            "PWD".to_owned(),
            cwd.to_string_lossy().into_owned(),
        );

        Ok(Self {
            cwd,
            shell,
            raw_environment: raw_process_environment(
                raw_base_environment,
                &resolved,
                &suppressed_raw_names,
            )
            .into(),
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
        Self::for_run_shell_with_base_environment(
            environment,
            options,
            session_name,
            session_id,
            socket_path,
            None,
            include_terminal_defaults,
            None,
            requested_cwd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_run_shell_with_base_environment(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: Option<&SessionName>,
        session_id: Option<u32>,
        socket_path: &Path,
        base_environment: Option<&SessionBaseEnvironment>,
        include_terminal_defaults: bool,
        pane_id: Option<PaneId>,
        requested_cwd: Option<&Path>,
    ) -> Result<Self, RmuxError> {
        let mut resolved = base_environment
            .map(SessionBaseEnvironment::environment_map)
            .unwrap_or_else(base_process_environment);
        let include_implicit_globals = base_environment.is_none();
        if base_environment.is_some() {
            environment
                .apply_to_process_environment_without_implicit_globals(session_name, &mut resolved);
        } else {
            environment.apply_to_process_environment(session_name, &mut resolved);
        }
        remove_environment_value(&mut resolved, "RMUX_PANE");
        remove_environment_value(&mut resolved, "TMUX_PANE");

        let mut suppressed = environment
            .suppressed_process_environment_names(session_name, include_implicit_globals);

        if include_terminal_defaults {
            if let Some(default_terminal) = session_name
                .and_then(|session_name| {
                    options.resolve(Some(session_name), OptionName::DefaultTerminal)
                })
                .or_else(|| options.resolve(None, OptionName::DefaultTerminal))
            {
                set_environment_value(
                    &mut resolved,
                    "TERM".to_owned(),
                    default_terminal.to_owned(),
                );
            }
            set_environment_value(&mut resolved, "TERM_PROGRAM".to_owned(), "rmux".to_owned());
            set_environment_value(
                &mut resolved,
                "TERM_PROGRAM_VERSION".to_owned(),
                env!("CARGO_PKG_VERSION").to_owned(),
            );
        } else {
            remove_environment_value(&mut resolved, "TERM_PROGRAM");
            remove_environment_value(&mut resolved, "TERM_PROGRAM_VERSION");
            suppress_terminal_program_environment(&mut suppressed);
        }

        let mux_session_id = session_id.map_or(0_i32, |id| i32::try_from(id).unwrap_or(i32::MAX));
        let mux_socket_path = mux_environment_socket_path(socket_path);
        let mux_env = format!(
            "{},{},{}",
            mux_socket_path.display(),
            std::process::id(),
            mux_session_id
        );
        set_environment_value(&mut resolved, "RMUX".to_owned(), mux_env.clone());
        set_environment_value(&mut resolved, "TMUX".to_owned(), mux_env);
        crate::tmux_shim::apply_tmux_shim_environment(&mut resolved, socket_path);
        if let Some(pane_id) = pane_id {
            let pane_env = format!("%{}", pane_id.as_u32());
            set_environment_value(&mut resolved, "RMUX_PANE".to_owned(), pane_env.clone());
            set_environment_value(&mut resolved, "TMUX_PANE".to_owned(), pane_env);
        }

        let cwd = resolve_working_directory(requested_cwd)?;
        let shell = resolve_shell_path(options, session_name, &resolved);
        #[cfg(windows)]
        {
            remove_environment_value(&mut resolved, CLIENT_SHELL_ENV);
        }
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

        suppressed.insert("RMUX_PANE".to_owned());
        suppressed.insert("TMUX_PANE".to_owned());
        #[cfg(windows)]
        suppressed.insert(CLIENT_SHELL_ENV.to_owned());
        Ok(Self {
            cwd,
            shell,
            raw_environment: raw_process_environment(
                base_environment.map(SessionBaseEnvironment::raw_environment),
                &resolved,
                &suppressed,
            )
            .into(),
        })
    }

    pub(crate) fn environment(&self) -> impl Iterator<Item = (&str, &str)> {
        self.raw_environment
            .iter()
            .filter_map(|(name, value)| Some((name.to_str()?, value.to_str()?)))
    }

    pub(crate) fn raw_environment(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.raw_environment
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str()))
    }

    pub(crate) fn with_source_depth(mut self, depth: usize) -> Self {
        let value = depth.to_string();
        set_raw_environment_value(
            Arc::make_mut(&mut self.raw_environment),
            OsString::from("RMUX_SOURCE_DEPTH"),
            OsString::from(value),
        );
        self
    }

    pub(crate) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn shell(&self) -> &Path {
        &self.shell
    }

    /// Reuses the shell resolved for an existing pane when that pane is
    /// respawned. tmux binds a pane to its original shell even when the
    /// `default-shell` option changes later; keeping `SHELL` aligned with the
    /// executable avoids exposing a contradictory child environment.
    pub(crate) fn with_respawn_shell(mut self, shell: PathBuf) -> Self {
        set_raw_environment_value(
            Arc::make_mut(&mut self.raw_environment),
            OsString::from("SHELL"),
            shell.as_os_str().to_os_string(),
        );
        self.shell = shell;
        self
    }

    pub(crate) fn resolved_default_shell(
        environment: &EnvironmentStore,
        options: &OptionStore,
        session_name: Option<&SessionName>,
    ) -> PathBuf {
        let mut resolved = base_process_environment();
        environment.apply_to_process_environment(session_name, &mut resolved);
        resolve_shell_path(options, session_name, &resolved)
    }

    pub(crate) fn shell_std_command(&self, command: &str) -> Command {
        shell_std_command(&self.shell, &self.cwd, command)
    }

    pub(crate) fn attach_shell_command(&self, command: String) -> AttachShellCommand {
        AttachShellCommand::new(
            command,
            self.shell.to_string_lossy().into_owned(),
            self.cwd.to_string_lossy().into_owned(),
        )
    }

    pub(crate) fn shell_child_command(&self, command: &str) -> ChildCommand {
        shell_child_command(&self.shell, &self.cwd, command)
    }

    pub(crate) fn interactive_child_command(&self) -> ChildCommand {
        ShellSpec::new(&self.shell).interactive_child(&self.cwd)
    }

    pub(crate) fn environment_value(&self, name: &str) -> Option<&str> {
        self.raw_environment
            .iter()
            .find(|(candidate, _)| os_environment_name_eq(candidate, name))
            .and_then(|(_, value)| value.to_str())
    }

    #[cfg(test)]
    pub(crate) fn with_test_environment(mut self, environment: HashMap<String, String>) -> Self {
        for (name, value) in environment {
            set_raw_environment_value(
                Arc::make_mut(&mut self.raw_environment),
                OsString::from(name),
                OsString::from(value),
            );
        }
        self
    }

    pub(crate) fn default_window_name(&self) -> Option<String> {
        self.environment_value("TERM_PROGRAM")
            .filter(|value| *value == "rmux")
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| shell_program_name(&self.shell))
    }

    pub(crate) fn initial_pane_title(&self) -> Option<String> {
        let host = crate::host_name::local_hostname()?;
        Some(host.split('.').next().unwrap_or(&host).to_owned())
    }

    pub(crate) fn automatic_window_name(&self, command: Option<&ProcessCommand>) -> Option<String> {
        if command.is_some() {
            self.runtime_window_name(command)
        } else {
            self.default_window_name()
        }
    }

    pub(crate) fn runtime_window_name(&self, command: Option<&ProcessCommand>) -> Option<String> {
        match command {
            Some(ProcessCommand::Shell(command)) => {
                shell_command_window_name(command).or_else(|| shell_program_name(&self.shell))
            }
            Some(ProcessCommand::Argv(argv)) if !argv.is_empty() => executable_name(&argv[0]),
            None => shell_program_name(&self.shell),
            Some(ProcessCommand::Argv(_)) | Some(_) => shell_program_name(&self.shell),
        }
    }

    fn environment_map(&self) -> HashMap<String, String> {
        environment_from_os_pairs(self.raw_environment.iter().cloned())
    }
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
    let first = command.split_whitespace().next()?;
    executable_name(first)
}

pub(crate) fn spawn_pane_process(
    size: PtyTerminalSize,
    profile: &TerminalProfile,
    command: Option<&ProcessCommand>,
) -> Result<(PtyMaster, PtyChild), RmuxError> {
    validate_process_command(command)?;
    let mut command = spawn_command(profile, command)
        .size(size)
        .clear_env()
        .current_dir(profile.cwd());

    for (name, value) in profile.raw_environment() {
        command = command.env(name, value);
    }

    let spawned = command.spawn().map_err(|error| {
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
    Ok(())
}

fn spawn_command(profile: &TerminalProfile, command: Option<&ProcessCommand>) -> ChildCommand {
    match command {
        Some(ProcessCommand::Shell(command)) => profile.shell_child_command(command),
        Some(ProcessCommand::Argv(argv)) if !argv.is_empty() => {
            let environment = profile.environment_map();
            let program =
                resolve_program_path_from(Path::new(&argv[0]), &environment, profile.cwd());
            #[cfg(windows)]
            if let Some(command) = windows_batch_child_command(&program, &argv[1..], &environment) {
                return command;
            }
            ChildCommand::new(program).args(&argv[1..])
        }
        Some(ProcessCommand::Argv(_)) | Some(_) | None => profile.interactive_child_command(),
    }
}

#[cfg(windows)]
fn windows_batch_child_command(
    program: &Path,
    args: &[String],
    environment: &HashMap<String, String>,
) -> Option<ChildCommand> {
    if !is_windows_batch_script(program) {
        return None;
    }

    let shell = cmd_shell_path(environment).unwrap_or_else(|| PathBuf::from("cmd.exe"));
    Some(
        ChildCommand::new(shell)
            .arg("/D")
            .arg("/S")
            .arg("/C")
            .windows_verbatim_args(windows_batch_cmd_tail(program, args)),
    )
}

#[cfg(windows)]
fn windows_batch_cmd_tail(program: &Path, args: &[String]) -> OsString {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(cmd_double_quoted(&program.to_string_lossy()));
    parts.extend(args.iter().map(|arg| cmd_double_quoted(arg)));
    format!("\"{}\"", parts.join(" ")).into()
}

#[cfg(windows)]
fn cmd_double_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(windows)]
fn is_windows_batch_script(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "bat" | "cmd"))
        .unwrap_or(false)
}

pub(crate) fn shell_child_command(shell: &Path, cwd: &Path, command: &str) -> ChildCommand {
    ShellSpec::new(shell).command_child(cwd, command)
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
