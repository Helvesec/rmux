use super::super::RequestHandler;
use super::session_name;
use crate::copy_mode::CopyPosition;
use crate::input_keys::{encode_key, encode_mouse_event, ExtendedKeyFormat, MouseForwardEvent};
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use rmux_core::{input::mode, key_string_lookup_string};
use rmux_proto::{
    BindKeyRequest, CopyModeRequest, ErrorResponse, ListKeysRequest, ListPanesRequest,
    NewSessionRequest, OptionName, PaneTarget, Request, Response, RmuxError, ScopeSelector,
    SendKeysExtRequest, SendKeysRequest, SendKeysResponse, SendPrefixRequest, SetOptionMode,
    SetOptionRequest, ShowBufferRequest, SwitchClientExtRequest, TerminalSize, UnbindKeyRequest,
    WindowTarget,
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

#[path = "handler_send_keys_tests/basic_dispatch.rs"]
mod basic_dispatch;

#[path = "handler_send_keys_tests/bindings_timeouts.rs"]
mod bindings_timeouts;

#[path = "handler_send_keys_tests/input_capture.rs"]
mod input_capture;
use input_capture::{PaneInputCapture, RawPaneInputProbe};

#[path = "handler_send_keys_tests/live_attach.rs"]
mod live_attach;

#[path = "handler_send_keys_tests/mouse_copy_mode.rs"]
mod mouse_copy_mode;

async fn handle_boxed(handler: &RequestHandler, request: Request) -> Response {
    Box::pin(handler.handle(request)).await
}
