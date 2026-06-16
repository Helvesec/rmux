use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_proto::{
    DisplayMessageRequest, HookLifecycle, HookName, KillSessionRequest, KillWindowRequest,
    LastWindowRequest, LinkWindowRequest, ListPanesRequest, ListWindowsRequest, MoveWindowRequest,
    MoveWindowTarget, NewSessionExtRequest, NewSessionRequest, NewWindowRequest, NextWindowRequest,
    OptionName, PaneTarget, PreviousWindowRequest, RenameSessionRequest, RenameWindowRequest,
    Request, ResizeWindowAdjustment, ResizeWindowRequest, ResolveTargetRequest, ResolveTargetType,
    RespawnWindowRequest, Response, RotateWindowDirection, RotateWindowRequest, ScopeSelector,
    SelectWindowRequest, SessionName, SetOptionMode, SetOptionRequest, SplitDirection,
    SplitWindowRequest, SplitWindowTarget, SwapWindowRequest, Target, TerminalSize,
    UnlinkWindowRequest, WindowTarget,
};
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::{timeout, Duration};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
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
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
}

async fn create_grouped_session(handler: &RequestHandler, name: &str, group_target: &SessionName) {
    let created = handler
        .handle(Request::NewSessionExt(NewSessionExtRequest {
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
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
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
                pane_alert_callback: None,
                pane_exit_callback: None,
            },
        )
        .expect("window terminal insert succeeds");
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

#[path = "handler_window_tests/swap_rotate.rs"]
mod swap_rotate;

#[path = "handler_window_tests/link_unlink.rs"]
mod link_unlink;

#[path = "handler_window_tests/active_selection.rs"]
mod active_selection;

#[path = "handler_window_tests/resize_respawn.rs"]
mod resize_respawn;
