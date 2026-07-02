#![cfg(windows)]

use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rmux_pty::{ChildCommand, SpawnedPty, TerminalSize};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

const SETUP_TIMEOUT: Duration = Duration::from_secs(8);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);

const FORMAT: &str = "#{client_termname}|#{client_termfeatures}|#{client_utf8}|#{client_flags}";
const EXPLICIT_TOP_LEVEL_TERMINAL_ARGS: &[&str] = &["-u", "-2", "-T", "RGB"];
const NO_TOP_LEVEL_ARGS: &[&str] = &[];
const INHERITED_RMUX: &str = r"C:\tmp\outer-rmux,123,0";
const INHERITED_TMUX: &str = r"C:\tmp\outer-tmux,123,0";

#[derive(Debug, Clone, Copy)]
struct TerminalProfile {
    name: &'static str,
    term: &'static str,
    term_program: &'static str,
    colorterm: &'static str,
    wt_session: bool,
    top_level_args: &'static [&'static str],
    expected_utf8: &'static str,
    expected_features: &'static [&'static str],
}

#[test]
fn windows_attached_client_terminal_profile_matrix_reports_runtime_features(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("attached-client-terminal-profile-matrix")?;
    let _ambient_multiplexer_env = MultiplexerEnvGuard::with_fake_outer();

    for profile in attach_terminal_profiles() {
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
        let label = format!(
            "win-term-matrix-{}-{}",
            sanitize_label(profile.name),
            std::process::id()
        );
        let _server = RmuxServerGuard::new(&binary, label.clone());

        run_rmux(
            &binary,
            &label,
            ["new-session", "-d", "-s", "alpha", "cmd.exe", "/D", "/K"],
        )?;
        run_rmux(&binary, &label, ["set-option", "-g", "status", "off"])?;

        let mut attach = spawn_profile_attach(&binary, &label, "alpha", *profile)?;
        let line = wait_for_attached_client_line(&binary, &label, SETUP_TIMEOUT)?;
        assert_profile_line(profile, &line, false)?;

        terminate_spawned(&mut attach);
    }

    Ok(())
}

#[test]
fn windows_control_mode_terminal_profile_matrix_reports_runtime_features(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("control-mode-terminal-profile-matrix")?;
    let _ambient_multiplexer_env = MultiplexerEnvGuard::with_fake_outer();

    for profile in control_terminal_profiles() {
        let binary = PathBuf::from(env!("CARGO_BIN_EXE_rmux"));
        let label = format!(
            "win-control-term-matrix-{}-{}",
            sanitize_label(profile.name),
            std::process::id()
        );
        let _server = RmuxServerGuard::new(&binary, label.clone());

        run_rmux(
            &binary,
            &label,
            ["new-session", "-d", "-s", "alpha", "cmd.exe", "/D", "/K"],
        )?;

        let transcript = run_control_mode_with_profile(
            &binary,
            &label,
            *profile,
            &format!("attach-session -t alpha\nlist-clients -F '{FORMAT}'\n"),
        )?;
        assert!(
            transcript.status.success(),
            "control-mode profile {} failed: status={:?}\nstdout={}\nstderr={}",
            profile.name,
            transcript.status.code(),
            String::from_utf8_lossy(&transcript.stdout),
            String::from_utf8_lossy(&transcript.stderr)
        );
        assert!(
            transcript.stderr.is_empty(),
            "control-mode stderr should stay empty for {}: {}",
            profile.name,
            String::from_utf8_lossy(&transcript.stderr)
        );

        let line = extract_control_payload_line(&transcript.stdout).ok_or_else(|| {
            format!(
                "control-mode profile {} did not emit the list-clients payload; transcript={}",
                profile.name,
                String::from_utf8_lossy(&transcript.stdout)
            )
        })?;
        assert_profile_line(profile, &line, true)?;
    }

    Ok(())
}

fn attach_terminal_profiles() -> &'static [TerminalProfile] {
    &[
        TerminalProfile {
            name: "explicit-vt100-top-level",
            term: "vt100",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: EXPLICIT_TOP_LEVEL_TERMINAL_ARGS,
            expected_utf8: "1",
            expected_features: &["256", "RGB"],
        },
        TerminalProfile {
            name: "windows-terminal",
            term: "xterm-256color",
            term_program: "Windows Terminal",
            colorterm: "truecolor",
            wt_session: true,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "1",
            expected_features: &["256", "RGB", "bpaste", "mouse", "sync"],
        },
        TerminalProfile {
            name: "kitty-term-profile",
            term: "xterm-kitty",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &["kitty-graphics", "hyperlinks", "margins", "osc7", "sync"],
        },
        TerminalProfile {
            name: "wezterm-profile",
            term: "wezterm",
            term_program: "WezTerm",
            colorterm: "truecolor",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &["kitty-graphics", "sixel", "sync", "RGB"],
        },
        TerminalProfile {
            name: "mintty-profile",
            term: "mintty",
            term_program: "mintty",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &["clipboard", "mouse", "sixel"],
        },
    ]
}

fn control_terminal_profiles() -> &'static [TerminalProfile] {
    &[
        TerminalProfile {
            name: "control-explicit-vt100-top-level",
            term: "vt100",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: EXPLICIT_TOP_LEVEL_TERMINAL_ARGS,
            expected_utf8: "1",
            expected_features: &["256", "RGB"],
        },
        TerminalProfile {
            name: "control-windows-terminal",
            term: "xterm-256color",
            term_program: "Windows Terminal",
            colorterm: "truecolor",
            wt_session: true,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "1",
            expected_features: &["bpaste", "mouse", "sync"],
        },
        TerminalProfile {
            name: "control-kitty-term-profile",
            term: "xterm-kitty",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &[],
        },
        TerminalProfile {
            name: "control-wezterm-profile",
            term: "wezterm",
            term_program: "WezTerm",
            colorterm: "truecolor",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &[],
        },
        TerminalProfile {
            name: "control-mintty-profile",
            term: "mintty",
            term_program: "mintty",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &[],
        },
    ]
}

fn spawn_profile_attach(
    binary: &Path,
    label: &str,
    session: &str,
    profile: TerminalProfile,
) -> Result<SpawnedPty, Box<dyn Error>> {
    let mut command = ChildCommand::new(binary)
        .arg("-L")
        .arg(label)
        .size(TerminalSize::new(100, 30));
    for arg in profile.top_level_args {
        command = command.arg(*arg);
    }
    command = command.args(["attach-session", "-t", session]);
    command = apply_profile_env_to_pty_command(command, profile);
    Ok(command.spawn()?)
}

fn run_control_mode_with_profile(
    binary: &Path,
    label: &str,
    profile: TerminalProfile,
    stdin: &str,
) -> Result<Output, Box<dyn Error>> {
    let mut command = Command::new(binary);
    command.arg("-L").arg(label);
    for arg in profile.top_level_args {
        command.arg(arg);
    }
    command.arg("-C");
    apply_profile_env_to_command(&mut command, profile);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .expect("control mode stdin is piped")
        .write_all(stdin.as_bytes())?;
    wait_for_child_output(child, CONTROL_TIMEOUT)
}

fn apply_profile_env_to_pty_command(
    command: ChildCommand,
    profile: TerminalProfile,
) -> ChildCommand {
    let command = apply_inherited_env_without_multiplexer_markers(command);

    command
        .env("TERM", profile.term)
        .env("TERM_PROGRAM", profile.term_program)
        .env("COLORTERM", profile.colorterm)
        .env(
            "WT_SESSION",
            if profile.wt_session {
                "rmux-windows-terminal-matrix"
            } else {
                ""
            },
        )
        .env("LC_ALL", "C")
        .env("LC_CTYPE", "")
        .env("LANG", "")
}

fn apply_profile_env_to_command(command: &mut Command, profile: TerminalProfile) {
    command
        .env("TERM", profile.term)
        .env("TERM_PROGRAM", profile.term_program)
        .env("COLORTERM", profile.colorterm)
        .env(
            "WT_SESSION",
            if profile.wt_session {
                "rmux-windows-terminal-matrix"
            } else {
                ""
            },
        )
        .env("LC_ALL", "C")
        .env("LC_CTYPE", "")
        .env("LANG", "")
        .env_remove("RMUX")
        .env_remove("TMUX");
}

fn apply_inherited_env_without_multiplexer_markers(mut command: ChildCommand) -> ChildCommand {
    command = command.clear_env();
    for (name, value) in std::env::vars_os() {
        if is_multiplexer_env_name(&name) {
            continue;
        }
        command = command.env(name, value);
    }
    command
}

fn is_multiplexer_env_name(name: &OsStr) -> bool {
    env_name_eq_ignore_ascii_case(name, "RMUX") || env_name_eq_ignore_ascii_case(name, "TMUX")
}

fn env_name_eq_ignore_ascii_case(name: &OsStr, expected: &str) -> bool {
    name.to_string_lossy().eq_ignore_ascii_case(expected)
}

fn wait_for_attached_client_line(
    binary: &Path,
    label: &str,
    timeout: Duration,
) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last_stdout = String::new();
    let mut last_stderr = String::new();
    while Instant::now() < deadline {
        let output = run_rmux_output(binary, label, ["list-clients", "-F", FORMAT])?;
        last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if output.status.success() {
            if let Some(line) = last_stdout.lines().find(|line| !line.trim().is_empty()) {
                return Ok(line.to_owned());
            }
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err(format!(
        "timed out waiting for attached client; last stdout={last_stdout:?} stderr={last_stderr:?}"
    )
    .into())
}

fn extract_control_payload_line(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .find(|line| !line.starts_with('%') && line.contains('|'))
        .map(ToOwned::to_owned)
}

fn assert_profile_line(
    profile: &TerminalProfile,
    line: &str,
    control_mode: bool,
) -> Result<(), Box<dyn Error>> {
    let parts = line.trim_end().split('|').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err(format!(
            "profile {} produced malformed list-clients line {line:?}",
            profile.name
        )
        .into());
    }

    assert_eq!(
        parts[0], profile.term,
        "profile {} should preserve client_termname",
        profile.name
    );
    assert_eq!(
        parts[2], profile.expected_utf8,
        "profile {} should preserve client_utf8",
        profile.name
    );

    let features = parts[1].split(',').collect::<Vec<_>>();
    for expected in profile.expected_features {
        assert!(
            features.iter().any(|feature| feature == expected),
            "profile {} should include feature {expected:?}; got {features:?} from {line:?}",
            profile.name
        );
    }

    let flags = parts[3].split(',').collect::<Vec<_>>();
    assert!(
        flags.contains(&"attached") && flags.contains(&"focused"),
        "profile {} should report attached/focused client flags; got {flags:?}",
        profile.name
    );
    if profile.expected_utf8 == "1" {
        assert!(
            flags.contains(&"UTF-8"),
            "profile {} should expose UTF-8 client flag; got {flags:?}",
            profile.name
        );
    } else {
        assert!(
            !flags.contains(&"UTF-8"),
            "profile {} should not expose UTF-8 client flag; got {flags:?}",
            profile.name
        );
    }
    assert_eq!(
        flags.contains(&"control-mode"),
        control_mode,
        "profile {} control-mode flag mismatch in {flags:?}",
        profile.name
    );

    Ok(())
}

struct RmuxServerGuard<'a> {
    binary: &'a Path,
    label: String,
}

impl<'a> RmuxServerGuard<'a> {
    fn new(binary: &'a Path, label: String) -> Self {
        Self { binary, label }
    }
}

impl Drop for RmuxServerGuard<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.binary)
            .arg("-L")
            .arg(&self.label)
            .arg("kill-server")
            .env_remove("RMUX")
            .env_remove("TMUX")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn run_rmux<const N: usize>(
    binary: &Path,
    label: &str,
    args: [&str; N],
) -> Result<(), Box<dyn Error>> {
    let output = run_rmux_output(binary, label, args)?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "rmux command failed with {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(())
}

fn run_rmux_output<const N: usize>(
    binary: &Path,
    label: &str,
    args: [&str; N],
) -> Result<Output, Box<dyn Error>> {
    Ok(Command::new(binary)
        .arg("-L")
        .arg(label)
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("RMUX")
        .env_remove("TMUX")
        .output()?)
}

fn wait_for_child_output(mut child: Child, timeout: Duration) -> Result<Output, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(_status) = child.try_wait()? {
            return Ok(child.wait_with_output()?);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            return Err(format!(
                "control-mode client did not exit before timeout; status={:?}\nstdout={}\nstderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminate_spawned(spawned: &mut SpawnedPty) {
    let _ = spawned.child().terminate_forcefully();
    let _ = spawned.child_mut().wait();
}

fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

struct MultiplexerEnvGuard {
    _rmux: EnvVarGuard,
    _tmux: EnvVarGuard,
}

impl MultiplexerEnvGuard {
    fn with_fake_outer() -> Self {
        Self {
            _rmux: EnvVarGuard::set("RMUX", INHERITED_RMUX),
            _tmux: EnvVarGuard::set("TMUX", INHERITED_TMUX),
        }
    }
}

struct EnvVarGuard {
    name: &'static str,
    previous_value: Option<OsString>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &'static str) -> Self {
        let previous_value = std::env::var_os(name);
        std::env::set_var(name, value);
        Self {
            name,
            previous_value,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous_value.as_ref() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}
