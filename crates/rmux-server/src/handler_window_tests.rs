use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_proto::{
    DisplayMessageRequest, HookLifecycle, HookName, KillSessionRequest, KillWindowRequest,
    LastWindowRequest, LinkWindowRequest, ListPanesRequest, ListWindowsRequest, MoveWindowRequest,
    MoveWindowTarget, NewSessionExtRequest, NewSessionRequest, NewWindowRequest, NextWindowRequest,
    OptionName, PaneTarget, PreviousWindowRequest, ProcessCommand, RenameSessionRequest,
    RenameWindowRequest, Request, ResizeWindowAdjustment, ResizeWindowRequest,
    ResolveTargetRequest, ResolveTargetType, RespawnWindowRequest, Response, RotateWindowDirection,
    RotateWindowRequest, ScopeSelector, SelectWindowRequest, SessionName, SetOptionMode,
    SetOptionRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget, SwapWindowRequest,
    Target, TerminalSize, UnlinkWindowRequest, WindowTarget,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn unique_window_temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-window-{label}-{}-{unique}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn window_shell_quote(path: &Path) -> String {
    crate::test_shell::sh_quote_path(path)
}

#[cfg(unix)]
fn window_respawn_replay_command(output: &Path, tag: &str) -> String {
    format!(
        "printf '%s:%s:{tag}\\n' \"$(pwd)\" \"$RMUX_RESPAWN\" >> {}; sleep 60",
        window_shell_quote(output)
    )
}

#[cfg(unix)]
fn window_respawn_shell_identity_command(output: &Path, tag: &str) -> String {
    format!(
        "printf '%s:%s:{tag}\\n' \"${{0##*/}}\" \"$SHELL\" >> {}; sleep 60",
        window_shell_quote(output)
    )
}

#[cfg(windows)]
fn window_respawn_replay_command(output: &Path, tag: &str) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "[System.IO.File]::AppendAllText({}, ((Get-Location).Path + ':' + $env:RMUX_RESPAWN + ':{}' + [char]10)); Start-Sleep -Seconds 60",
        crate::test_shell::powershell_quote_path(output),
        tag
    ))
}

#[cfg(windows)]
fn expected_window_spawn_cwd(path: &Path) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let rendered = canonical.display().to_string();
    if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        rendered
            .strip_prefix(r"\\?\")
            .unwrap_or(&rendered)
            .to_owned()
    }
}

#[cfg(unix)]
fn expected_window_spawn_cwd(path: &Path) -> String {
    path.display().to_string()
}

async fn wait_for_window_file_contents(path: &Path, expected: &str) {
    #[cfg(windows)]
    let timeout = Duration::from_secs(20);
    #[cfg(not(windows))]
    let timeout = Duration::from_secs(5);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return,
            Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(contents) => panic!(
                "timed out waiting for {} to contain {:?}, got {:?}",
                path.display(),
                expected,
                contents
            ),
            Err(error) => panic!(
                "timed out waiting for {} to exist with {:?}: {error}",
                path.display(),
                expected
            ),
        }
    }
}

fn window_respawn_probe_line(cwd: &str, environment: &str, tag: &str) -> String {
    format!("{cwd}:{environment}:{tag}\n")
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name(name)),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
}

async fn create_grouped_session(handler: &RequestHandler, name: &str, group_target: &SessionName) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name(name)),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
            group_target: Some(group_target.clone()),
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
}

async fn enable_global_monitor_silence(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::MonitorSilence,
            value: "60".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

#[cfg(unix)]
async fn set_window_test_default_shell(handler: &RequestHandler, shell: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultShell,
            value: shell.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

#[cfg(unix)]
fn quiet_window_test_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_window_test_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    [
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 60 127.0.0.1 >NUL".to_owned(),
    ]
    .into_iter()
    .collect()
}

async fn insert_window(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    let mut state = handler.state.lock().await;
    let pane_id = state.sessions.allocate_pane_id();
    {
        let session = state
            .sessions
            .session_mut(session_name)
            .expect("session should exist");
        session
            .insert_window_with_initial_pane_with_id(
                window_index,
                TerminalSize { cols: 90, rows: 30 },
                pane_id,
            )
            .expect("window insert succeeds");
    }
    state
        .insert_window_terminal(
            session_name,
            window_index,
            crate::pane_terminals::WindowSpawnOptions {
                start_directory: None,
                command: None,
                socket_path: Path::new("/tmp/rmux-test.sock"),
                spawn_environment: None,
                environment_overrides: None,
                respawn_shell: None,
                respawn_environment: None,
                pane_alert_callback: None,
                pane_exit_callback: None,
            },
        )
        .expect("window terminal insert succeeds");
}

async fn link_duplicate_window(
    handler: &RequestHandler,
    session_name: &SessionName,
    source_index: u32,
    destination_index: u32,
) {
    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(session_name.clone(), source_index),
            target: WindowTarget::with_window(session_name.clone(), destination_index),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
}

fn assert_refresh(control: AttachControl) {
    assert!(matches!(control, AttachControl::Switch(_)));
}

async fn drain_attach_controls(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(now);
        let idle = remaining.min(Duration::from_millis(250));
        match timeout(idle, control_rx.recv()).await {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
}

#[path = "handler_window_tests/lifecycle.rs"]
mod lifecycle;

#[path = "handler_window_tests/renumber.rs"]
mod renumber;

#[path = "handler_window_tests/listing_refresh.rs"]
mod listing_refresh;

#[path = "handler_window_tests/move_window.rs"]
mod move_window;

#[path = "handler_window_tests/relative_group_transactions.rs"]
mod relative_group_transactions;
#[path = "handler_window_tests/relative_metadata.rs"]
mod relative_metadata;

#[path = "handler_window_tests/swap_rotate.rs"]
mod swap_rotate;

#[path = "handler_window_tests/silence_fanout.rs"]
mod silence_fanout;

#[path = "handler_window_tests/link_unlink.rs"]
mod link_unlink;

#[path = "handler_window_tests/active_selection.rs"]
mod active_selection;

#[path = "handler_window_tests/resize_respawn.rs"]
mod resize_respawn;

#[path = "handler_window_tests/respawn_linked_refresh.rs"]
mod respawn_linked_refresh;

#[path = "handler_window_tests/respawn_linked_guard.rs"]
mod respawn_linked_guard;
