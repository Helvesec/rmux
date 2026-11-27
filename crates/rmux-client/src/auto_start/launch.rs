//! Hidden daemon binary resolution and process launch.

use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::time::Duration;

#[cfg(not(any(unix, windows)))]
use super::AutoStartError;
use super::{AutoStartConfig, INTERNAL_DAEMON_FLAG};

#[cfg(windows)]
const STARTUP_READY_EVENT_TIMEOUT: Duration = Duration::from_secs(2);

const BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_BINARY_PATH";
const BINARY_OVERRIDE_TEST_OPT_IN_ENV: &str = "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE";

#[cfg(not(any(unix, windows)))]
pub(super) fn launch_hidden_daemon(
    socket_path: &Path,
    config: &AutoStartConfig,
) -> Result<(), AutoStartError> {
    let binary_path = rmux_binary_path(config).map_err(AutoStartError::BinaryPath)?;
    spawn_hidden_daemon_for(&binary_path, socket_path, config).map_err(|error| {
        AutoStartError::Launch {
            path: binary_path,
            error,
        }
    })
}

#[cfg(not(unix))]
pub(super) fn spawn_hidden_daemon_for(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> io::Result<()> {
    let verified = rmux_os::build_identity::resolve(binary_path.as_os_str())?;
    spawn_hidden_daemon_for_polling(verified.as_path(), socket_path, config)
}

#[cfg(not(unix))]
fn spawn_hidden_daemon_for_polling(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> io::Result<()> {
    #[cfg(windows)]
    {
        spawn_hidden_daemon_for_windows(binary_path, socket_path, config)
    }

    #[cfg(not(windows))]
    {
        let command = hidden_daemon_command(binary_path, socket_path, config, true);
        spawn_hidden_daemon(command)
    }
}

#[cfg(windows)]
fn spawn_hidden_daemon_for_windows(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> io::Result<()> {
    let ready = rmux_os::daemon::StartupReadyEvent::new()?;
    let mut command = hidden_daemon_command(binary_path, socket_path, config, true);
    append_startup_ready_event(&mut command, &ready);
    spawn_hidden_daemon(command)?;
    let _ = ready.wait(STARTUP_READY_EVENT_TIMEOUT);
    Ok(())
}

#[cfg(windows)]
fn append_startup_ready_event(command: &mut Command, ready: &rmux_os::daemon::StartupReadyEvent) {
    command.arg("--startup-ready-event").arg(ready.name());
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn hidden_daemon_command_preserving_fd(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
    allow_job_breakaway: bool,
    preserved_fd: i32,
) -> Command {
    let mut command = hidden_daemon_command_base(binary_path, socket_path, config);
    rmux_os::daemon::configure_hidden_daemon_command_preserving_fds(
        &mut command,
        allow_job_breakaway,
        &[preserved_fd],
    );
    command
}

#[cfg(any(not(unix), all(test, not(target_os = "linux"))))]
pub(super) fn hidden_daemon_command(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
    allow_job_breakaway: bool,
) -> Command {
    let mut command = hidden_daemon_command_base(binary_path, socket_path, config);
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, allow_job_breakaway);
    command
}

pub(super) fn hidden_daemon_command_base(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> Command {
    let mut command = Command::new(binary_path);
    command
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    config.append_hidden_daemon_args(&mut command);
    command
}

#[cfg(windows)]
pub(super) fn spawn_hidden_daemon(command: Command) -> io::Result<()> {
    let child = rmux_os::daemon::launch_hidden_daemon(command.get_program(), command.get_args())?;
    drop(child);
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn spawn_hidden_daemon(mut command: Command) -> io::Result<()> {
    let child = rmux_os::daemon::spawn_hidden_daemon_command_requiring_job_breakaway(&mut command)?;
    // Intentionally drop without `wait()`: the daemon must outlive the
    // short-lived client process that launched it.
    drop(child);
    Ok(())
}

pub(super) fn rmux_binary_path(config: &AutoStartConfig) -> io::Result<PathBuf> {
    if let Some(path) = &config.binary_override {
        return Ok(path.clone());
    }

    let current_exe = env::current_exe()?;
    let resolved_exe = std::fs::canonicalize(&current_exe).ok();
    match env::var_os(BINARY_OVERRIDE_ENV).filter(|_| binary_override_enabled_for_tests()) {
        Some(path) => Ok(PathBuf::from(path)),
        None => Ok(hidden_daemon_binary_path_for_executable_paths(
            &current_exe,
            resolved_exe.as_deref(),
            config,
        )
        .unwrap_or(current_exe)),
    }
}

fn binary_override_enabled_for_tests() -> bool {
    cfg!(debug_assertions)
        && env::var_os(BINARY_OVERRIDE_TEST_OPT_IN_ENV).is_some_and(|value| value == "1")
}

#[cfg(all(test, unix))]
pub(super) fn hidden_daemon_binary_path(current_exe: &Path) -> Option<PathBuf> {
    hidden_daemon_binary_path_for_executable_paths(current_exe, None, &AutoStartConfig::disabled())
}

pub(super) fn hidden_daemon_binary_path_for_executable_paths(
    current_exe: &Path,
    resolved_exe: Option<&Path>,
    config: &AutoStartConfig,
) -> Option<PathBuf> {
    hidden_daemon_binary_path_for_config(current_exe, config).or_else(|| {
        resolved_exe.and_then(|path| hidden_daemon_binary_path_for_config(path, config))
    })
}

pub(super) fn hidden_daemon_binary_path_for_config(
    current_exe: &Path,
    config: &AutoStartConfig,
) -> Option<PathBuf> {
    if config.web_required {
        return None;
    }
    let file_stem = current_exe.file_stem()?.to_str()?;
    if file_stem == "rmux-daemon" {
        return None;
    }

    let mut candidate = current_exe.to_path_buf();
    let daemon_file_name = match current_exe
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension) if !extension.is_empty() => format!("rmux-daemon.{extension}"),
        _ => "rmux-daemon".to_owned(),
    };
    candidate.set_file_name(daemon_file_name);
    candidate.is_file().then_some(candidate)
}
