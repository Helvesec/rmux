use crate::common;

pub(super) use std::error::Error;
pub(super) use std::ffi::OsString;
pub(super) use std::fs::{self, File};
pub(super) use std::io::Write;
pub(super) use std::os::fd::AsRawFd;
pub(super) use std::path::{Path, PathBuf};
pub(super) use std::process::{Child, Command, Stdio};
pub(super) use std::sync::{Mutex, OnceLock};
pub(super) use std::time::{Duration, Instant};

pub(super) use crate::common::{
    CapturedCommand, EnvironmentOverrides, FrozenTmuxBinary, TmuxCompatHarness, TmuxCompatRun,
    TmuxCompatRunConfig, FROZEN_TMUX_ENV,
};
pub(super) use rmux_core::{input::InputParser, Screen};
pub(super) use rmux_proto::TerminalSize as ScreenTerminalSize;
pub(super) use rmux_pty::{PtyPair, TerminalSize as PtyTerminalSize};

pub(super) const TMUX_COMPAT_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) fn frozen_tmux_or_skip(
    harness: &TmuxCompatHarness,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    match FrozenTmuxBinary::discover() {
        FrozenTmuxBinary::Available(path) => Ok(Some(path)),
        FrozenTmuxBinary::Unavailable {
            checked_path,
            reason,
        } => {
            eprintln!(
                "runtime skip: frozen tmux binary unavailable via {FROZEN_TMUX_ENV} or default '{}': {reason}",
                checked_path.display()
            );
            harness.assert_socket_dirs_clean()?;
            Ok(None)
        }
    }
}

pub(super) fn tmux_compat_config() -> TmuxCompatRunConfig {
    TmuxCompatRunConfig::default().with_timeout(TMUX_COMPAT_TIMEOUT)
}

pub(super) fn config_with_clean_homes(
    harness: &TmuxCompatHarness,
) -> Result<(TmuxCompatRunConfig, EnvironmentOverrides), Box<dyn Error>> {
    let home = harness.tmpdir().join("home");
    let xdg = harness.tmpdir().join("xdg");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&xdg)?;

    let config = tmux_compat_config()
        .with_env("HOME", home.as_os_str())
        .with_env("XDG_CONFIG_HOME", xdg.as_os_str());
    let overrides = default_overrides(harness.tmpdir())
        .into_iter()
        .chain([
            (OsString::from("HOME"), Some(home.as_os_str().to_owned())),
            (
                OsString::from("XDG_CONFIG_HOME"),
                Some(xdg.as_os_str().to_owned()),
            ),
        ])
        .collect();
    Ok((config, overrides))
}

pub(super) fn default_overrides(tmpdir: &Path) -> EnvironmentOverrides {
    vec![
        (
            OsString::from("TMPDIR"),
            Some(tmpdir.as_os_str().to_owned()),
        ),
        (
            OsString::from("RMUX_TMPDIR"),
            Some(tmpdir.as_os_str().to_owned()),
        ),
        (
            OsString::from("TMUX_TMPDIR"),
            Some(tmpdir.as_os_str().to_owned()),
        ),
        (OsString::from("TMUX"), None),
        (
            OsString::from("TERM"),
            Some(OsString::from("xterm-256color")),
        ),
    ]
}

pub(super) fn assert_run_metadata(
    run: &TmuxCompatRun,
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    argv: &[&str],
    expected_overrides: &EnvironmentOverrides,
) {
    assert_command_metadata(
        &run.rmux,
        "rmux",
        Path::new(env!("CARGO_BIN_EXE_rmux")),
        harness.rmux_socket_dir(),
        argv,
        &argv_os(argv),
        expected_overrides,
    );
    assert_command_metadata(
        &run.tmux,
        "tmux",
        tmux_binary,
        harness.tmux_socket_dir(),
        argv,
        &tmux_effective_argv(harness, argv),
        expected_overrides,
    );
}

pub(super) fn assert_rmux_metadata(
    command: &CapturedCommand,
    harness: &TmuxCompatHarness,
    argv: &[&str],
    expected_overrides: &EnvironmentOverrides,
) {
    assert_command_metadata(
        command,
        "rmux",
        Path::new(env!("CARGO_BIN_EXE_rmux")),
        harness.rmux_socket_dir(),
        argv,
        &argv_os(argv),
        expected_overrides,
    );
}

pub(super) fn assert_command_metadata(
    command: &CapturedCommand,
    program: &str,
    program_path: &Path,
    socket_dir: &Path,
    requested_argv: &[&str],
    effective_argv: &[OsString],
    expected_overrides: &EnvironmentOverrides,
) {
    assert_eq!(command.program, program);
    assert_eq!(command.program_path, program_path);
    assert_eq!(command.requested_argv, argv_os(requested_argv));
    assert_eq!(command.effective_argv, effective_argv);
    assert_eq!(command.socket_dir, socket_dir);
    assert_eq!(command.timeout, TMUX_COMPAT_TIMEOUT);
    assert_eq!(&command.environment_overrides, expected_overrides);
}

pub(super) fn tmux_effective_argv(harness: &TmuxCompatHarness, argv: &[&str]) -> Vec<OsString> {
    let mut effective = vec![
        OsString::from("-S"),
        harness.tmux_socket_path().as_os_str().to_owned(),
    ];
    effective.extend(argv_os(argv));
    effective
}

pub(super) fn argv_os(argv: &[&str]) -> Vec<OsString> {
    argv.iter().map(OsString::from).collect()
}

pub(super) fn assert_quiet_success(run: &TmuxCompatRun) {
    assert_eq!(
        run.tmux.status_code,
        Some(0),
        "tmux failed: stdout={:?} stderr={:?}",
        run.tmux.stdout_string(),
        run.tmux.stderr_string()
    );
    assert_eq!(
        run.rmux.status_code,
        Some(0),
        "rmux failed: stdout={:?} stderr={:?}",
        run.rmux.stdout_string(),
        run.rmux.stderr_string()
    );
    assert!(!run.tmux.timed_out);
    assert!(!run.rmux.timed_out);
    assert!(
        run.tmux.stdout.is_empty(),
        "tmux stdout should be empty, got {:?}",
        run.tmux.stdout_string()
    );
    assert!(
        run.rmux.stdout.is_empty(),
        "rmux stdout should be empty, got {:?}",
        run.rmux.stdout_string()
    );
    assert!(
        run.tmux.stderr.is_empty(),
        "tmux stderr should be empty, got {:?}",
        run.tmux.stderr_string()
    );
    assert!(
        run.rmux.stderr.is_empty(),
        "rmux stderr should be empty, got {:?}",
        run.rmux.stderr_string()
    );
}

pub(super) fn assert_exact_tmux_compat(run: &TmuxCompatRun) {
    assert_eq!(run.tmux.status_code, run.rmux.status_code);
    assert_eq!(run.tmux.timed_out, run.rmux.timed_out);
    assert_eq!(run.tmux.stdout, run.rmux.stdout);
    assert_eq!(run.tmux.stderr, run.rmux.stderr);
}

pub(super) fn drop_frozen_mirrored_layout_bindings(output: &[u8]) -> Vec<u8> {
    let output = std::str::from_utf8(output).expect("list-keys output is UTF-8");
    let mut normalized = String::new();
    for line in output.lines() {
        if matches!(line, "prefix:M-6:0" | "prefix:M-7:0") {
            continue;
        }
        normalized.push_str(line);
        normalized.push('\n');
    }
    normalized.into_bytes()
}

pub(super) fn collapse_repeated_horizontal_borders(line: &str) -> String {
    let mut collapsed = String::with_capacity(line.len());
    let mut previous_horizontal = false;
    for character in line.chars() {
        if character == '─' {
            if !previous_horizontal {
                collapsed.push(character);
            }
            previous_horizontal = true;
        } else {
            collapsed.push(character);
            previous_horizontal = false;
        }
    }
    collapsed
}

pub(super) fn utf8_window_name_display_is_ready(output: &str) -> bool {
    let Some(output) = output.strip_suffix('\n') else {
        return false;
    };
    let mut parts = output.split(':');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("alpha"), Some(window_name), Some("80"), None) if !window_name.is_empty()
    )
}

pub(super) fn assert_success_without_stderr(run: &TmuxCompatRun) {
    assert_eq!(run.tmux.status_code, Some(0));
    assert_eq!(run.rmux.status_code, Some(0));
    assert!(!run.tmux.timed_out);
    assert!(!run.rmux.timed_out);
    assert!(run.tmux.stderr_string().is_empty());
    assert!(run.rmux.stderr_string().is_empty());
}

#[derive(Debug)]
pub(super) struct PtyAttachedClient {
    master: File,
    child: Child,
}

impl PtyAttachedClient {
    pub(super) fn spawn(mut command: Command) -> Result<Self, Box<dyn Error>> {
        let pty = PtyPair::open_with_size(PtyTerminalSize { cols: 80, rows: 24 })?;
        let master = File::from(pty.master().try_clone()?.into_owned_fd()?);
        let _terminal = File::from(pty.slave().try_clone()?.into_owned_fd());
        // SAFETY: fcntl is called on a valid file descriptor obtained from the PTY master.
        unsafe {
            let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
            if flags < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        command
            .stdin(Stdio::from(pty.slave().try_clone()?.into_owned_fd()))
            .stdout(Stdio::from(pty.slave().try_clone()?.into_owned_fd()))
            .stderr(Stdio::from(pty.slave().try_clone()?.into_owned_fd()));
        drop(pty);

        Ok(Self {
            master,
            child: command.spawn()?,
        })
    }

    pub(super) fn master_mut(&mut self) -> &mut File {
        &mut self.master
    }

    pub(super) fn assert_running(&mut self, label: &str) -> Result<(), Box<dyn Error>> {
        if let Some(status) = self.child.try_wait()? {
            return Err(format!("{label} attach client exited early with status {status}").into());
        }
        Ok(())
    }
}

impl Drop for PtyAttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn pty_tmux_compat_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub(super) fn spawn_rmux_attached_client(
    harness: &TmuxCompatHarness,
    session_name: &str,
) -> Result<PtyAttachedClient, Box<dyn Error>> {
    spawn_rmux_attached_client_with(harness, session_name, &[], &[])
}

pub(super) fn spawn_rmux_attached_client_with(
    harness: &TmuxCompatHarness,
    session_name: &str,
    top_level_args: &[&str],
    environment: &[(&str, &str)],
) -> Result<PtyAttachedClient, Box<dyn Error>> {
    let home = harness.tmpdir().join("home");
    let xdg = harness.tmpdir().join("xdg");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&xdg)?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_rmux"));
    command
        .env("TMPDIR", harness.tmpdir())
        .env("RMUX_TMPDIR", harness.tmpdir())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("TERM", "xterm-256color")
        .env_remove("RMUX");
    for (name, value) in environment {
        command.env(name, value);
    }
    command
        .args(top_level_args)
        .args(["attach-session", "-r", "-t", session_name]);
    PtyAttachedClient::spawn(command)
}

pub(super) fn spawn_tmux_attached_client(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    session_name: &str,
) -> Result<PtyAttachedClient, Box<dyn Error>> {
    spawn_tmux_attached_client_with(harness, tmux_binary, session_name, &[], &[])
}

pub(super) fn spawn_tmux_attached_client_with(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    session_name: &str,
    top_level_args: &[&str],
    environment: &[(&str, &str)],
) -> Result<PtyAttachedClient, Box<dyn Error>> {
    let mut command = Command::new(tmux_binary);
    command
        .env("TMPDIR", harness.tmpdir())
        .env("TMUX_TMPDIR", harness.tmpdir())
        .env("TERM", "xterm-256color")
        .env_remove("TMUX");
    for (name, value) in environment {
        command.env(name, value);
    }
    command
        .args(top_level_args)
        .arg("-S")
        .arg(harness.tmux_socket_path())
        .args(["attach-session", "-r", "-t", session_name]);
    PtyAttachedClient::spawn(command)
}

pub(super) fn spawn_rmux_attached_input_client(
    harness: &TmuxCompatHarness,
    session_name: &str,
) -> Result<PtyAttachedClient, Box<dyn Error>> {
    let home = harness.tmpdir().join("home");
    let xdg = harness.tmpdir().join("xdg");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&xdg)?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_rmux"));
    command
        .env("TMPDIR", harness.tmpdir())
        .env("RMUX_TMPDIR", harness.tmpdir())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env("LC_CTYPE", "C.UTF-8")
        .env_remove("RMUX")
        .args(["attach-session", "-t", session_name]);
    PtyAttachedClient::spawn(command)
}

pub(super) fn spawn_tmux_attached_input_client(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    session_name: &str,
) -> Result<PtyAttachedClient, Box<dyn Error>> {
    let mut command = Command::new(tmux_binary);
    command
        .env("TMPDIR", harness.tmpdir())
        .env("TMUX_TMPDIR", harness.tmpdir())
        .env("TERM", "xterm-256color")
        .env("LC_ALL", "C.UTF-8")
        .env("LC_CTYPE", "C.UTF-8")
        .env_remove("TMUX")
        .arg("-S")
        .arg(harness.tmux_socket_path())
        .args(["attach-session", "-t", session_name]);
    PtyAttachedClient::spawn(command)
}

pub(super) struct AttachedClientDeadline {
    deadline: Instant,
}

impl AttachedClientDeadline {
    pub(super) fn new() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    pub(super) fn remaining(&self) -> Result<Duration, Box<dyn Error>> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| "attached client scenario exceeded its 30 second bound".into())
    }

    pub(super) fn check(&self) -> Result<(), Box<dyn Error>> {
        let _ = self.remaining()?;
        Ok(())
    }
}

pub(super) fn attached_client_config() -> TmuxCompatRunConfig {
    tmux_compat_config()
        .with_timeout(Duration::from_secs(5))
        .with_env("LC_ALL", "C.UTF-8")
        .with_env("LC_CTYPE", "C.UTF-8")
}

pub(super) fn attached_client_new_session(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    config: TmuxCompatRunConfig,
    deadline: &AttachedClientDeadline,
) -> Result<(), Box<dyn Error>> {
    deadline.check()?;
    let create = harness.run_pair_with(
        tmux_binary,
        &[
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-x",
            "80",
            "-y",
            "24",
            "-n",
            "bash",
        ],
        config.clone(),
    )?;
    assert_quiet_success(&create);
    let populate = harness.run_pair_with(
        tmux_binary,
        &[
            "send-keys",
            "-t",
            "alpha:0.0",
            "for i in $(seq 1 30); do printf 'P0-LINE-%02d\\n' \"$i\"; done",
            "Enter",
        ],
        config.clone(),
    )?;
    assert_quiet_success(&populate);
    let _ = wait_for_attached_pair(
        harness,
        tmux_binary,
        &["capture-pane", "-p", "-S", "-", "-t", "alpha:0.0"],
        config,
        deadline,
        |run| {
            run.rmux.stdout_string().contains("P0-LINE-12")
                && run.tmux.stdout_string().contains("P0-LINE-12")
        },
    )?;
    Ok(())
}

pub(super) fn wait_for_attached_clients(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    config: TmuxCompatRunConfig,
    deadline: &AttachedClientDeadline,
) -> Result<(), Box<dyn Error>> {
    let _ = wait_for_attached_pair(
        harness,
        tmux_binary,
        &["list-clients", "-F", "#{session_name}"],
        config,
        deadline,
        |run| {
            run.rmux.stdout_string().contains("alpha") && run.tmux.stdout_string().contains("alpha")
        },
    )?;
    Ok(())
}

pub(super) fn attached_capture_pair(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    target: &str,
    config: TmuxCompatRunConfig,
    deadline: &AttachedClientDeadline,
) -> Result<TmuxCompatRun, Box<dyn Error>> {
    deadline.check()?;
    harness.run_pair_with(
        tmux_binary,
        &["capture-pane", "-p", "-S", "-", "-t", target],
        config,
    )
}

pub(super) fn wait_for_attached_pair<F>(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    argv: &[&str],
    config: TmuxCompatRunConfig,
    deadline: &AttachedClientDeadline,
    ready: F,
) -> Result<TmuxCompatRun, Box<dyn Error>>
where
    F: Fn(&TmuxCompatRun) -> bool,
{
    let mut last_detail = String::new();
    loop {
        if deadline.remaining().is_err() {
            return Err(format!(
                "attached client scenario exceeded its 30 second bound: {last_detail}"
            )
            .into());
        }
        let run = harness.run_pair_with(tmux_binary, argv, config.clone())?;
        if ready(&run) {
            return Ok(run);
        }
        last_detail = format!(
            "argv={argv:?} tmux={:?}/{:?} rmux={:?}/{:?}",
            run.tmux.stdout_string(),
            run.tmux.stderr_string(),
            run.rmux.stdout_string(),
            run.rmux.stderr_string()
        );
        std::thread::sleep(Duration::from_millis(50).min(deadline.remaining()?));
    }
}

pub(super) fn write_attached_keys(
    client: &mut PtyAttachedClient,
    bytes: &[u8],
    deadline: &AttachedClientDeadline,
) -> Result<(), Box<dyn Error>> {
    deadline.check()?;
    client.master_mut().write_all(bytes)?;
    std::thread::sleep(Duration::from_millis(75).min(deadline.remaining()?));
    Ok(())
}

pub(super) fn shutdown_attached_rmux(harness: &TmuxCompatHarness) -> Result<(), Box<dyn Error>> {
    common::shutdown_rmux_server(harness.rmux_socket_path())?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

pub(super) fn wait_for_pair_run<F>(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    argv: &[&str],
    config: TmuxCompatRunConfig,
    timeout: Duration,
    ready: F,
) -> Result<TmuxCompatRun, Box<dyn Error>>
where
    F: Fn(&TmuxCompatRun) -> bool,
{
    let deadline = Instant::now() + timeout;

    loop {
        let run = harness.run_pair_with(tmux_binary, argv, config.clone())?;
        if ready(&run) {
            return Ok(run);
        }
        let detail = format!(
            "tmux stdout={:?} stderr={:?} rmux stdout={:?} stderr={:?}",
            run.tmux.stdout_string(),
            run.tmux.stderr_string(),
            run.rmux.stdout_string(),
            run.rmux.stderr_string()
        );

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for compatibility command readiness: {}",
                detail
            )
            .into());
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn extract_control_frame_payload_lines(output: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_frame = false;

    for line in output.lines() {
        if line.starts_with("%begin ") {
            in_frame = true;
            continue;
        }
        if line.starts_with("%end ") || line.starts_with("%error ") {
            in_frame = false;
            continue;
        }
        if in_frame && !line.is_empty() {
            lines.push(line.to_owned());
        }
    }

    lines
}

pub(super) fn nonempty_capture_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn normalize_pts_paths(line: &str) -> String {
    let mut normalized = String::new();
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        normalized.push(ch);
        if normalized.ends_with("/dev/pts/") {
            while chars.peek().is_some_and(|next| next.is_ascii_digit()) {
                let _ = chars.next();
            }
            normalized.push('N');
        }
    }
    normalized
}

pub(super) fn drain_pty(client: &mut PtyAttachedClient) -> Result<Vec<u8>, Box<dyn Error>> {
    use std::io::ErrorKind;

    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match std::io::Read::read(client.master_mut(), &mut buffer) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(bytes)
}

pub(super) fn render_cells(bytes: &[u8], cols: usize, rows: usize) -> Vec<String> {
    let mut screen = vec![vec![' '; cols]; rows];
    let mut row = 0usize;
    let mut col = 0usize;
    let chars = String::from_utf8_lossy(bytes).chars().collect::<Vec<_>>();
    let mut index = 0usize;

    while index < chars.len() {
        match chars[index] {
            '\u{1b}' => {
                index += 1;
                if index >= chars.len() {
                    break;
                }
                match chars[index] {
                    '[' => {
                        index += 1;
                        let mut params = String::new();
                        while index < chars.len()
                            && !chars[index].is_ascii_alphabetic()
                            && chars[index] != 'X'
                            && chars[index] != 'H'
                            && chars[index] != 'K'
                            && chars[index] != 'm'
                        {
                            params.push(chars[index]);
                            index += 1;
                        }
                        if index >= chars.len() {
                            break;
                        }
                        match chars[index] {
                            'H' => {
                                let mut parts = params.split(';');
                                row = parts
                                    .next()
                                    .and_then(|value| value.parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .saturating_sub(1)
                                    .min(rows.saturating_sub(1));
                                col = parts
                                    .next()
                                    .and_then(|value| value.parse::<usize>().ok())
                                    .unwrap_or(1)
                                    .saturating_sub(1)
                                    .min(cols.saturating_sub(1));
                            }
                            'K' => {
                                for cell in screen[row].iter_mut().skip(col) {
                                    *cell = ' ';
                                }
                            }
                            'X' => {
                                let count = params.parse::<usize>().unwrap_or(1);
                                for cell in screen[row]
                                    .iter_mut()
                                    .skip(col)
                                    .take(count.min(cols.saturating_sub(col)))
                                {
                                    *cell = ' ';
                                }
                            }
                            'm' => {}
                            _ => {}
                        }
                    }
                    '(' | ')' => {
                        index += 1;
                    }
                    _ => {}
                }
            }
            '\r' => col = 0,
            '\n' => row = row.saturating_add(1).min(rows.saturating_sub(1)),
            ch => {
                if row < rows && col < cols {
                    screen[row][col] = ch;
                }
                col = col.saturating_add(1);
            }
        }
        index += 1;
    }

    screen
        .into_iter()
        .map(|line| line.into_iter().collect::<String>())
        .collect()
}

pub(super) fn display_panes_overlay_visible(rendered: &str) -> bool {
    rendered.contains("39x23")
        || rendered.contains("40x23")
        || rendered.contains("39x24")
        || rendered.contains("40x24")
}

pub(super) fn render_transcript(bytes: &[u8], cols: u16, rows: u16) -> String {
    let mut screen = Screen::new(ScreenTerminalSize { cols, rows }, 0);
    let mut parser = InputParser::new();
    parser.parse(bytes, &mut screen);
    String::from_utf8(screen.capture_transcript(Default::default(), Default::default()))
        .expect("captured transcript must be utf-8")
}

pub(super) fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

#[derive(Debug)]
pub(super) struct ControlModeOutput {
    pub(super) status_code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) fn run_control_mode_client(
    mut command: Command,
    commands: &str,
) -> Result<ControlModeOutput, Box<dyn Error>> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .as_mut()
        .expect("control stdin")
        .write_all(commands.as_bytes())?;

    let output = child.wait_with_output()?;
    Ok(ControlModeOutput {
        status_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub(super) fn run_rmux_control_mode(
    harness: &TmuxCompatHarness,
    commands: &str,
) -> Result<ControlModeOutput, Box<dyn Error>> {
    run_rmux_control_mode_with(harness, commands, &[], &[])
}

pub(super) fn run_rmux_control_mode_with(
    harness: &TmuxCompatHarness,
    commands: &str,
    top_level_args: &[&str],
    environment: &[(&str, &str)],
) -> Result<ControlModeOutput, Box<dyn Error>> {
    let home = harness.tmpdir().join("home");
    let xdg = harness.tmpdir().join("xdg");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&xdg)?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_rmux"));
    command
        .env("TMPDIR", harness.tmpdir())
        .env("RMUX_TMPDIR", harness.tmpdir())
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg)
        .env("TERM", "xterm-256color")
        .env_remove("RMUX");
    for (name, value) in environment {
        command.env(name, value);
    }
    command.args(top_level_args).arg("-C");
    run_control_mode_client(command, commands)
}

pub(super) fn run_tmux_control_mode(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    commands: &str,
) -> Result<ControlModeOutput, Box<dyn Error>> {
    run_tmux_control_mode_with(harness, tmux_binary, commands, &[], &[])
}

pub(super) fn run_tmux_control_mode_with(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    commands: &str,
    top_level_args: &[&str],
    environment: &[(&str, &str)],
) -> Result<ControlModeOutput, Box<dyn Error>> {
    let mut command = Command::new(tmux_binary);
    command
        .env("TMPDIR", harness.tmpdir())
        .env("TMUX_TMPDIR", harness.tmpdir())
        .env("TERM", "xterm-256color")
        .env_remove("TMUX");
    for (name, value) in environment {
        command.env(name, value);
    }
    command
        .args(top_level_args)
        .arg("-C")
        .arg("-S")
        .arg(harness.tmux_socket_path());
    run_control_mode_client(command, commands)
}

pub(super) fn sorted_first_words(output: &str) -> Vec<String> {
    let mut words: Vec<String> = output
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(ToOwned::to_owned)
        .collect();
    words.sort();
    words.dedup();
    words
}

pub(super) fn assert_matching_line(run: &TmuxCompatRun, prefix: &str) -> String {
    let tmux_line = line_with_prefix(&run.tmux.stdout_string(), prefix);
    let rmux_line = line_with_prefix(&run.rmux.stdout_string(), prefix);
    assert_eq!(tmux_line, rmux_line);
    rmux_line
}

fn line_with_prefix(output: &str, prefix: &str) -> String {
    output
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing line with prefix {prefix:?} in output {output:?}"))
        .to_owned()
}
