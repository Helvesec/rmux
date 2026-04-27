use super::{parse_environment_assignments, spawn_hook_command, TerminalProfile};
use rmux_core::{EnvironmentStore, OptionStore};
use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode};
#[cfg(windows)]
use rmux_pty::TerminalSize as PtyTerminalSize;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;
use tokio::time::sleep;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn spawn_hook_command_requires_a_runtime_before_launching_a_child() {
    let output_path = unique_output_path("no-runtime");

    let error = spawn_hook_command(hook_write_command(&output_path, "launched"))
        .expect_err("spawning a hook without a runtime must fail");

    assert_eq!(error.kind(), io::ErrorKind::Other);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !output_path.exists(),
        "hook shell should not launch when no runtime is available"
    );
}

#[tokio::test]
async fn spawn_hook_command_runs_compound_shell_commands() -> Result<(), Box<dyn Error>> {
    let output_path = unique_output_path("compound-command");

    spawn_hook_command(hook_append_command(&output_path, "first", "second"))?;

    wait_for_file_contents(&output_path, "firstsecond").await?;
    fs::remove_file(&output_path)?;
    Ok(())
}

#[test]
fn terminal_profile_sets_rmux_term_shell_and_pane_context() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultTerminal,
            "tmux-256color".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-terminal succeeds");
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            default_shell_string(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        temp_socket_path().as_path(),
        true,
        Some(&["FOO=bar".to_owned()]),
        Some(rmux_core::PaneId::new(3)),
        Some(std::env::temp_dir().as_path()),
    )
    .expect("profile");
    assert_eq!(profile.environment_value("TERM"), Some("tmux-256color"));
    assert_eq!(profile.environment_value("TERM_PROGRAM"), Some("rmux"));
    assert_eq!(
        profile.environment_value("TERM_PROGRAM_VERSION"),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(profile.environment_value("COLORTERM"), Some("truecolor"));
    let socket_path = temp_socket_path();
    let expected_rmux = format!("{},{},7", socket_path.display(), std::process::id());
    assert_eq!(
        profile.environment_value("RMUX"),
        Some(expected_rmux.as_str())
    );
    assert_eq!(profile.environment_value("RMUX_PANE"), Some("%3"));
    assert_eq!(profile.environment_value("FOO"), Some("bar"));
    let expected_cwd = std::env::temp_dir();
    assert_eq!(
        profile.environment_value("SHELL"),
        Some(default_shell_string().as_str())
    );
    assert_eq!(
        profile.environment_value("PWD"),
        Some(expected_cwd.to_string_lossy().as_ref())
    );
    assert_eq!(profile.cwd(), expected_cwd.as_path());
}

#[test]
fn terminal_profile_applies_default_terminal_before_per_command_term_override() {
    let mut environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    environment.set(
        ScopeSelector::Session(session_name.clone()),
        "TERM".to_owned(),
        "screen-256color".to_owned(),
    );
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultTerminal,
            "tmux-256color".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-terminal succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        2,
        Path::new("/tmp/rmux.sock"),
        true,
        None,
        None,
        None,
    )
    .expect("profile");
    assert_eq!(profile.environment_value("TERM"), Some("tmux-256color"));

    let override_profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        2,
        Path::new("/tmp/rmux.sock"),
        true,
        Some(&["TERM=screen-256color".to_owned()]),
        None,
        None,
    )
    .expect("override profile");
    assert_eq!(
        override_profile.environment_value("TERM"),
        Some("screen-256color")
    );
}

#[test]
fn terminal_profile_prefers_rmux_term_program_for_default_window_name() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/bash".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        true,
        None,
        None,
        None,
    )
    .expect("profile");

    assert_eq!(profile.default_window_name().as_deref(), Some("rmux"));
}

#[test]
fn terminal_profile_falls_back_to_shell_name_without_term_program() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/bash".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        false,
        None,
        None,
        None,
    )
    .expect("profile");

    assert_eq!(profile.default_window_name().as_deref(), Some("bash"));
}

#[test]
fn terminal_profile_ignores_non_rmux_term_program_for_default_window_name() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/bash".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        true,
        Some(&["TERM_PROGRAM=tmux".to_owned()]),
        None,
        None,
    )
    .expect("profile");

    assert_eq!(profile.default_window_name().as_deref(), Some("bash"));
}

#[test]
fn terminal_profile_runtime_window_name_tracks_spawned_command_shape() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/bash".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        true,
        None,
        None,
        None,
    )
    .expect("profile");

    assert_eq!(profile.runtime_window_name(None).as_deref(), Some("bash"));
    assert_eq!(
        profile
            .runtime_window_name(Some(&["printf hi".to_owned()]))
            .as_deref(),
        Some("printf")
    );
    assert_eq!(
        profile
            .runtime_window_name(Some(&["exit 0".to_owned()]))
            .as_deref(),
        Some("exit")
    );
    assert_eq!(
        profile
            .runtime_window_name(Some(&["/usr/bin/top".to_owned(), "-H".to_owned()]))
            .as_deref(),
        Some("top")
    );
    assert_eq!(profile.automatic_window_name(None).as_deref(), Some("rmux"));
    assert_eq!(
        profile
            .automatic_window_name(Some(&["sleep 30".to_owned()]))
            .as_deref(),
        Some("sleep")
    );
}

#[test]
fn resolve_shell_path_prefers_default_shell_option_before_shell_env_fallback() {
    let options = OptionStore::new();
    let environment = HashMap::from([("SHELL".to_owned(), "/bin/sh".to_owned())]);
    let resolved = super::resolve_shell_path(&options, None, &environment);
    let expected = options
        .resolve(None, OptionName::DefaultShell)
        .expect("default-shell has a table default");

    assert_eq!(
        resolved,
        super::normalize_shell_path(PathBuf::from(expected))
    );
}

#[cfg(windows)]
#[test]
fn windows_interactive_cmd_starts_in_profile_cwd_and_accepts_input() -> Result<(), Box<dyn Error>> {
    let environment = EnvironmentStore::new();
    let options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let cwd = unique_directory("windows-interactive-cmd")?;
    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        temp_socket_path().as_path(),
        true,
        None,
        Some(rmux_core::PaneId::new(3)),
        Some(cwd.as_path()),
    )?;

    let (master, mut child) =
        super::spawn_pane_process(PtyTerminalSize::new(100, 30), &profile, None)?;
    let io = master.try_clone_io()?;
    let cwd_marker = cwd.to_string_lossy().into_owned();
    let mut output = read_until_io(&io, cwd_marker.as_bytes(), Duration::from_secs(3))?;

    io.write_all(b"echo RMUX_WINDOWS_INTERACTIVE_OK\r\n")?;
    output.extend(read_until_io(
        &io,
        b"RMUX_WINDOWS_INTERACTIVE_OK",
        Duration::from_secs(3),
    )?);

    child.terminate_forcefully()?;
    let _ = child.wait()?;
    fs::remove_dir_all(&cwd)?;

    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains(&cwd_marker),
        "expected Windows shell prompt to start in {cwd_marker}, got {output:?}"
    );
    assert!(
        output.contains("RMUX_WINDOWS_INTERACTIVE_OK"),
        "expected Windows interactive input marker, got {output:?}"
    );
    Ok(())
}

#[test]
fn parse_environment_assignments_rejects_missing_equals() {
    let error = parse_environment_assignments(&["INVALID".to_owned()])
        .expect_err("invalid environment assignment");
    assert_eq!(
        error,
        rmux_proto::RmuxError::Server(
            "environment assignment must be NAME=VALUE: INVALID".to_owned()
        )
    );
}

fn unique_output_path(label: &str) -> PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "rmux-server-terminal-{label}-{}-{unique_id}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&path);
    path
}

#[cfg(windows)]
fn unique_directory(label: &str) -> io::Result<PathBuf> {
    let path = unique_output_path(label);
    fs::create_dir_all(&path)?;
    Ok(path)
}

fn temp_socket_path() -> PathBuf {
    std::env::temp_dir().join("rmux.sock")
}

fn default_shell_string() -> String {
    #[cfg(unix)]
    {
        "/bin/sh".to_owned()
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned())
    }
}

fn hook_write_command(path: &Path, text: &str) -> String {
    #[cfg(unix)]
    {
        format!("printf {} > {}", shell_quote(text), shell_quote_path(path))
    }
    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, {})",
            powershell_quote_path(path),
            powershell_quote(text)
        )
    }
}

fn hook_append_command(path: &Path, first: &str, second: &str) -> String {
    #[cfg(unix)]
    {
        format!(
            "printf {} > {} && printf {} >> {}",
            shell_quote(first),
            shell_quote_path(path),
            shell_quote(second),
            shell_quote_path(path)
        )
    }
    #[cfg(windows)]
    {
        format!(
            "[IO.File]::WriteAllText({}, {}); [IO.File]::AppendAllText({}, {})",
            powershell_quote_path(path),
            powershell_quote(first),
            powershell_quote_path(path),
            powershell_quote(second)
        )
    }
}

#[cfg(unix)]
fn shell_quote_path(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn powershell_quote_path(path: &Path) -> String {
    powershell_quote(&path.display().to_string())
}

#[cfg(windows)]
fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

async fn wait_for_file_contents(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    for _ in 0..100 {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) | Err(_) => sleep(Duration::from_millis(20)).await,
        }
    }

    Err(io::Error::other(format!(
        "file '{}' never reached expected contents '{expected}'",
        path.display()
    ))
    .into())
}

#[cfg(windows)]
fn read_until_io(
    io: &rmux_pty::PtyIo,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];

    while Instant::now() < deadline {
        let bytes_read = io.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..bytes_read]);
        if output.windows(needle.len()).any(|window| window == needle) {
            return Ok(output);
        }
    }

    Ok(output)
}
