use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use rmux_core::{EnvironmentStore, OptionStore, PaneId};
use rmux_proto::{AttachShellCommand, OptionName, ProcessCommand, RmuxError, SessionName};
use rmux_pty::ChildCommand;

#[cfg(windows)]
use super::shell_resolver::CLIENT_SHELL_ENV;
use super::shell_resolver::{resolve_profile_shell, resolve_shell_path};
use super::shell_spec::{CommandArgv0, ShellSpec};
use super::{
    base_process_environment, environment_from_os_pairs, executable_name,
    mux_environment_socket_path, parse_environment_assignments, raw_process_environment,
    remove_environment_value, resolve_working_directory, set_environment_value,
    set_raw_environment_value, shell_command_window_name, shell_program_name, shell_std_command,
    suppress_client_shell_environment, suppress_terminal_program_environment,
    SessionBaseEnvironment, TerminalProfile,
};

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
        let shell = resolve_profile_shell(options, Some(session_name), &resolved, &cwd);
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
        let shell = resolve_profile_shell(options, session_name, &resolved, &cwd);
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

    pub(crate) fn quote_shell_path(&self, path: &Path) -> String {
        ShellSpec::new(&self.shell).quote_path(path)
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

    #[cfg(all(test, unix))]
    pub(crate) fn shell_child_command(&self, command: &str) -> ChildCommand {
        ShellSpec::new(&self.shell).named_command_child(&self.cwd, command, CommandArgv0::ShellPath)
    }

    /// A pane command uses the shell's file name as `argv[0]` on Unix, as tmux does.
    pub(super) fn pane_shell_child_command(&self, command: &str) -> ChildCommand {
        ShellSpec::new(&self.shell).named_command_child(&self.cwd, command, CommandArgv0::ShellName)
    }

    /// A display-popup job also uses the shell's file name as `argv[0]` on Unix.
    /// Generic shell-command users keep the shell's path.
    pub(crate) fn popup_job_child_command(&self, command: &str) -> ChildCommand {
        ShellSpec::new(&self.shell).named_command_child(&self.cwd, command, CommandArgv0::ShellName)
    }

    pub(crate) fn interactive_child_command(&self) -> ChildCommand {
        ShellSpec::new(&self.shell).interactive_child(&self.cwd)
    }

    /// A display-popup's shell when the popup has no command.
    pub(crate) fn popup_interactive_child_command(&self) -> ChildCommand {
        ShellSpec::new(&self.shell).popup_interactive_child(&self.cwd)
    }

    #[cfg(test)]
    pub(crate) fn environment_value(&self, name: &str) -> Option<&str> {
        self.raw_environment
            .iter()
            .find(|(candidate, _)| super::os_environment_name_eq(candidate, name))
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

    /// tmux `default_window_name`: the shell's basename, never the server's
    /// own program name, whatever `TERM_PROGRAM` the pane receives.
    pub(crate) fn default_window_name(&self) -> Option<String> {
        shell_program_name(&self.shell)
    }

    pub(crate) fn initial_pane_title(&self) -> Option<String> {
        let host = crate::host_name::local_hostname()?;
        #[cfg(unix)]
        {
            // tmux seeds pane_title with the complete hostname. The status
            // format owns any clipping, independently of host_short.
            Some(host)
        }
        #[cfg(not(unix))]
        {
            Some(host.split('.').next().unwrap_or(&host).to_owned())
        }
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

    pub(super) fn environment_map(&self) -> HashMap<String, String> {
        environment_from_os_pairs(self.raw_environment.iter().cloned())
    }
}
