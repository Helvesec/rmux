//! Attached launch: runs Claude inside a private RMUX session and shows teammate panes.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{self, Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use super::claude_program::windows_git_bash_path;
#[cfg(windows)]
use super::runtime_dir::cleanup_stale_windows_claude_temp_dirs;
use super::runtime_dir::create_runtime_dir;
use super::{
    ClaudeInvocation, ExitFailure, DIRECT_LAUNCH_ENV, INTERNAL_RUNNER_COMMAND, MAIN_SESSION,
    MAIN_WINDOW, OUTER_MUX_ENV, SWARM_SESSION, VIEWER_WAIT_TIMEOUT,
};

#[cfg(any(unix, windows))]
pub(super) fn should_launch_attached() -> bool {
    #[cfg(windows)]
    let stdin_is_msys_pty = stdin_is_msys_pty();
    #[cfg(not(windows))]
    let stdin_is_msys_pty = false;
    launch_attached_decision(
        env::var_os(DIRECT_LAUNCH_ENV).is_some(),
        io::stdin().is_terminal(),
        stdin_is_msys_pty,
    )
}

#[cfg(any(unix, windows))]
pub(super) fn launch_attached_decision(
    direct_launch_requested: bool,
    stdin_is_terminal: bool,
    stdin_is_msys_pty: bool,
) -> bool {
    !direct_launch_requested && stdin_is_terminal && !stdin_is_msys_pty
}

#[cfg(any(unix, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DirectLaunchNotice {
    NonTerminal,
    MsysPty,
}

#[cfg(any(unix, windows))]
pub(super) fn direct_launch_notice(
    direct_launch_requested: bool,
    stderr_is_terminal: bool,
    stdin_is_msys_pty: bool,
    stderr_is_msys_pty: bool,
) -> Option<DirectLaunchNotice> {
    if direct_launch_requested || (!stderr_is_terminal && !stderr_is_msys_pty) {
        return None;
    }
    Some(if stdin_is_msys_pty {
        DirectLaunchNotice::MsysPty
    } else {
        DirectLaunchNotice::NonTerminal
    })
}

/// Explains a fall-through to the direct launch path when the user did not
/// ask for it. Claude's interactive UI needs a terminal; reaching this point
/// means stdin was not recognized as one (issue #77: Git Bash/mintty ptys,
/// VS Code tasks, plain pipes), and launching silently used to look like
/// "rmux claude does nothing".
#[cfg(any(unix, windows))]
pub(super) fn report_unrequested_direct_launch() {
    #[cfg(windows)]
    let stdin_is_msys_pty = stdin_is_msys_pty();
    #[cfg(windows)]
    let stderr_is_msys_pty = stderr_is_msys_pty();
    #[cfg(not(windows))]
    let stdin_is_msys_pty = false;
    #[cfg(not(windows))]
    let stderr_is_msys_pty = false;
    let notice = direct_launch_notice(
        env::var_os(DIRECT_LAUNCH_ENV).is_some(),
        io::stderr().is_terminal(),
        stdin_is_msys_pty,
        stderr_is_msys_pty,
    );
    match notice {
        Some(DirectLaunchNotice::MsysPty) => eprintln!(
            "rmux claude: stdin is a Git Bash / MSYS pty, not a Windows console; \
             the interactive claude UI cannot attach here. Run `rmux claude` from \
             Windows Terminal, PowerShell, or cmd.exe (or prefix with `winpty`); \
             set {DIRECT_LAUNCH_ENV}=1 to launch claude directly without a terminal."
        ),
        Some(DirectLaunchNotice::NonTerminal) => eprintln!(
            "rmux claude: stdin is not a terminal, so the interactive UI is skipped \
             and claude is launched directly; run from a terminal, or set \
             {DIRECT_LAUNCH_ENV}=1 to make the direct launch explicit."
        ),
        None => {}
    }
}

#[cfg(windows)]
fn stdin_is_msys_pty() -> bool {
    use std::os::windows::io::AsRawHandle;

    crate::windows_terminal::handle_is_msys_pty(io::stdin().as_raw_handle())
}

#[cfg(windows)]
fn stderr_is_msys_pty() -> bool {
    use std::os::windows::io::AsRawHandle;

    crate::windows_terminal::handle_is_msys_pty(io::stderr().as_raw_handle())
}

#[cfg(any(unix, windows))]
pub(super) fn run_attached(invocation: ClaudeInvocation) -> Result<i32, ExitFailure> {
    #[cfg(windows)]
    cleanup_stale_windows_claude_temp_dirs();
    let binary = env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve current rmux binary: {error}"),
        )
    })?;
    let runtime_dir = create_runtime_dir("rmux-claude")?;
    let main_socket = runtime_dir
        .file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rmux-claude-{}", process::id()));
    let pid_file = runtime_dir.join("claude.pid");

    if let Err(error) = start_main_session(&binary, &main_socket, &pid_file, invocation.args) {
        #[cfg(windows)]
        let _ = shutdown_private_server(&binary, &main_socket);
        let _ = fs::remove_dir_all(runtime_dir);
        return Err(error);
    }

    let monitor_running = Arc::new(AtomicBool::new(true));
    let monitor = spawn_teammate_viewer_monitor(ViewerMonitor {
        binary: binary.clone(),
        main_socket: main_socket.clone(),
        pid_file: pid_file.clone(),
        running: Arc::clone(&monitor_running),
    });

    let status = private_rmux_command(&binary)
        .arg("-L")
        .arg(&main_socket)
        .arg("attach-session")
        .arg("-t")
        .arg(MAIN_SESSION)
        .status()
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("rmux claude: failed to attach RMUX session: {error}"),
            )
        });

    monitor_running.store(false, Ordering::Relaxed);
    let _ = monitor.join();
    let _ = fs::remove_dir_all(runtime_dir);

    let status = status?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(windows)]
fn shutdown_private_server(binary: &Path, main_socket: &str) -> Result<(), ExitFailure> {
    let status = private_rmux_command(binary)
        .arg("-L")
        .arg(main_socket)
        .arg("kill-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("rmux claude: failed to stop private RMUX server: {error}"),
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(ExitFailure::new(
            status.code().unwrap_or(1),
            "rmux claude: failed to stop private RMUX server",
        ))
    }
}

#[cfg(any(unix, windows))]
fn start_main_session(
    binary: &Path,
    main_socket: &str,
    pid_file: &Path,
    claude_args: Vec<OsString>,
) -> Result<(), ExitFailure> {
    let mut runner_args = vec![
        binary.as_os_str().to_os_string(),
        OsString::from(INTERNAL_RUNNER_COMMAND),
        pid_file.as_os_str().to_os_string(),
        OsString::from(main_socket),
        OsString::from("--"),
    ];
    runner_args.extend(claude_args);

    let mut command = private_rmux_command(binary);
    command.arg("-L").arg(main_socket);
    #[cfg(windows)]
    {
        // `start-server` applies `exit-empty` before its standalone process
        // returns. Keep startup, shell configuration, and the first live
        // session in one queue so that cleanup observes the session without
        // overriding the user's `exit-empty` setting.
        let bash = windows_git_bash_path()?;
        command
            .arg("start-server")
            .arg(";")
            .arg("set-option")
            .arg("-g")
            .arg("default-shell")
            .arg(bash)
            .arg(";");
    }
    command
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(MAIN_SESSION)
        .arg("-n")
        .arg(MAIN_WINDOW);
    if let Ok(cwd) = env::current_dir() {
        command.arg("-c").arg(cwd);
    }
    append_runner_command(&mut command, &runner_args)?;
    let status = command.status().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to create attached RMUX session: {error}"),
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(ExitFailure::new(
            status.code().unwrap_or(1),
            "rmux claude: failed to create attached RMUX session",
        ))
    }
}

#[cfg(unix)]
fn append_runner_command(
    command: &mut ProcessCommand,
    runner_args: &[OsString],
) -> Result<(), ExitFailure> {
    command.arg(shell_command(runner_args)?);
    Ok(())
}

#[cfg(windows)]
fn append_runner_command(
    command: &mut ProcessCommand,
    runner_args: &[OsString],
) -> Result<(), ExitFailure> {
    command.args(runner_args);
    Ok(())
}

#[cfg(any(unix, windows))]
struct ViewerMonitor {
    binary: PathBuf,
    main_socket: String,
    pid_file: PathBuf,
    running: Arc<AtomicBool>,
}

#[cfg(any(unix, windows))]
fn spawn_teammate_viewer_monitor(context: ViewerMonitor) -> thread::JoinHandle<()> {
    thread::spawn(move || run_teammate_viewer_monitor(context))
}

#[cfg(any(unix, windows))]
fn run_teammate_viewer_monitor(context: ViewerMonitor) {
    let deadline = Instant::now() + VIEWER_WAIT_TIMEOUT;
    if wait_for_runner_pid(&context.pid_file, deadline, &context.running).is_none() {
        return;
    }
    while context.running.load(Ordering::Relaxed) && Instant::now() < deadline {
        if swarm_session_has_panes(&context.binary, &context.main_socket)
            && show_teammate_window(&context.binary, &context.main_socket).is_ok()
        {
            return;
        }
        thread::sleep(Duration::from_millis(300));
    }
}

#[cfg(any(unix, windows))]
fn wait_for_runner_pid(pid_file: &Path, deadline: Instant, running: &AtomicBool) -> Option<String> {
    while running.load(Ordering::Relaxed) && Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(pid_file) {
            let pid = contents.trim();
            if !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()) {
                return Some(pid.to_string());
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

#[cfg(any(unix, windows))]
fn swarm_session_has_panes(binary: &Path, main_socket: &str) -> bool {
    let Ok(output) = private_rmux_command(binary)
        .arg("-L")
        .arg(main_socket)
        .arg("list-panes")
        .arg("-t")
        .arg(SWARM_SESSION)
        .arg("-s")
        .arg("-F")
        .arg("#{pane_id}")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
}

#[cfg(any(unix, windows))]
fn show_teammate_window(binary: &Path, main_socket: &str) -> Result<(), ExitFailure> {
    let main_window = format!("{MAIN_SESSION}:0");
    let teammate_window = format!("{MAIN_SESSION}:1");
    let link_status = private_rmux_command(binary)
        .arg("-L")
        .arg(main_socket)
        .arg("link-window")
        .arg("-a")
        .arg("-s")
        .arg(format!("{SWARM_SESSION}:0"))
        .arg("-t")
        .arg(&main_window)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !link_status.is_ok_and(|status| status.success()) {
        return Err(ExitFailure::new(
            1,
            "rmux claude: failed to link teammate window",
        ));
    }

    let select_status = private_rmux_command(binary)
        .arg("-L")
        .arg(main_socket)
        .arg("select-window")
        .arg("-t")
        .arg(&teammate_window)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if select_status.is_ok_and(|status| status.success()) {
        Ok(())
    } else {
        Err(ExitFailure::new(
            1,
            "rmux claude: failed to select teammate window",
        ))
    }
}

#[cfg(any(unix, windows))]
fn private_rmux_command(binary: &Path) -> ProcessCommand {
    let mut command = ProcessCommand::new(binary);
    clear_outer_mux_environment(&mut command);
    command
}

#[cfg(any(unix, windows))]
fn clear_outer_mux_environment(command: &mut ProcessCommand) {
    for name in OUTER_MUX_ENV {
        command.env_remove(name);
    }
}

#[cfg(unix)]
fn shell_command(args: &[OsString]) -> Result<String, ExitFailure> {
    let mut command = String::new();
    for arg in args {
        if !command.is_empty() {
            command.push(' ');
        }
        command.push_str(&shell_quote(arg.as_os_str())?);
    }
    Ok(command)
}

#[cfg(unix)]
fn shell_quote(value: &OsStr) -> Result<String, ExitFailure> {
    let value = value.to_str().ok_or_else(|| {
        ExitFailure::new(
            1,
            "rmux claude: attached launcher requires UTF-8 command arguments",
        )
    })?;
    if value.is_empty() {
        return Ok("''".to_string());
    }
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'=' | b',' | b'%')
    }) {
        return Ok(value.to_string());
    }
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}
