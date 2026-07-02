use std::env;
#[cfg(any(unix, windows))]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::io::Read;
#[cfg(any(unix, windows))]
use std::io::Write;
#[cfg(any(unix, windows))]
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
#[cfg(any(unix, windows))]
use std::process::Stdio;
use std::process::{self, Command as ProcessCommand};
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::thread;
#[cfg(any(unix, windows))]
use std::time::{Duration, Instant};
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

use super::ExitFailure;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const TEAMMATE_MODE_FLAG: &str = "--teammate-mode";
const TEAMMATE_MODE: &str = "tmux";
const AGENT_TEAMS_ENV: &str = "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS";
const DISABLE_TMUX_SHIM_ENV: &str = "RMUX_DISABLE_TMUX_SHIM";
#[cfg(any(unix, windows))]
const PUBLIC_BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_PUBLIC_BINARY_PATH";
const CLAUDE_MAIN_SOCKET_ENV: &str = "RMUX_INTERNAL_CLAUDE_MAIN_SOCKET";
const CLAUDE_SWARM_SOCKET_ENV: &str = "RMUX_INTERNAL_CLAUDE_SWARM_SOCKET";
#[cfg(windows)]
const CLAUDE_GIT_BASH_PATH_ENV: &str = "CLAUDE_CODE_GIT_BASH_PATH";
#[cfg(any(unix, windows))]
const DIRECT_LAUNCH_ENV: &str = "RMUX_CLAUDE_DIRECT";
#[cfg(any(unix, windows))]
const OUTER_MUX_ENV: &[&str] = &["RMUX", "TMUX", "RMUX_PANE", "TMUX_PANE"];
const INTERNAL_RUNNER_COMMAND: &str = "__rmux-internal-claude-runner";
#[cfg(any(unix, windows))]
const MAIN_SESSION: &str = "rmux-claude";
#[cfg(any(unix, windows))]
const MAIN_WINDOW: &str = "claude";
#[cfg(any(unix, windows))]
const SWARM_SESSION: &str = "claude-swarm";
const SWARM_SOCKET_PREFIX: &str = "claude-swarm-";
#[cfg(any(unix, windows))]
const VIEWER_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaudeInvocation {
    args: Vec<OsString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaudeRunnerInvocation {
    pid_file: PathBuf,
    main_socket: String,
    args: Vec<OsString>,
}

pub(super) fn parse_invocation(arguments: &[OsString]) -> Option<ClaudeInvocation> {
    let command_index = split_top_level_prefix(arguments)?;
    let command = arguments.get(command_index)?.to_str()?;
    (command == "claude").then(|| ClaudeInvocation {
        args: arguments[command_index + 1..].to_vec(),
    })
}

pub(super) fn parse_internal_runner(arguments: &[OsString]) -> Option<ClaudeRunnerInvocation> {
    let command = arguments.first()?.to_str()?;
    if command != INTERNAL_RUNNER_COMMAND {
        return None;
    }
    let pid_file = PathBuf::from(arguments.get(1)?);
    let main_socket = arguments.get(2)?.to_str()?.to_owned();
    let separator = arguments.get(3)?.to_str()?;
    if separator != "--" {
        return None;
    }
    Some(ClaudeRunnerInvocation {
        pid_file,
        main_socket,
        args: arguments[4..].to_vec(),
    })
}

pub(super) fn run(invocation: ClaudeInvocation) -> Result<i32, ExitFailure> {
    #[cfg(any(unix, windows))]
    if should_launch_attached() {
        return run_attached(invocation);
    }
    run_direct(invocation)
}

pub(super) fn run_internal_runner(invocation: ClaudeRunnerInvocation) -> Result<i32, ExitFailure> {
    write_runner_pid_file(&invocation.pid_file)?;
    let (mut command, _shim) = build_claude_command(invocation.args)?;
    command
        .env(CLAUDE_MAIN_SOCKET_ENV, &invocation.main_socket)
        .env(
            CLAUDE_SWARM_SOCKET_ENV,
            format!("{SWARM_SOCKET_PREFIX}{}", process::id()),
        );
    prepare_claude_leader_environment(&mut command, &invocation.main_socket)?;
    run_claude(command)
}

fn prepare_claude_leader_environment(
    command: &mut ProcessCommand,
    main_socket: &str,
) -> Result<(), ExitFailure> {
    prepare_claude_leader_environment_impl(command, main_socket)
}

#[cfg(windows)]
fn prepare_claude_leader_environment_impl(
    command: &mut ProcessCommand,
    main_socket: &str,
) -> Result<(), ExitFailure> {
    let socket_path = rmux_client::socket_path_for_label(main_socket).map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve private tmux socket: {error}"),
        )
    })?;
    let pane = env::var_os("RMUX_PANE")
        .or_else(|| env::var_os("TMUX_PANE"))
        .unwrap_or_else(|| OsString::from("%0"));
    command
        .env("TMUX", format!("{},0,0", socket_path.display()))
        .env("TMUX_PANE", pane);
    Ok(())
}

#[cfg(not(windows))]
fn prepare_claude_leader_environment_impl(
    _command: &mut ProcessCommand,
    _main_socket: &str,
) -> Result<(), ExitFailure> {
    Ok(())
}

fn run_direct(invocation: ClaudeInvocation) -> Result<i32, ExitFailure> {
    let (command, _shim) = build_claude_command(invocation.args)?;
    run_claude(command)
}

fn build_claude_command(
    args: Vec<OsString>,
) -> Result<(ProcessCommand, Option<PrivateTmuxShim>), ExitFailure> {
    let mut command = claude_process_command()?;
    command
        .arg(TEAMMATE_MODE_FLAG)
        .arg(TEAMMATE_MODE)
        .args(args)
        .env(AGENT_TEAMS_ENV, "1");
    let shim = if private_tmux_shim_enabled() {
        let shim = ensure_private_tmux_shim()?;
        let path = path_with_shim_first(shim.path())?;
        set_command_path_with_shim(&mut command, path);
        Some(shim)
    } else {
        None
    };
    configure_claude_process_environment(&mut command)?;
    Ok((command, shim))
}

fn private_tmux_shim_enabled() -> bool {
    !env_flag_enabled(DISABLE_TMUX_SHIM_ENV)
}

fn env_flag_enabled(name: &str) -> bool {
    let Ok(value) = env::var(name) else {
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

#[cfg(windows)]
fn set_command_path_with_shim(command: &mut ProcessCommand, path: OsString) {
    // Claude Code resolves tmux via Node/Bun + where.exe. On Windows those
    // paths observe the canonical `Path` key; setting only `PATH` can leave a
    // stale inherited `Path` visible to that resolver.
    command.env("Path", &path).env("PATH", &path);
    ensure_windows_pathext(command);
}

#[cfg(not(windows))]
fn set_command_path_with_shim(command: &mut ProcessCommand, path: OsString) {
    command.env("PATH", path);
}

#[cfg(windows)]
fn ensure_windows_pathext(command: &mut ProcessCommand) {
    let existing = env::var_os("PATHEXT")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC"));
    let has_exe = existing
        .to_string_lossy()
        .split(';')
        .any(|extension| extension.eq_ignore_ascii_case(".EXE"));
    if has_exe {
        command.env("PATHEXT", existing);
        return;
    }
    let mut updated = existing;
    updated.push(";.EXE");
    command.env("PATHEXT", updated);
}

#[cfg(windows)]
fn configure_claude_process_environment(command: &mut ProcessCommand) -> Result<(), ExitFailure> {
    let bash = windows_git_bash_path()?;
    command
        .env("SHELL", &bash)
        .env(CLAUDE_GIT_BASH_PATH_ENV, &bash);
    Ok(())
}

#[cfg(not(windows))]
fn configure_claude_process_environment(_command: &mut ProcessCommand) -> Result<(), ExitFailure> {
    Ok(())
}

struct PrivateTmuxShim {
    dir: PathBuf,
    cleanup_on_drop: bool,
}

impl PrivateTmuxShim {
    #[cfg(any(unix, windows))]
    fn persistent(dir: PathBuf) -> Self {
        Self {
            dir,
            cleanup_on_drop: false,
        }
    }

    #[cfg(windows)]
    fn temporary(dir: PathBuf) -> Self {
        Self {
            dir,
            cleanup_on_drop: true,
        }
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for PrivateTmuxShim {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            remove_temporary_shim_dir(&self.dir);
        }
    }
}

#[cfg(windows)]
fn remove_temporary_shim_dir(dir: &Path) {
    for attempt in 0..10 {
        match fs::remove_dir_all(dir) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) if attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

#[cfg(not(windows))]
fn remove_temporary_shim_dir(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

#[cfg(any(unix, windows))]
fn should_launch_attached() -> bool {
    env::var_os(DIRECT_LAUNCH_ENV).is_none() && io::stdin().is_terminal()
}

#[cfg(any(unix, windows))]
fn run_attached(invocation: ClaudeInvocation) -> Result<i32, ExitFailure> {
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

    #[cfg(windows)]
    if let Err(error) = prepare_windows_claude_server(&binary, &main_socket) {
        let _ = shutdown_private_server(&binary, &main_socket);
        let _ = fs::remove_dir_all(runtime_dir);
        return Err(error);
    }

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
fn prepare_windows_claude_server(binary: &Path, main_socket: &str) -> Result<(), ExitFailure> {
    start_private_server(binary, main_socket)?;
    configure_windows_claude_default_shell(binary, main_socket)
}

#[cfg(windows)]
fn start_private_server(binary: &Path, main_socket: &str) -> Result<(), ExitFailure> {
    let output = private_rmux_command(binary)
        .arg("-L")
        .arg(main_socket)
        .arg("start-server")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("rmux claude: failed to start private RMUX server: {error}"),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ExitFailure::new(
        output.status.code().unwrap_or(1),
        format!(
            "rmux claude: failed to start private RMUX server{}",
            command_output_suffix(&output)
        ),
    ))
}

#[cfg(windows)]
fn configure_windows_claude_default_shell(
    binary: &Path,
    main_socket: &str,
) -> Result<(), ExitFailure> {
    let bash = windows_git_bash_path()?;
    let output = private_rmux_command(binary)
        .arg("-L")
        .arg(main_socket)
        .arg("set-option")
        .arg("-g")
        .arg("default-shell")
        .arg(&bash)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("rmux claude: failed to configure teammate shell: {error}"),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let _ = shutdown_private_server(binary, main_socket);
    Err(ExitFailure::new(
        output.status.code().unwrap_or(1),
        format!(
            "rmux claude: failed to configure teammate shell '{}'{}",
            bash.display(),
            command_output_suffix(&output)
        ),
    ))
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

#[cfg(windows)]
fn command_output_suffix(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = stdout.trim();
    let stderr = stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(": {stdout}"),
        (true, false) => format!(": {stderr}"),
        (false, false) => format!(": {stdout}; {stderr}"),
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
    command
        .arg("-L")
        .arg(main_socket)
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

fn write_runner_pid_file(pid_file: &Path) -> Result<(), ExitFailure> {
    #[cfg(unix)]
    {
        write_runner_pid_file_unix(pid_file)
    }

    #[cfg(not(unix))]
    {
        if let Some(parent) = pid_file.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create runner pid directory '{}': {error}",
                        parent.display()
                    ),
                )
            })?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(pid_file)
            .map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create runner pid file '{}': {error}",
                        pid_file.display()
                    ),
                )
            })?;
        file.write_all(format!("{}\n", process::id()).as_bytes())
            .map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to write runner pid file '{}': {error}",
                        pid_file.display()
                    ),
                )
            })
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
fn create_runtime_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    create_secure_runtime_dir(prefix)
}

#[cfg(windows)]
fn create_runtime_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    create_windows_temporary_dir(prefix)
}

#[cfg(unix)]
fn write_runner_pid_file_unix(pid_file: &Path) -> Result<(), ExitFailure> {
    if let Some(parent) = pid_file.parent() {
        validate_secure_owner_directory(parent, "Claude runner pid directory")?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(pid_file)
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to create runner pid file '{}': {error}",
                    pid_file.display()
                ),
            )
        })?;
    file.write_all(format!("{}\n", process::id()).as_bytes())
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to write runner pid file '{}': {error}",
                    pid_file.display()
                ),
            )
        })
}

#[cfg(unix)]
fn create_secure_runtime_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    let temp_root = env::temp_dir();
    for _ in 0..64 {
        let suffix = random_hex_128()?;
        let runtime_dir = temp_root.join(format!("{prefix}-{suffix}"));
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(&runtime_dir) {
            Ok(()) => {
                validate_secure_owner_directory(&runtime_dir, "Claude runtime directory")?;
                return Ok(runtime_dir);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create runtime directory '{}': {error}",
                        runtime_dir.display()
                    ),
                ));
            }
        }
    }
    Err(ExitFailure::new(
        1,
        "rmux claude: failed to create a unique runtime directory",
    ))
}

#[cfg(unix)]
fn random_hex_128() -> Result<String, ExitFailure> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("rmux claude: failed to read OS randomness: {error}"),
            )
        })?;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(output)
}

#[cfg(unix)]
fn validate_secure_owner_directory(path: &Path, label: &str) -> Result<(), ExitFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to inspect {label} '{}': {error}",
                path.display()
            ),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing symlinked {label} '{}'",
                path.display()
            ),
        ));
    }
    if !metadata.is_dir() {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing non-directory {label} '{}'",
                path.display()
            ),
        ));
    }
    // SAFETY: `geteuid` reads the effective uid of the current process and has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if metadata.uid() != uid {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing {label} '{}' owned by uid {}",
                path.display(),
                metadata.uid()
            ),
        ));
    }

    let mode = metadata.mode() & 0o777;
    if mode & 0o077 == 0 {
        return Ok(());
    }

    fs::set_permissions(path, fs::Permissions::from_mode(mode & !0o077)).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to tighten permissions on {label} '{}': {error}",
                path.display()
            ),
        )
    })?;
    let tightened = fs::symlink_metadata(path).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to re-inspect {label} '{}': {error}",
                path.display()
            ),
        )
    })?;
    if tightened.mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(ExitFailure::new(
            1,
            format!(
                "rmux claude: refusing group/world-accessible {label} '{}'",
                path.display()
            ),
        ))
    }
}

#[cfg(unix)]
fn run_claude(mut command: ProcessCommand) -> Result<i32, ExitFailure> {
    let error = command.exec();
    Err(ExitFailure::new(
        1,
        format!("rmux claude: failed to execute claude: {error}"),
    ))
}

#[cfg(not(unix))]
fn run_claude(mut command: ProcessCommand) -> Result<i32, ExitFailure> {
    let status = command.status().map_err(|error| {
        ExitFailure::new(1, format!("rmux claude: failed to execute claude: {error}"))
    })?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(unix)]
fn ensure_private_tmux_shim() -> Result<PrivateTmuxShim, ExitFailure> {
    let dir = private_shim_dir()?;
    std::fs::create_dir_all(&dir).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to create private tmux shim directory '{}': {error}",
                dir.display()
            ),
        )
    })?;
    validate_secure_owner_directory(&dir, "private tmux shim directory")?;
    let target = private_tmux_shim_target_binary()?;
    let shim = dir.join(tmux_file_name());
    match std::fs::symlink_metadata(&shim) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if !symlink_points_to(&shim, &target) {
                std::fs::remove_file(&shim).map_err(|error| {
                    ExitFailure::new(
                        1,
                        format!(
                            "rmux claude: failed to replace private tmux shim '{}': {error}",
                            shim.display()
                        ),
                    )
                })?;
                symlink(&target, &shim).map_err(|error| {
                    ExitFailure::new(
                        1,
                        format!(
                            "rmux claude: failed to create private tmux shim '{}': {error}",
                            shim.display()
                        ),
                    )
                })?;
            }
        }
        Ok(_) => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "rmux claude: '{}' exists and is not a symlink; refusing to overwrite it",
                    shim.display()
                ),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            symlink(&target, &shim).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create private tmux shim '{}': {error}",
                        shim.display()
                    ),
                )
            })?;
        }
        Err(error) => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to inspect private tmux shim '{}': {error}",
                    shim.display()
                ),
            ));
        }
    }
    Ok(PrivateTmuxShim::persistent(dir))
}

#[cfg(windows)]
fn ensure_private_tmux_shim() -> Result<PrivateTmuxShim, ExitFailure> {
    cleanup_stale_windows_claude_temp_dirs();
    let dir = windows_persistent_shim_dir();
    fs::create_dir_all(&dir).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude: failed to create private tmux shim directory '{}': {error}",
                dir.display()
            ),
        )
    })?;
    let source = windows_tmux_shim_source_binary()?;
    install_windows_tmux_shim_in_dir(&source, dir)
}

#[cfg(windows)]
fn install_windows_tmux_shim_in_dir(
    source: &Path,
    dir: PathBuf,
) -> Result<PrivateTmuxShim, ExitFailure> {
    let shim = dir.join(tmux_file_name());
    match install_windows_tmux_shim(source, &shim) {
        Ok(()) => Ok(PrivateTmuxShim::persistent(dir)),
        Err(primary_error) => {
            let fallback_dir = create_windows_temporary_dir("rmux-claude-shim")?;
            let fallback_shim = fallback_dir.join(tmux_file_name());
            install_windows_tmux_shim(source, &fallback_shim).map_err(|fallback_error| {
                let _ = fs::remove_dir_all(&fallback_dir);
                ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to refresh private tmux shim '{}' ({}) and failed to create fallback shim '{}' ({})",
                        shim.display(),
                        primary_error.message(),
                        fallback_shim.display(),
                        fallback_error.message()
                    ),
                )
            })?;
            Ok(PrivateTmuxShim::temporary(fallback_dir))
        }
    }
}

#[cfg(windows)]
fn install_windows_tmux_shim(source: &Path, shim: &Path) -> Result<(), ExitFailure> {
    if shim.exists() {
        match fs::remove_file(shim) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to replace private tmux shim '{}': {error}",
                        shim.display()
                    ),
                ));
            }
        }
    }
    fs::hard_link(source, shim)
        .or_else(|_| fs::copy(source, shim).map(|_| ()))
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!(
                    "rmux claude: failed to create private tmux shim '{}' from '{}': {error}",
                    shim.display(),
                    source.display()
                ),
            )
        })
}

#[cfg(not(any(unix, windows)))]
fn ensure_private_tmux_shim() -> Result<PrivateTmuxShim, ExitFailure> {
    Err(ExitFailure::new(
        1,
        "rmux claude is currently supported only on Unix-like systems and Windows",
    ))
}

#[cfg(windows)]
fn create_windows_temporary_dir(prefix: &str) -> Result<PathBuf, ExitFailure> {
    let temp_root = env::temp_dir();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    for attempt in 0..64u32 {
        let dir = temp_root.join(format!(
            "{prefix}-{}-{timestamp:x}-{attempt}",
            process::id()
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude: failed to create temporary directory '{}': {error}",
                        dir.display()
                    ),
                ));
            }
        }
    }
    Err(ExitFailure::new(
        1,
        "rmux claude: failed to create a unique temporary directory",
    ))
}

#[cfg(windows)]
fn windows_persistent_shim_dir() -> PathBuf {
    let root = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    root.join("rmux").join("claude-tmux-shim")
}

#[cfg(windows)]
fn cleanup_stale_windows_claude_temp_dirs() {
    cleanup_stale_windows_claude_temp_dirs_with_root(&env::temp_dir());
}

#[cfg(windows)]
fn cleanup_stale_windows_claude_temp_dirs_with_root(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        let Some(pid) = stale_windows_claude_temp_dir_pid(name) else {
            continue;
        };
        if !windows_process_is_running(pid) {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(windows)]
fn stale_windows_claude_temp_dir_pid(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix("rmux-claude-shim-")
        .or_else(|| name.strip_prefix("rmux-claude-"))?;
    let pid = rest.split('-').next()?;
    (!pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| pid.parse().ok())
        .flatten()
}

#[cfg(windows)]
fn windows_process_is_running(pid: u32) -> bool {
    rmux_os::process::is_live(pid)
}

#[cfg(windows)]
fn windows_tmux_shim_source_binary() -> Result<PathBuf, ExitFailure> {
    let current = env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve current rmux binary: {error}"),
        )
    })?;
    if let Some(path) = env::var_os(PUBLIC_BINARY_OVERRIDE_ENV) {
        let public = PathBuf::from(path);
        for candidate in windows_full_helper_candidates(&public) {
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    for candidate in windows_full_helper_candidates(&current) {
        if candidate.is_file() && candidate != current {
            return Ok(candidate);
        }
    }
    Ok(current)
}

#[cfg(windows)]
fn windows_full_helper_candidates(current: &Path) -> Vec<PathBuf> {
    let Some(parent) = current.parent() else {
        return Vec::new();
    };
    vec![
        parent.join("libexec").join("rmux").join(rmux_file_name()),
        parent
            .join("..")
            .join("libexec")
            .join("rmux")
            .join(rmux_file_name()),
    ]
}

#[cfg(unix)]
fn private_shim_dir() -> Result<PathBuf, ExitFailure> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExitFailure::new(1, "rmux claude: HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("rmux")
        .join("claude-tmux-shim"))
}

#[cfg(unix)]
fn private_tmux_shim_target_binary() -> Result<PathBuf, ExitFailure> {
    let public = public_rmux_binary()?;
    Ok(private_tmux_shim_target_for_public_binary(&public))
}

#[cfg(unix)]
fn private_tmux_shim_target_for_public_binary(public: &Path) -> PathBuf {
    unix_full_helper_candidates(public)
        .into_iter()
        .find(|candidate| candidate.is_file() && candidate.as_path() != public)
        .unwrap_or_else(|| public.to_path_buf())
}

#[cfg(unix)]
fn unix_full_helper_candidates(public: &Path) -> Vec<PathBuf> {
    let Some(parent) = public.parent() else {
        return Vec::new();
    };
    vec![
        parent.join("libexec").join("rmux").join(rmux_file_name()),
        parent
            .join("..")
            .join("libexec")
            .join("rmux")
            .join(rmux_file_name()),
    ]
}

#[cfg(unix)]
fn public_rmux_binary() -> Result<PathBuf, ExitFailure> {
    if let Some(path) = env::var_os(PUBLIC_BINARY_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve current rmux binary: {error}"),
        )
    })
}

#[cfg(windows)]
enum WindowsClaudeProgram {
    Executable(PathBuf),
    PowerShellScript(PathBuf),
}

#[cfg(windows)]
fn claude_process_command() -> Result<ProcessCommand, ExitFailure> {
    match resolve_windows_claude_program()? {
        WindowsClaudeProgram::Executable(path) => Ok(ProcessCommand::new(path)),
        WindowsClaudeProgram::PowerShellScript(path) => {
            let mut command = ProcessCommand::new("powershell.exe");
            command
                .arg("-NoProfile")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-File")
                .arg(path);
            Ok(command)
        }
    }
}

#[cfg(not(windows))]
fn claude_process_command() -> Result<ProcessCommand, ExitFailure> {
    Ok(ProcessCommand::new("claude"))
}

#[cfg(windows)]
fn resolve_windows_claude_program() -> Result<WindowsClaudeProgram, ExitFailure> {
    resolve_windows_program("claude").ok_or_else(|| {
        ExitFailure::new(
            1,
            "rmux claude: failed to find claude on PATH; install Claude Code or set PATH",
        )
    })
}

#[cfg(windows)]
fn windows_git_bash_path() -> Result<PathBuf, ExitFailure> {
    find_windows_git_bash_path().ok_or_else(|| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude on Windows requires Git Bash for teammate panes; install Git for Windows or set {CLAUDE_GIT_BASH_PATH_ENV}"
            ),
        )
    })
}

#[cfg(windows)]
fn find_windows_git_bash_path() -> Option<PathBuf> {
    env::var_os(CLAUDE_GIT_BASH_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            windows_known_git_bash_paths()
                .into_iter()
                .find(|path| path.is_file())
        })
        .or_else(|| match resolve_windows_program("bash") {
            Some(WindowsClaudeProgram::Executable(path)) if is_git_bash_path(&path) => Some(path),
            _ => None,
        })
}

#[cfg(windows)]
fn windows_known_git_bash_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files\Git\usr\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Git\bin\bash.exe"),
        PathBuf::from(r"C:\Program Files (x86)\Git\usr\bin\bash.exe"),
    ]
}

#[cfg(windows)]
fn is_git_bash_path(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("bash.exe"))
        && path.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case("Git")
        })
}

#[cfg(windows)]
fn resolve_windows_program(name: &str) -> Option<WindowsClaudeProgram> {
    let name_path = Path::new(name);
    let has_path_separator = name.contains('\\') || name.contains('/');
    let mut candidates = Vec::new();
    if has_path_separator || name_path.is_absolute() {
        candidates.extend(windows_program_candidates(name_path));
    } else {
        let path = env::var_os("PATH").unwrap_or_default();
        for dir in env::split_paths(&path) {
            candidates.extend(windows_program_candidates(&dir.join(name)));
        }
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .map(windows_program_kind)
}

#[cfg(windows)]
fn windows_program_candidates(path: &Path) -> Vec<PathBuf> {
    if path.extension().is_some() {
        return vec![path.to_path_buf()];
    }
    windows_pathexts()
        .into_iter()
        .map(|extension| path.with_extension(extension.trim_start_matches('.')))
        .collect()
}

#[cfg(windows)]
fn windows_pathexts() -> Vec<String> {
    env::var_os("PATHEXT")
        .map(|value| {
            env::split_paths(&value)
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| {
            [".COM", ".EXE", ".BAT", ".CMD", ".PS1"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
}

#[cfg(windows)]
fn windows_program_kind(path: PathBuf) -> WindowsClaudeProgram {
    match path
        .extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("ps1") => WindowsClaudeProgram::PowerShellScript(path),
        _ => WindowsClaudeProgram::Executable(path),
    }
}

fn path_with_shim_first(shim_dir: &Path) -> Result<OsString, ExitFailure> {
    path_with_shim_first_from(shim_dir, env::var_os("PATH"))
}

fn path_with_shim_first_from(
    shim_dir: &Path,
    original: Option<OsString>,
) -> Result<OsString, ExitFailure> {
    let original = original.unwrap_or_default();
    let mut paths = vec![shim_dir.to_path_buf()];
    if !original.is_empty() {
        paths.extend(env::split_paths(&original));
    }
    env::join_paths(paths).map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to build PATH with private tmux shim: {error}"),
        )
    })
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

#[cfg(unix)]
fn symlink_points_to(shim: &Path, target: &Path) -> bool {
    let Ok(link_target) = std::fs::read_link(shim) else {
        return false;
    };
    let resolved = if link_target.is_absolute() {
        link_target
    } else {
        shim.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(link_target)
    };
    paths_resolve_to_same_file(&resolved, target)
}

#[cfg(unix)]
fn paths_resolve_to_same_file(left: &Path, right: &Path) -> bool {
    let Ok(left) = std::fs::canonicalize(left) else {
        return false;
    };
    let Ok(right) = std::fs::canonicalize(right) else {
        return false;
    };
    left == right
}

fn tmux_file_name() -> OsString {
    let mut name = OsString::from("tmux");
    if !env::consts::EXE_SUFFIX.is_empty() {
        name.push(env::consts::EXE_SUFFIX);
    }
    name
}

fn rmux_file_name() -> OsString {
    let mut name = OsString::from("rmux");
    if !env::consts::EXE_SUFFIX.is_empty() {
        name.push(env::consts::EXE_SUFFIX);
    }
    name
}

fn split_top_level_prefix(arguments: &[OsString]) -> Option<usize> {
    let mut index = 0;

    while let Some(argument) = arguments.get(index) {
        let value = argument.to_str()?;
        if value == "--" {
            return Some(index + 1);
        }
        if !value.starts_with('-') || value == "-" {
            return Some(index);
        }

        match value {
            "-2" | "-D" | "-N" | "-l" | "-u" => {}
            "-C" | "-v" => {}
            "-c" | "-f" | "-L" | "-S" | "-T" => {
                index += 1;
            }
            _ if value.starts_with("-L") && value.len() > 2 => {}
            _ if value.starts_with("-S") && value.len() > 2 => {}
            _ if value.starts_with("-f") && value.len() > 2 => {}
            _ if value.starts_with("-T") && value.len() > 2 => {}
            _ if is_short_flag_cluster(value, "2CDNluv") => {}
            _ => return Some(index),
        }

        index += 1;
    }

    None
}

fn is_short_flag_cluster(value: &str, allowed: &str) -> bool {
    value.len() > 2
        && value.starts_with('-')
        && !value.starts_with("--")
        && value.chars().skip(1).all(|flag| allowed.contains(flag))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::{
        cleanup_stale_windows_claude_temp_dirs_with_root, ensure_private_tmux_shim,
        install_windows_tmux_shim_in_dir, resolve_windows_program,
        stale_windows_claude_temp_dir_pid, windows_full_helper_candidates,
    };
    use super::{parse_invocation, path_with_shim_first_from};
    use std::env;
    use std::ffi::OsString;
    #[cfg(any(unix, windows))]
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(windows)]
    use std::sync::Mutex;

    #[cfg(windows)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_claude_after_top_level_socket_flags() {
        let invocation = parse_invocation(&args(&[
            "-Ldemo",
            "claude",
            "--dangerously-skip-permissions",
            "--teammate-mode",
            "in-process",
        ]))
        .expect("claude invocation");

        assert_eq!(
            invocation.args,
            args(&[
                "--dangerously-skip-permissions",
                "--teammate-mode",
                "in-process"
            ])
        );
    }

    #[test]
    fn ignores_other_commands() {
        assert!(parse_invocation(&args(&["list-sessions"])).is_none());
    }

    #[test]
    fn shim_path_precedes_existing_path() {
        let shim = Path::new("rmux-shim");
        let existing =
            env::join_paths([Path::new("usr-bin"), Path::new("bin")]).expect("joined path");
        let path = path_with_shim_first_from(shim, Some(existing)).expect("joined path");
        let mut entries = env::split_paths(&path);
        assert_eq!(entries.next(), Some(shim.to_path_buf()));
        assert_eq!(entries.next(), Some(PathBuf::from("usr-bin")));
        assert_eq!(entries.next(), Some(PathBuf::from("bin")));
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_tmux_shim_prefers_packaged_full_helper() {
        let root = unique_test_dir("unix-full-helper");
        let bin = root.join("bin");
        let libexec = root.join("libexec").join("rmux");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::create_dir_all(&libexec).expect("libexec dir");
        let public = bin.join("rmux");
        let helper = libexec.join("rmux");
        fs::write(&public, b"tiny").expect("public rmux");
        fs::write(&helper, b"full").expect("full helper");

        assert_eq!(
            fs::canonicalize(super::private_tmux_shim_target_for_public_binary(&public))
                .expect("canonical target"),
            fs::canonicalize(helper).expect("canonical helper")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_tmux_shim_falls_back_to_public_binary_without_helper() {
        let root = unique_test_dir("unix-no-helper");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin dir");
        let public = bin.join("rmux");
        fs::write(&public, b"full").expect("public rmux");

        assert_eq!(
            super::private_tmux_shim_target_for_public_binary(&public),
            public
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_tmux_shim_is_persistent() {
        let shim = ensure_private_tmux_shim().expect("persistent shim");
        let dir = shim.path().to_path_buf();
        assert!(dir.ends_with(Path::new("rmux").join("claude-tmux-shim")));
        assert!(dir.join("tmux.exe").is_file());
        drop(shim);
        assert!(dir.exists(), "persistent shim directory should be retained");
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_tmux_shim_uses_temporary_fallback_when_persistent_is_stale() {
        let root = unique_test_dir("claude-shim-fallback");
        let persistent = root.join("persistent");
        fs::create_dir_all(&persistent).expect("persistent dir");
        let source = root.join("rmux.exe");
        fs::write(&source, b"rmux").expect("source binary");
        fs::create_dir(persistent.join("tmux.exe")).expect("stale directory blocks replacement");

        let shim =
            install_windows_tmux_shim_in_dir(&source, persistent.clone()).expect("fallback shim");
        let fallback = shim.path().to_path_buf();

        assert_ne!(fallback, persistent);
        assert!(shim.cleanup_on_drop);
        assert!(fallback.join("tmux.exe").is_file());
        drop(shim);
        assert!(
            !fallback.exists(),
            "temporary fallback shim directory should be cleaned up"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_claude_resolution_accepts_cmd_from_pathext() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let root = unique_test_dir("claude-cmd");
        fs::create_dir_all(&root).expect("test dir");
        fs::write(root.join("claude.cmd"), "@echo off\r\n").expect("fake claude");
        let _path = EnvGuard::set("PATH", root.as_os_str());
        let _pathext = EnvGuard::set("PATHEXT", ".CMD;.EXE");

        let program = resolve_windows_program("claude").expect("claude.cmd resolves");

        match program {
            super::WindowsClaudeProgram::Executable(path) => {
                assert!(path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&root.join("claude.cmd").to_string_lossy()))
            }
            _ => panic!("expected executable resolution"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_claude_temp_cleanup_removes_dead_pid_dirs_only() {
        let root = unique_test_dir("claude-cleanup");
        fs::create_dir_all(&root).expect("test root");
        let dead = root.join("rmux-claude-999999-abc-0");
        let live = root.join(format!("rmux-claude-{}-abc-0", std::process::id()));
        fs::create_dir_all(&dead).expect("dead dir");
        fs::create_dir_all(&live).expect("live dir");

        cleanup_stale_windows_claude_temp_dirs_with_root(&root);

        assert!(!dead.exists(), "dead pid temp dir should be removed");
        assert!(live.exists(), "live pid temp dir should be retained");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn stale_windows_claude_temp_dir_pid_parses_known_prefixes() {
        assert_eq!(
            stale_windows_claude_temp_dir_pid("rmux-claude-123-a-0"),
            Some(123)
        );
        assert_eq!(
            stale_windows_claude_temp_dir_pid("rmux-claude-shim-456-a-0"),
            Some(456)
        );
        assert_eq!(stale_windows_claude_temp_dir_pid("rmux-claude-x-a-0"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_helper_candidates_cover_packaged_layouts() {
        let current = Path::new(r"C:\rmux\bin\rmux.exe");
        let candidates = windows_full_helper_candidates(current);
        assert_eq!(
            candidates[0],
            PathBuf::from(r"C:\rmux\bin\libexec\rmux\rmux.exe")
        );
        assert_eq!(
            candidates[1],
            PathBuf::from(r"C:\rmux\bin\..\libexec\rmux\rmux.exe")
        );
    }

    #[cfg(windows)]
    fn unique_test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "rmux-claude-launcher-{label}-{}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn unique_test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "rmux-claude-launcher-{label}-{}",
            std::process::id()
        ))
    }

    #[cfg(windows)]
    struct EnvGuard {
        name: &'static str,
        old: Option<OsString>,
    }

    #[cfg(windows)]
    impl EnvGuard {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old = env::var_os(name);
            // SAFETY: These Windows-only tests serialize their environment mutations with a
            // process-wide mutex, so no other thread observes a concurrent env update here.
            unsafe {
                env::set_var(name, value.as_ref());
            }
            Self { name, old }
        }
    }

    #[cfg(windows)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: See `EnvGuard::set`; the guard is used only while holding the same
            // process-wide test mutex that serializes environment mutation.
            unsafe {
                if let Some(old) = &self.old {
                    env::set_var(self.name, old);
                } else {
                    env::remove_var(self.name);
                }
            }
        }
    }
}
