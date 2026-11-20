#[cfg(unix)]
use super::environment_from_os_pairs;
#[cfg(unix)]
#[path = "post_exec_tests.rs"]
mod post_exec_tests;
#[cfg(windows)]
use super::raw_process_environment;
#[cfg(windows)]
use super::SessionBaseEnvironment;
use super::{
    parse_environment_assignments, spawn_hook_command, validate_process_command, TerminalProfile,
};
use rmux_core::{EnvironmentStore, OptionStore};
#[cfg(windows)]
use rmux_proto::RmuxError;
use rmux_proto::{OptionName, ProcessCommand, ScopeSelector, SessionName, SetOptionMode};
#[cfg(windows)]
use rmux_pty::TerminalSize as PtyTerminalSize;
use std::collections::HashMap;
use std::error::Error;
#[cfg(any(unix, windows))]
use std::ffi::OsString;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(windows)]
use std::sync::mpsc;
#[cfg(windows)]
use std::thread;
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;
use tokio::time::sleep;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[path = "tests/program_path_spaces.rs"]
mod program_path_spaces;

#[cfg(unix)]
#[test]
fn base_environment_snapshot_skips_non_utf8_pairs() {
    let environment = environment_from_os_pairs([
        (
            OsString::from_vec(b"INVALID_NAME_\xff".to_vec()),
            OsString::from("value"),
        ),
        (
            OsString::from("INVALID_VALUE"),
            OsString::from_vec(b"value_\xff".to_vec()),
        ),
        (OsString::from("VALID"), OsString::from("value")),
    ]);

    assert_eq!(environment.get("VALID").map(String::as_str), Some("value"));
    assert_eq!(environment.len(), 1);
}

#[cfg(windows)]
#[test]
fn raw_environment_suppresses_names_case_insensitively() {
    let raw = raw_process_environment(
        Some(&[(
            std::ffi::OsString::from("Path"),
            std::ffi::OsString::from("C:\\base"),
        )]),
        &HashMap::new(),
        &["PATH".to_owned()].into_iter().collect(),
    );

    assert!(raw.is_empty());
}

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
        None,
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
    let ambient_colorterm = std::env::var("COLORTERM").ok();
    assert_eq!(
        profile.environment_value("COLORTERM"),
        ambient_colorterm.as_deref()
    );
    let socket_path = temp_socket_path();
    let expected_rmux = expected_mux_env(&socket_path, 7);
    assert_eq!(
        profile.environment_value("RMUX"),
        Some(expected_rmux.as_str())
    );
    assert_eq!(
        profile.environment_value("TMUX"),
        Some(expected_rmux.as_str())
    );
    assert_eq!(profile.environment_value("RMUX_PANE"), Some("%3"));
    assert_eq!(profile.environment_value("TMUX_PANE"), Some("%3"));
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
fn terminal_profile_applies_spawn_environment_before_explicit_overrides() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let spawn_environment = HashMap::from([
        ("PATH".to_owned(), "/client/bin:/usr/bin".to_owned()),
        ("RMUX_CLIENT_ONLY".to_owned(), "present".to_owned()),
    ]);

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
        Some(&spawn_environment),
        true,
        Some(&["RMUX_CLIENT_ONLY=override".to_owned()]),
        None,
        None,
    )
    .expect("profile");

    assert_eq!(
        profile.environment_value("PATH"),
        Some("/client/bin:/usr/bin")
    );
    assert_eq!(
        profile.environment_value("RMUX_CLIENT_ONLY"),
        Some("override")
    );
}

#[cfg(unix)]
#[test]
fn terminal_profile_uses_client_shell_when_default_shell_is_unset() {
    let environment = EnvironmentStore::new();
    let options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let client_shell = std::env::current_exe().expect("test executable path");
    let client_shell_value = client_shell
        .to_str()
        .expect("test executable path is UTF-8")
        .to_owned();
    let spawn_environment = HashMap::from([
        ("SHELL".to_owned(), client_shell_value.clone()),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
    ]);

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        temp_socket_path().as_path(),
        Some(&spawn_environment),
        true,
        None,
        None,
        None,
    )
    .expect("profile");

    assert_eq!(profile.shell(), client_shell);
    assert_eq!(
        profile.environment_value("SHELL"),
        Some(client_shell_value.as_str())
    );
}

#[test]
fn terminal_profile_honors_explicit_color_environment_overrides() {
    let mut environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

    environment.set(
        ScopeSelector::Session(session_name.clone()),
        "NO_COLOR".to_owned(),
        "1".to_owned(),
    );
    environment.set(
        ScopeSelector::Session(session_name.clone()),
        "COLORTERM".to_owned(),
        "truecolor".to_owned(),
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
        7,
        temp_socket_path().as_path(),
        None,
        true,
        Some(&["NODE_DISABLE_COLORS=1".to_owned(), "CLICOLOR=0".to_owned()]),
        Some(rmux_core::PaneId::new(3)),
        Some(std::env::temp_dir().as_path()),
    )
    .expect("profile");

    assert_eq!(profile.environment_value("NO_COLOR"), Some("1"));
    assert_eq!(profile.environment_value("COLORTERM"), Some("truecolor"));
    assert_eq!(profile.environment_value("NODE_DISABLE_COLORS"), Some("1"));
    assert_eq!(profile.environment_value("CLICOLOR"), Some("0"));
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
        None,
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
        None,
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
fn run_shell_profile_exports_tmux_env_for_plugin_children() {
    let environment = EnvironmentStore::new();
    let options = OptionStore::new();
    let socket_path = temp_socket_path();

    let detached_profile = TerminalProfile::for_run_shell(
        &environment,
        &options,
        None,
        None,
        socket_path.as_path(),
        false,
        Some(std::env::temp_dir().as_path()),
    )
    .expect("detached run-shell profile");
    let expected_detached = expected_mux_env(&socket_path, 0);
    assert_eq!(
        detached_profile.environment_value("RMUX"),
        Some(expected_detached.as_str())
    );
    assert_eq!(
        detached_profile.environment_value("TMUX"),
        Some(expected_detached.as_str())
    );

    let session_name = SessionName::new("alpha").expect("valid session name");
    let targeted_profile = TerminalProfile::for_run_shell(
        &environment,
        &options,
        Some(&session_name),
        Some(7),
        socket_path.as_path(),
        false,
        Some(std::env::temp_dir().as_path()),
    )
    .expect("targeted run-shell profile");
    let expected_targeted = expected_mux_env(&socket_path, 7);
    assert_eq!(
        targeted_profile.environment_value("RMUX"),
        Some(expected_targeted.as_str())
    );
    assert_eq!(
        targeted_profile.environment_value("TMUX"),
        Some(expected_targeted.as_str())
    );
}

#[test]
fn run_shell_profile_exports_absolute_mux_env_for_relative_socket() -> Result<(), Box<dyn Error>> {
    let environment = EnvironmentStore::new();
    let options = OptionStore::new();
    let socket_path = PathBuf::from("relative-rmux.sock");
    let run_cwd = unique_output_path("run-shell-relative-socket-cwd");
    fs::create_dir_all(&run_cwd)?;

    let profile = TerminalProfile::for_run_shell(
        &environment,
        &options,
        None,
        None,
        socket_path.as_path(),
        false,
        Some(run_cwd.as_path()),
    )
    .expect("run-shell profile");
    let expected_rmux = expected_mux_env(socket_path.as_path(), 0);

    assert_eq!(
        profile.environment_value("RMUX"),
        Some(expected_rmux.as_str())
    );
    assert_eq!(
        profile.environment_value("TMUX"),
        Some(expected_rmux.as_str())
    );
    assert_eq!(profile.cwd(), run_cwd.as_path());

    Ok(())
}

#[cfg(windows)]
#[test]
fn run_shell_profile_replaces_reserved_names_case_insensitively() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let socket_path = temp_socket_path();
    let base_environment = SessionBaseEnvironment {
        raw_environment: [
            ("term", "legacy-term"),
            ("term_program", "legacy-program"),
            ("term_program_version", "legacy-version"),
            ("rmux", "legacy-rmux"),
            ("tmux", "legacy-tmux"),
            ("rmux_pane", "%old"),
            ("tmux_pane", "%old"),
        ]
        .into_iter()
        .map(|(name, value)| (OsString::from(name), OsString::from(value)))
        .collect(),
    };

    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultTerminal,
            "tmux-256color".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-terminal succeeds");

    let profile = TerminalProfile::for_run_shell_with_base_environment(
        &environment,
        &options,
        Some(&session_name),
        Some(7),
        socket_path.as_path(),
        Some(&base_environment),
        true,
        None,
        Some(std::env::temp_dir().as_path()),
    )
    .expect("profile");
    let expected_tmux = expected_mux_env(&socket_path, 7);

    assert_eq!(profile.environment_value("TERM"), Some("tmux-256color"));
    assert_eq!(profile.environment_value("TERM_PROGRAM"), Some("rmux"));
    assert_eq!(
        profile.environment_value("TERM_PROGRAM_VERSION"),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        profile.environment_value("RMUX"),
        Some(expected_tmux.as_str())
    );
    assert_eq!(
        profile.environment_value("TMUX"),
        Some(expected_tmux.as_str())
    );
    assert_eq!(profile.environment_value("RMUX_PANE"), None);
    assert_eq!(profile.environment_value("TMUX_PANE"), None);

    for name in [
        "term",
        "term_program",
        "term_program_version",
        "rmux",
        "tmux",
        "rmux_pane",
        "tmux_pane",
    ] {
        assert!(
            profile
                .raw_environment()
                .all(|(candidate, _)| candidate.to_str() != Some(name)),
            "{name} should have been replaced case-insensitively"
        );
    }
}

#[test]
fn terminal_profile_names_default_window_after_shell_despite_rmux_term_program() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");

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
        Path::new("/tmp/rmux.sock"),
        None,
        true,
        None,
        None,
        None,
    )
    .expect("profile");

    assert_eq!(profile.environment_value("TERM_PROGRAM"), Some("rmux"));
    let shell_name = default_shell_name();
    assert_eq!(profile.default_window_name().as_deref(), Some(&*shell_name));
}

#[test]
fn terminal_profile_initial_pane_title_uses_platform_hostname() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let home = std::env::current_dir().expect("current dir");
    let home_text = home.to_string_lossy().into_owned();

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
        Path::new("/tmp/rmux.sock"),
        None,
        true,
        Some(&[
            "USER=alice".to_owned(),
            format!("HOME={home_text}"),
            "PWD=/ignored".to_owned(),
        ]),
        None,
        Some(&home),
    )
    .expect("profile");

    let title = profile.initial_pane_title().expect("initial title");
    let host = crate::host_name::local_hostname().expect("host name");
    #[cfg(unix)]
    assert_eq!(title, host);
    #[cfg(not(unix))]
    assert_eq!(title, host.split('.').next().unwrap_or(&host));
}

#[cfg(unix)]
#[test]
fn terminal_profile_initial_pane_title_preserves_native_hostname() {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
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
        Path::new("/tmp/rmux.sock"),
        None,
        false,
        None,
        None,
        None,
    )
    .expect("profile");

    // tmux seeds pane_title with the full hostname. The default status-right
    // then clips that title to 21 cells; shortening it here loses the domain.
    assert_eq!(
        profile.initial_pane_title(),
        rmux_os::host::local_hostname(),
    );
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
            default_shell_string(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        None,
        false,
        None,
        None,
        None,
    )
    .expect("profile");

    let shell_name = default_shell_name();
    assert_eq!(profile.default_window_name().as_deref(), Some(&*shell_name));
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
            default_shell_string(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        None,
        true,
        Some(&["TERM_PROGRAM=tmux".to_owned()]),
        None,
        None,
    )
    .expect("profile");

    let shell_name = default_shell_name();
    assert_eq!(profile.default_window_name().as_deref(), Some(&*shell_name));
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
            default_shell_string(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new("/tmp/rmux.sock"),
        None,
        true,
        None,
        None,
        None,
    )
    .expect("profile");

    let shell_name = default_shell_name();
    assert_eq!(
        profile.runtime_window_name(None).as_deref(),
        Some(&*shell_name)
    );
    assert_eq!(
        profile
            .runtime_window_name(Some(&rmux_proto::ProcessCommand::Shell(
                "printf hi".to_owned(),
            )))
            .as_deref(),
        Some("printf")
    );
    assert_eq!(
        profile
            .runtime_window_name(Some(&rmux_proto::ProcessCommand::Shell(
                "exit 0".to_owned()
            )))
            .as_deref(),
        Some("exit")
    );
    assert_eq!(
        profile
            .runtime_window_name(Some(&rmux_proto::ProcessCommand::Argv(vec![
                "/usr/bin/top".to_owned(),
                "-H".to_owned(),
            ])))
            .as_deref(),
        Some("top")
    );
    assert_eq!(
        profile.automatic_window_name(None).as_deref(),
        Some(&*shell_name)
    );
    assert_eq!(
        profile
            .automatic_window_name(Some(&rmux_proto::ProcessCommand::Shell(
                "sleep 30".to_owned(),
            )))
            .as_deref(),
        Some("sleep")
    );
}

#[test]
fn explicit_empty_process_commands_are_rejected() {
    for command in [
        ProcessCommand::Argv(Vec::new()),
        ProcessCommand::Argv(vec![String::new()]),
    ] {
        let error = validate_process_command(Some(&command))
            .expect_err("explicit empty process commands must be rejected");
        assert!(
            error
                .to_string()
                .contains("process command must not be empty"),
            "unexpected validation error: {error}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_structured_argv_rejects_batch_programs_before_shell_interpretation() {
    for extension in ["cmd", "CMD", "bat", "BAT"] {
        let command = ProcessCommand::Argv(vec![
            format!(r"C:\Users\RMUX User\scripts\probe.{extension}"),
            "two words".to_owned(),
            "quote\"value".to_owned(),
            "%RMUX_SENTINEL%".to_owned(),
            "bang!value".to_owned(),
            "amp&value".to_owned(),
            "pipe|value".to_owned(),
            "less<value".to_owned(),
            "greater>value".to_owned(),
            "caret^value".to_owned(),
            String::new(),
        ]);

        let error = validate_process_command(Some(&command))
            .expect_err("Windows batch argv must be rejected before cmd.exe can reinterpret it");
        assert!(matches!(error, RmuxError::SpawnFailed { .. }));
        assert_eq!(
            error.to_string(),
            super::WINDOWS_BATCH_ARGV_UNSUPPORTED_MESSAGE
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_structured_argv_keeps_native_executables_direct() {
    for extension in ["exe", "EXE", "com", "COM"] {
        validate_process_command(Some(&ProcessCommand::Argv(vec![format!(
            r"C:\Tools\native.{extension}"
        )])))
        .expect("native Windows executables remain valid structured argv targets");
    }
}

#[cfg(windows)]
#[test]
fn windows_structured_argv_rejects_pathext_resolved_batch_shims() -> Result<(), Box<dyn Error>> {
    let root = unique_directory("batch-argv-shim")?;
    let bin = root.join("bin");
    fs::create_dir_all(&bin)?;
    fs::write(root.join("opencode.cmd"), "@echo off\r\nexit /b 0\r\n")?;
    fs::write(root.join("absolute-shim.cmd"), "@echo off\r\nexit /b 0\r\n")?;
    fs::write(bin.join("relative-shim.bat"), "@echo off\r\nexit /b 0\r\n")?;
    fs::write(root.join("native.exe"), b"")?;
    fs::write(bin.join("native.com"), b"")?;
    let environment = EnvironmentStore::new();
    let options = OptionStore::new();
    let session_name = SessionName::new("batch-shim").expect("valid session name");
    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        Path::new(r"\\.\pipe\rmux-batch-argv-test"),
        None,
        true,
        Some(&[
            format!("PATH={}", root.display()),
            "PATHEXT=.COM;.EXE;.BAT;.CMD".to_owned(),
        ]),
        None,
        Some(&root),
    )?;

    for (program, shape) in [
        (PathBuf::from("opencode"), "bare PATH shim"),
        (root.join("absolute-shim"), "absolute extensionless shim"),
        (
            PathBuf::from("bin").join("relative-shim"),
            "relative extensionless shim",
        ),
    ] {
        let error = super::validate_windows_process_command_for_profile(
            &profile,
            Some(&ProcessCommand::Argv(vec![program
                .to_string_lossy()
                .into_owned()])),
        )
        .expect_err("PATHEXT-resolved batch shim must not cross into cmd.exe argv parsing");
        assert!(matches!(error, RmuxError::SpawnFailed { .. }), "{shape}");
        assert_eq!(
            error.to_string(),
            super::WINDOWS_BATCH_ARGV_UNSUPPORTED_MESSAGE,
            "{shape}"
        );
    }

    for program in [root.join("native"), PathBuf::from("bin").join("native")] {
        super::validate_windows_process_command_for_profile(
            &profile,
            Some(&ProcessCommand::Argv(vec![program
                .to_string_lossy()
                .into_owned()])),
        )
        .expect("extensionless native .exe/.com target remains structured argv");
    }

    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_structured_argv_does_not_treat_windows_batch_suffixes_specially() {
    for extension in ["cmd", "bat"] {
        validate_process_command(Some(&ProcessCommand::Argv(vec![format!(
            "/tmp/native-name.{extension}"
        )])))
        .expect("Unix argv targets remain direct regardless of Windows-style suffixes");
    }
}

#[test]
fn empty_shell_process_command_is_allowed_for_empty_tmux_panes() {
    validate_process_command(Some(&ProcessCommand::Shell(String::new())))
        .expect("empty shell command creates a tmux-style empty pane");
}

#[cfg(unix)]
#[test]
fn resolve_shell_path_prefers_explicit_default_shell_option_before_shell_env_fallback() {
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let environment = HashMap::from([("SHELL".to_owned(), "/bin/sh".to_owned())]);
    options
        .set(
            ScopeSelector::Session(session_name.clone()),
            OptionName::DefaultShell,
            "/bin/bash".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let resolved = super::resolve_shell_path(&options, Some(&session_name), &environment);

    assert_eq!(
        resolved,
        super::shell_resolver::normalize_shell_path(PathBuf::from("/bin/bash"))
    );
}

#[cfg(unix)]
#[test]
fn resolve_shell_path_uses_shell_env_when_default_shell_is_explicitly_empty() {
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    let environment = HashMap::from([("SHELL".to_owned(), "/bin/sh".to_owned())]);
    options
        .set(
            ScopeSelector::Session(session_name.clone()),
            OptionName::DefaultShell,
            String::new(),
            SetOptionMode::Replace,
        )
        .expect("default-shell accepts an empty override");

    let resolved = super::resolve_shell_path(&options, Some(&session_name), &environment);

    assert_eq!(
        resolved,
        super::shell_resolver::normalize_shell_path(PathBuf::from("/bin/sh"))
    );
}

#[cfg(windows)]
#[test]
fn resolve_shell_path_uses_stable_windows_default_shell() {
    let options = OptionStore::new();
    let environment = HashMap::from([
        (
            "PATH".to_owned(),
            std::env::var("COMSPEC")
                .ok()
                .and_then(|value| Path::new(&value).parent().map(Path::to_path_buf))
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32"))
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "SystemRoot".to_owned(),
            std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_owned()),
        ),
    ]);

    let resolved = super::resolve_shell_path(&options, None, &environment);
    let leaf = super::executable_name(resolved.as_os_str())
        .expect("resolved shell has a leaf")
        .to_ascii_lowercase();

    assert_eq!(
        leaf, "cmd.exe",
        "expected Windows server fallback shell to prefer COMSPEC/cmd without a client shell hint, got {resolved:?}"
    );
}

#[cfg(windows)]
#[test]
fn terminal_profile_consumes_windows_client_shell_hint_without_exporting_it() {
    let environment = EnvironmentStore::new();
    let options = OptionStore::new();
    let session_name = SessionName::new("client-shell").expect("valid session name");
    let spawn_environment = HashMap::from([(
        super::shell_resolver::CLIENT_SHELL_ENV.to_owned(),
        "cmd.exe".to_owned(),
    )]);
    let raw_environment = vec![(
        OsString::from(super::shell_resolver::CLIENT_SHELL_ENV),
        OsString::from("cmd.exe"),
    )];

    let profile = TerminalProfile::for_initial_session_pane(
        &environment,
        &options,
        &session_name,
        1,
        Path::new(r"\\.\pipe\rmux-test"),
        Some(&spawn_environment),
        Some(&raw_environment),
        true,
        None,
        None,
        None,
    )
    .expect("profile builds");
    let leaf = profile
        .shell()
        .file_name()
        .expect("shell has a leaf")
        .to_string_lossy()
        .to_ascii_lowercase();

    assert_eq!(leaf, "cmd.exe");
    assert_eq!(
        profile.environment_value(super::shell_resolver::CLIENT_SHELL_ENV),
        None
    );
}

#[cfg(windows)]
#[test]
fn resolve_shell_path_respects_explicit_windows_cmd_default_shell() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "cmd.exe".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");

    let resolved = super::resolve_shell_path(&options, None, &HashMap::new());
    let leaf = super::executable_name(resolved.as_os_str())
        .expect("resolved shell has a leaf")
        .to_ascii_lowercase();

    assert_eq!(leaf, "cmd.exe");
}

#[cfg(windows)]
#[test]
fn resolve_shell_path_prefers_session_shell_over_global_on_windows() {
    let mut options = OptionStore::new();
    let session_name = SessionName::new("alpha").expect("valid session name");
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "powershell.exe".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("global default-shell succeeds");
    options
        .set(
            ScopeSelector::Session(session_name.clone()),
            OptionName::DefaultShell,
            "cmd.exe".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("session default-shell succeeds");

    let resolved = super::resolve_shell_path(&options, Some(&session_name), &HashMap::new());
    let leaf = super::executable_name(resolved.as_os_str())
        .expect("resolved shell has a leaf")
        .to_ascii_lowercase();

    assert_eq!(leaf, "cmd.exe");
}

#[cfg(windows)]
#[test]
fn windows_interactive_cmd_starts_in_profile_cwd_and_accepts_input() -> Result<(), Box<dyn Error>> {
    windows_interactive_shell_starts_in_profile_cwd_and_accepts_input("cmd.exe")
}

#[cfg(windows)]
#[test]
fn windows_interactive_pwsh_starts_in_profile_cwd_and_accepts_input() -> Result<(), Box<dyn Error>>
{
    windows_interactive_shell_starts_in_profile_cwd_and_accepts_input("pwsh.exe")
}

#[cfg(windows)]
#[test]
fn windows_interactive_powershell_starts_in_profile_cwd_and_accepts_input(
) -> Result<(), Box<dyn Error>> {
    let powershell = Path::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    if !powershell.is_file() {
        eprintln!(
            "skipping Windows PowerShell interactive probe: {} missing",
            powershell.display()
        );
        return Ok(());
    }

    windows_interactive_shell_starts_in_profile_cwd_and_accepts_input(
        &powershell.display().to_string(),
    )
}

#[cfg(windows)]
fn reap_windows_test_child(child: &mut rmux_pty::PtyChild) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }

    child.terminate_forcefully()?;
    let _ = child.wait()?;
    Ok(())
}

#[cfg(windows)]
const WINDOWS_INTERACTIVE_SHELL_OUTPUT_TIMEOUT: Duration = Duration::from_secs(20);

#[cfg(windows)]
fn windows_interactive_shell_starts_in_profile_cwd_and_accepts_input(
    default_shell: &str,
) -> Result<(), Box<dyn Error>> {
    let environment = EnvironmentStore::new();
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            default_shell.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell succeeds");
    let session_name = SessionName::new("alpha").expect("valid session name");
    let cwd = unique_directory("windows-interactive-shell")?;
    let profile = TerminalProfile::for_session(
        &environment,
        &options,
        &session_name,
        7,
        temp_socket_path().as_path(),
        None,
        true,
        None,
        Some(rmux_core::PaneId::new(3)),
        Some(cwd.as_path()),
    )?;

    let (master, mut child) = super::spawn_pane_process(
        &rmux_pty::ProcessShell,
        PtyTerminalSize::new(100, 30),
        &profile,
        None,
    )?;
    let io = master.try_clone_io()?;
    let cwd_markers = windows_path_markers(&cwd);

    let output = (|| -> Result<Vec<u8>, Box<dyn Error>> {
        let lower_default_shell = default_shell.to_ascii_lowercase();
        if lower_default_shell.contains("powershell") || lower_default_shell.contains("pwsh") {
            io.write_all(b"Get-Location; Write-Output RMUX_WINDOWS_INTERACTIVE_OK; exit\r\n")?;
        } else {
            io.write_all(b"cd && echo RMUX_WINDOWS_INTERACTIVE_OK && exit\r\n")?;
        }
        read_until_io(
            &io,
            b"RMUX_WINDOWS_INTERACTIVE_OK",
            WINDOWS_INTERACTIVE_SHELL_OUTPUT_TIMEOUT,
        )
    })();

    reap_windows_test_child(&mut child)?;
    fs::remove_dir_all(&cwd)?;

    let output = output?;
    let output = String::from_utf8_lossy(&output);
    let unwrapped_output = output.replace(['\r', '\n'], "");
    let folded_output = unwrapped_output.to_ascii_lowercase();
    assert!(
        cwd_markers
            .iter()
            .any(|marker| folded_output.contains(&marker.to_ascii_lowercase())),
        "expected Windows shell command to start in one of {cwd_markers:?}, got {output:?}"
    );
    assert!(
        output.contains("RMUX_WINDOWS_INTERACTIVE_OK"),
        "expected Windows interactive input marker, got {output:?}"
    );
    Ok(())
}

#[cfg(windows)]
fn windows_path_markers(path: &Path) -> Vec<String> {
    let mut markers = vec![path.display().to_string()];
    if let Ok(canonical) = fs::canonicalize(path) {
        let rendered = canonical.display().to_string();
        let normalized = if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{rest}")
        } else {
            rendered
                .strip_prefix(r"\\?\")
                .unwrap_or(&rendered)
                .to_owned()
        };
        if !markers
            .iter()
            .any(|marker| marker.eq_ignore_ascii_case(&normalized))
        {
            markers.push(normalized);
        }
    }
    markers
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

fn expected_mux_env(socket_path: &Path, index: u32) -> String {
    let absolute = if socket_path.is_absolute() {
        socket_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(socket_path))
            .unwrap_or_else(|_| socket_path.to_path_buf())
    };
    let socket_path = if let Ok(canonical) = fs::canonicalize(&absolute) {
        canonical
    } else {
        match (absolute.parent(), absolute.file_name()) {
            (Some(parent), Some(file_name)) => fs::canonicalize(parent)
                .map(|canonical_parent| canonical_parent.join(file_name))
                .unwrap_or(absolute),
            _ => absolute,
        }
    };
    format!("{},{},{}", socket_path.display(), std::process::id(), index)
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

fn default_shell_name() -> String {
    let shell = default_shell_string();
    Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.trim_start_matches('-').to_owned())
        .filter(|name| !name.is_empty())
        .expect("test default shell has a file name")
}

fn hook_write_command(path: &Path, text: &str) -> String {
    #[cfg(unix)]
    {
        format!("printf {} > {}", shell_quote(text), shell_quote_path(path))
    }
    #[cfg(windows)]
    {
        crate::test_shell::powershell_encoded_command(&format!(
            "[IO.File]::WriteAllText({}, {})",
            powershell_quote_path(path),
            powershell_quote(text)
        ))
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
            r#"<nul set /p "={first}">"{}" & <nul set /p "={second}">>"{}""#,
            path.display(),
            path.display()
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
    let deadline = std::time::Instant::now() + test_process_deadline();
    while std::time::Instant::now() < deadline {
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

fn test_process_deadline() -> Duration {
    if cfg!(windows) {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(2)
    }
}

#[cfg(windows)]
fn read_until_io(
    io: &rmux_pty::PtyIo,
    needle: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    let reader = io.try_clone()?;
    let expected = String::from_utf8_lossy(needle).into_owned();
    let needle = needle.to_vec();
    let (sender, receiver) = mpsc::channel();
    let partial_output = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let reader_partial_output = std::sync::Arc::clone(&partial_output);

    thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0_u8; 4096];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(Ok(output));
                    return;
                }
                Ok(bytes_read) => {
                    output.extend_from_slice(&buffer[..bytes_read]);
                    if let Ok(mut partial) = reader_partial_output.lock() {
                        partial.clear();
                        partial.extend_from_slice(&output);
                    }
                    if output
                        .windows(needle.len())
                        .any(|window| window == needle.as_slice())
                    {
                        let _ = sender.send(Ok(output));
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error));
                    return;
                }
            }
        }
    });

    match receiver.recv_timeout(timeout) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(error.into()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let partial = partial_output
                .lock()
                .map(|output| output.clone())
                .unwrap_or_default();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "pty output did not contain {expected:?} within {timeout:?}; partial output: {:?}",
                    String::from_utf8_lossy(&partial)
                ),
            )
            .into())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("pty reader thread stopped before sending output").into())
        }
    }
}

#[cfg(unix)]
#[test]
fn unix_pane_shell_command_uses_basename_and_preserves_context() -> Result<(), Box<dyn Error>> {
    let (output, cwd) = unix_shell_context_witness(true)?;
    assert_eq!(output, format!("sh\n{cwd}\nvalue with spaces;*\n"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn unix_generic_shell_command_keeps_path_and_preserves_context() -> Result<(), Box<dyn Error>> {
    let (output, cwd) = unix_shell_context_witness(false)?;
    assert_eq!(output, format!("/bin/sh\n{cwd}\nvalue with spaces;*\n"));
    Ok(())
}

#[cfg(unix)]
fn unix_shell_context_witness(pane: bool) -> Result<(String, String), Box<dyn Error>> {
    let cwd = unique_output_path(if pane {
        "pane-shell-context"
    } else {
        "generic-shell-context"
    });
    fs::create_dir(&cwd)?;
    let output_path = cwd.join("witness");
    let result = (|| -> Result<(String, String), Box<dyn Error>> {
        let environment = EnvironmentStore::new();
        let mut options = OptionStore::new();
        options.set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/sh".to_owned(),
            SetOptionMode::Replace,
        )?;
        let profile = TerminalProfile::for_session(
            &environment,
            &options,
            &SessionName::new("shell-context")?,
            7,
            &cwd.join("unused.sock"),
            None,
            true,
            Some(&["RMUX_TEST_SHELL_WITNESS=value with spaces;*".to_owned()]),
            Some(rmux_core::PaneId::new(3)),
            Some(&cwd),
        )?;
        let command = format!(
            "printf '%s\\n' \"$0\" \"$PWD\" \"$RMUX_TEST_SHELL_WITNESS\" > {}",
            shell_quote_path(&output_path)
        );
        let (master, mut child) = if pane {
            // Exercise the real pane dispatch, not just the dedicated constructor.
            super::spawn_pane_process(
                &rmux_pty::ProcessShell,
                rmux_pty::TerminalSize::new(80, 24),
                &profile,
                Some(&ProcessCommand::Shell(command)),
            )?
        } else {
            let mut child_command = profile
                .shell_child_command(&command)
                .clear_env()
                .current_dir(profile.cwd());
            for (name, value) in profile.raw_environment() {
                child_command = child_command.env(name, value);
            }
            child_command.spawn()?.into_parts()
        };
        #[cfg(target_os = "linux")]
        println!(
            "shell-context child pid={} stat={:?}",
            child.pid().as_u32(),
            fs::read_to_string(format!("/proc/{}/stat", child.pid().as_u32()))
        );
        let status = wait_for_unix_shell_witness(&mut child);
        drop(master);
        let status = status?;
        assert!(status.success(), "shell witness failed: {status}");
        let bytes = fs::read(&output_path)?;
        assert!(bytes.len() < 4096, "unexpected shell witness output volume");
        let output = String::from_utf8(bytes)?;
        println!("shell-context output={output:?}");
        Ok((output, cwd.to_string_lossy().into_owned()))
    })();
    fs::remove_dir_all(&cwd)?;
    result
}

#[cfg(unix)]
fn wait_for_unix_shell_witness(
    child: &mut rmux_pty::PtyChild,
) -> Result<std::process::ExitStatus, Box<dyn Error>> {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(4) {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // This handle still owns an unreaped child, so its PID cannot have been reused.
    child.terminate_forcefully()?;
    while start.elapsed() < Duration::from_secs(5) {
        if child.try_wait()?.is_some() {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "shell witness exceeded 4s").into(),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "shell witness cleanup exceeded 5s").into())
}

#[cfg(windows)]
#[test]
fn lone_windows_program_path_runs_without_the_shell() {
    let program = std::env::current_exe().expect("test binary path");
    assert!(super::is_lone_windows_program(&program.to_string_lossy()));
    assert!(!super::is_lone_windows_program(
        r"C:\no such dir\missing.exe"
    ));
    assert!(!super::is_lone_windows_program("cmd /c echo hi"));
}

#[test]
fn shell_command_window_name_keeps_a_program_path_with_spaces_whole() {
    let program = std::env::current_exe().expect("test binary path");
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let quoted = format!("\"{}\" -x", program.display());

    assert_eq!(super::shell_command_window_name(&quoted), name);
    assert_eq!(
        super::shell_command_window_name("vim notes.txt").as_deref(),
        Some("vim")
    );
    #[cfg(windows)]
    assert_eq!(
        super::shell_command_window_name(&program.to_string_lossy()),
        name
    );
}
