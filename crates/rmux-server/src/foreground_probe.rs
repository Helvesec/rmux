//! Best-effort pane foreground process probing.

use rmux_core::PaneId;
use rmux_os::process;
use rmux_proto::{
    ForegroundFieldSource, ForegroundSourcesDto, ForegroundStateDto, PaneTarget, RmuxError,
};

use crate::format_runtime::pane_path_from_osc7;
use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForegroundProbeSeed {
    pane_id: PaneId,
    generation: u64,
    root_pid: Option<u32>,
    foreground_pid: Option<u32>,
    runtime_name: Option<String>,
    shell_name: Option<String>,
    profile_cwd: Option<String>,
    osc7_path: Option<String>,
    env_pwd: Option<String>,
    env_home: Option<String>,
    env_userprofile: Option<String>,
}

impl ForegroundProbeSeed {
    pub(crate) const fn pane_id(&self) -> PaneId {
        self.pane_id
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }
}

pub(crate) fn capture_foreground_probe_seed(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<ForegroundProbeSeed, RmuxError> {
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })?;
    let generation = state.pane_output_generation(target.session_name(), pane_id);
    let root_pid = state
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok();
    let foreground_pid = foreground_pid_for_target(state, target, root_pid);
    let runtime_name = state
        .pane_runtime_window_name_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .flatten();
    let profile = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok();
    let shell_name = profile.and_then(|profile| {
        profile
            .shell()
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
    });
    let profile_cwd = state
        .pane_profile_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .map(|profile| profile.cwd().to_string_lossy().into_owned());
    let osc7_path = state
        .pane_screen_state(target.session_name(), pane_id)
        .and_then(|screen| pane_path_from_osc7(&screen.path));
    let env_pwd = state
        .environment
        .resolve(Some(target.session_name()), "PWD")
        .map(str::to_owned);
    let env_home = state
        .environment
        .resolve(Some(target.session_name()), "HOME")
        .map(str::to_owned);
    let env_userprofile = state
        .environment
        .resolve(Some(target.session_name()), "USERPROFILE")
        .map(str::to_owned);

    Ok(ForegroundProbeSeed {
        pane_id,
        generation,
        root_pid,
        foreground_pid,
        runtime_name,
        shell_name,
        profile_cwd,
        osc7_path,
        env_pwd,
        env_home,
        env_userprofile,
    })
}

pub(crate) fn probe_foreground(seed: &ForegroundProbeSeed) -> ForegroundStateDto {
    let mut sources = ForegroundSourcesDto::default();
    let pid = foreground_pid(seed);
    sources.pid = pid.map(|(_, source)| source);

    let command = foreground_command(seed);
    sources.command = command.as_ref().map(|(_, source)| *source);

    let cwd = foreground_cwd(seed);
    sources.cwd = cwd.as_ref().map(|(_, source)| *source);

    ForegroundStateDto {
        pid: pid.map(|(pid, _)| pid),
        command: command.map(|(command, _)| command),
        cwd: cwd.map(|(cwd, _)| cwd),
        exe: None,
        sources,
    }
}

#[cfg(unix)]
fn foreground_pid_for_target(
    state: &HandlerState,
    target: &PaneTarget,
    root_pid: Option<u32>,
) -> Option<u32> {
    state
        .pane_master_fd(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .ok()
        .and_then(process::unix::foreground_pid)
        .or(root_pid)
}

#[cfg(windows)]
fn foreground_pid_for_target(
    _state: &HandlerState,
    _target: &PaneTarget,
    root_pid: Option<u32>,
) -> Option<u32> {
    root_pid
}

#[cfg(unix)]
fn foreground_pid(seed: &ForegroundProbeSeed) -> Option<(u32, ForegroundFieldSource)> {
    seed.foreground_pid
        .map(|pid| (pid, ForegroundFieldSource::Process))
}

#[cfg(windows)]
fn foreground_pid(seed: &ForegroundProbeSeed) -> Option<(u32, ForegroundFieldSource)> {
    seed.root_pid
        .map(|pid| (pid, ForegroundFieldSource::RootProcess))
}

#[cfg(unix)]
fn foreground_command(seed: &ForegroundProbeSeed) -> Option<(String, ForegroundFieldSource)> {
    let foreground_name = seed.foreground_pid.and_then(process::command_name);
    match (
        foreground_name,
        seed.runtime_name.clone(),
        seed.shell_name.clone(),
    ) {
        (Some(foreground), Some(runtime), Some(shell))
            if foreground == shell && runtime != shell =>
        {
            Some((runtime, ForegroundFieldSource::RuntimeName))
        }
        (Some(foreground), _, _) => Some((foreground, ForegroundFieldSource::Process)),
        (None, Some(runtime), _) => Some((runtime, ForegroundFieldSource::RuntimeName)),
        (None, None, Some(shell)) => Some((shell, ForegroundFieldSource::Profile)),
        (None, None, None) => None,
    }
}

#[cfg(windows)]
fn foreground_command(seed: &ForegroundProbeSeed) -> Option<(String, ForegroundFieldSource)> {
    seed.runtime_name
        .clone()
        .map(|name| (name, ForegroundFieldSource::RuntimeName))
        .or_else(|| {
            seed.root_pid
                .and_then(process::command_name)
                .map(|name| (name, ForegroundFieldSource::RootProcess))
        })
        .or_else(|| {
            seed.shell_name
                .clone()
                .map(|name| (name, ForegroundFieldSource::Profile))
        })
}

#[cfg(unix)]
fn foreground_cwd(seed: &ForegroundProbeSeed) -> Option<(String, ForegroundFieldSource)> {
    seed.foreground_pid
        .and_then(process::current_path)
        .map(|path| (path, ForegroundFieldSource::Process))
        .or_else(|| {
            seed.osc7_path
                .clone()
                .map(|path| (path, ForegroundFieldSource::Osc7))
        })
        .or_else(|| {
            seed.profile_cwd
                .clone()
                .map(|path| (path, ForegroundFieldSource::Profile))
        })
        .or_else(|| {
            seed.env_pwd
                .clone()
                .map(|path| (path, ForegroundFieldSource::Environment))
        })
        .or_else(|| {
            seed.env_home
                .clone()
                .map(|path| (path, ForegroundFieldSource::Environment))
        })
}

#[cfg(windows)]
fn foreground_cwd(seed: &ForegroundProbeSeed) -> Option<(String, ForegroundFieldSource)> {
    seed.osc7_path
        .clone()
        .map(|path| (path, ForegroundFieldSource::Osc7))
        .or_else(|| {
            seed.root_pid
                .and_then(process::current_path)
                .map(|path| (path, ForegroundFieldSource::RootProcess))
        })
        .or_else(|| {
            seed.profile_cwd
                .clone()
                .map(|path| (path, ForegroundFieldSource::Profile))
        })
        .or_else(|| {
            seed.env_pwd
                .clone()
                .map(|path| (path, ForegroundFieldSource::Environment))
        })
        .or_else(|| {
            seed.env_userprofile
                .clone()
                .map(|path| (path, ForegroundFieldSource::Environment))
        })
}
