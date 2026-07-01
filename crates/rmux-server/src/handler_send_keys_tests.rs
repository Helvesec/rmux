use super::super::RequestHandler;
use super::session_name;
use crate::input_keys::{encode_key, encode_mouse_event, ExtendedKeyFormat, MouseForwardEvent};
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use rmux_core::{input::mode, key_string_lookup_string};
use rmux_proto::{
    BindKeyRequest, CopyModeRequest, ErrorResponse, ListKeysRequest, ListPanesRequest,
    NewSessionRequest, OptionName, PaneBroadcastInputRequest, PaneId, PaneTarget, PaneTargetRef,
    Request, Response, RmuxError, ScopeSelector, SelectPaneRequest, SendKeysExtRequest,
    SendKeysRequest, SendKeysResponse, SendPrefixRequest, SendPrefixResponse, SetOptionMode,
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

#[path = "handler_send_keys_tests/bracketed_paste_live.rs"]
mod bracketed_paste_live;

#[path = "handler_send_keys_tests/bracketed_paste_large.rs"]
mod bracketed_paste_large;

#[path = "handler_send_keys_tests/kitty_graphics_live.rs"]
mod kitty_graphics_live;

#[path = "handler_send_keys_tests/synchronize_panes.rs"]
mod synchronize_panes;

#[path = "handler_send_keys_tests/attached_input_bounds.rs"]
mod attached_input_bounds;

#[path = "handler_send_keys_tests/mouse_copy_mode.rs"]
mod mouse_copy_mode;

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
