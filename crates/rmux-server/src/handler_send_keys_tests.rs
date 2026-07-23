use super::super::RequestHandler;
use super::session_name;
use crate::input_keys::{
    encode_key, encode_mouse_event, ExtendedKeyFormat, MouseForwardEvent, MAX_SGR_MOUSE_FRAME_BYTES,
};
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use rmux_core::{input::mode, key_string_lookup_string};
use rmux_proto::{
    BindKeyRequest, CopyModeRequest, ErrorResponse, HookLifecycle, HookName, ListKeysRequest,
    ListPanesRequest, NewSessionExtRequest, NewSessionRequest, OptionName,
    PaneBroadcastInputRequest, PaneId, PaneTarget, PaneTargetRef, Request, Response, RmuxError,
    ScopeSelector, SelectPaneRequest, SendKeysExtRequest, SendKeysRequest, SendKeysResponse,
    SendPrefixRequest, SendPrefixResponse, SetHookMutationRequest, SetHookRequest, SetOptionMode,
    SetOptionRequest, ShowBufferRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
    SwitchClientExtRequest, TerminalSize, UnbindKeyRequest, WindowTarget, DEFAULT_MAX_FRAME_LENGTH,
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

#[path = "handler_send_keys_tests/basic_dispatch.rs"]
mod basic_dispatch;

#[path = "handler_send_keys_tests/target_client.rs"]
mod target_client;

#[path = "handler_send_keys_tests/bindings_timeouts.rs"]
mod bindings_timeouts;

use super::super::input_capture::RawPaneInputProbe;

#[path = "handler_send_keys_tests/live_attach.rs"]
mod live_attach;

#[path = "handler_send_keys_tests/kitty_keyboard.rs"]
mod kitty_keyboard;

#[cfg(windows)]
#[path = "handler_send_keys_tests/windows_console_repeat.rs"]
mod windows_console_repeat;

#[path = "handler_send_keys_tests/bracketed_paste_live.rs"]
mod bracketed_paste_live;

#[path = "handler_send_keys_tests/bracketed_paste_large.rs"]
mod bracketed_paste_large;

#[path = "handler_send_keys_tests/kitty_graphics_live.rs"]
mod kitty_graphics_live;

#[path = "handler_send_keys_tests/palette_modal.rs"]
mod palette_modal;

#[path = "handler_send_keys_tests/synchronize_panes.rs"]
mod synchronize_panes;

#[path = "handler_send_keys_tests/attached_input_bounds.rs"]
mod attached_input_bounds;

#[path = "handler_send_keys_tests/mouse_copy_mode.rs"]
mod mouse_copy_mode;

#[path = "handler_send_keys_tests/copy_mode_mouse_origin.rs"]
mod copy_mode_mouse_origin;

#[path = "handler_send_keys_tests/copy_mode_vi.rs"]
mod copy_mode_vi;

async fn handle_boxed(handler: &RequestHandler, request: Request) -> Response {
    Box::pin(handler.handle(request)).await
}

async fn create_send_keys_test_session(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
) {
    #[cfg(unix)]
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                "/bin/bash".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("test default-shell is valid");
    }

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
}

async fn spawn_accounted_attach_control_drain(
    handler: &RequestHandler,
    requester_pid: u32,
    mut control_rx: mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) -> tokio::task::JoinHandle<()> {
    let control_backlog = {
        let active_attach = handler.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client exists")
            .control_backlog
            .clone()
    };
    tokio::spawn(async move {
        while let Some(control) = control_rx.recv().await {
            crate::pane_io::release_attach_control_backlog(
                &control_backlog,
                control.received_backlog_units(),
            );
        }
    })
}

// A pane command that stays alive but emits nothing to the transcript, so a
// test that asserts on rendered content is not racing the real login shell's
// prompt (cmd.exe prints `C:\Users\...`, bash prints `PS1`) into the same
// cells the test wrote. Mirrors the quiet command used by the alert tests.
#[cfg(unix)]
fn quiet_pane_command() -> Vec<String> {
    ["/bin/sh", "-c", "sleep 60"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn quiet_pane_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

// Like create_send_keys_test_session but the pane runs an inert, silent command
// and we block until its terminal has finished starting, so a subsequent
// transcript write is the only content in the pane.
async fn create_quiet_input_session(handler: &RequestHandler, session: &rmux_proto::SessionName) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_pane_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::new(session.clone(), 0))
        .await;
}
