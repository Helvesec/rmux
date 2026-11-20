use super::{
    after_hook_format_values, AccessMode, RequestHandler, ServerAccessStore, DEFAULT_SESSION_SIZE,
};
use crate::control::ControlModeUpgrade;
use crate::daemon::ShutdownHandle;
use crate::pane_io::AttachControl;
use rmux_core::{
    command_parser::parse_command_string, PaneGeometry, WINDOW_ALERTFLAGS, WINLINK_ALERTFLAGS,
};
use rmux_ipc::PeerIdentity;
use rmux_os::identity::UserIdentity;
use rmux_proto::{
    ControlMode, DisplayMessageRequest, ErrorResponse, HasSessionRequest, HookLifecycle, HookName,
    KillPaneRequest, KillSessionRequest, LayoutName, ListPanesRequest, ListSessionsRequest,
    NewSessionExtRequest, NewSessionRequest, OptionName, PaneTarget, RenameSessionRequest, Request,
    ResizePaneAdjustment, ResizePaneTargetActionRequest, Response, RmuxError, ScopeSelector,
    SelectPaneRequest, SessionName, SetHookRequest, SetOptionMode, SetOptionRequest,
    SplitWindowIdentityRequest, SplitWindowRequest, SplitWindowTarget,
    SplitWindowTargetActionRequest, Target, TerminalSize,
};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[path = "handler_tests/new_session.rs"]
mod new_session;

#[path = "handler_tests/kill_session.rs"]
mod kill_session;

#[path = "handler_tests/session_leases.rs"]
mod session_leases;

#[path = "handler_tests/clients.rs"]
mod clients;

#[path = "handler_tests/list_clients_session_id.rs"]
mod list_clients_session_id;

#[path = "handler_tests/list_clients_session_group.rs"]
mod list_clients_session_group;

#[path = "handler_tests/panes.rs"]
mod panes;

#[path = "handler_tests/split_before.rs"]
mod split_before;

#[path = "handler_tests/rename_session.rs"]
mod rename_session;

#[path = "handler_tests/rename_session_retained_output.rs"]
mod rename_session_retained_output;

#[path = "handler_tests/lists_and_hooks.rs"]
mod lists_and_hooks;

#[path = "handler_tests/poisoned_pane_selection.rs"]
mod poisoned_pane_selection;

#[path = "handler_tests/poisoned_window_selection.rs"]
mod poisoned_window_selection;

#[path = "handler_tests/poisoned_transcript.rs"]
mod poisoned_transcript;

#[path = "handler_tests/poisoned_transcript_matrix.rs"]
mod poisoned_transcript_matrix;

#[path = "handler_tests/poisoned_transcript_reach.rs"]
mod poisoned_transcript_reach;

#[path = "handler_tests/poisoned_transcript_named_commands.rs"]
mod poisoned_transcript_named_commands;

#[path = "handler_tests/poisoned_transcript_transfer.rs"]
mod poisoned_transcript_transfer;

#[path = "handler_send_keys_tests.rs"]
mod send_keys_tests;

#[path = "handler_copy_mode_tests.rs"]
mod copy_mode_tests;

#[path = "handler_overlay_tests.rs"]
mod overlay_tests;

#[test]
fn access_mode_for_peer_uses_user_identity_not_pid() {
    let owner = UserIdentity::Sid("S-1-5-21-1000".into());
    let handler = RequestHandler::new();
    *handler
        .server_access
        .lock()
        .expect("server access mutex must not be poisoned") =
        ServerAccessStore::new_for_identity(0, owner.clone());

    let owner_peer = PeerIdentity {
        pid: 4242,
        uid: 0,
        user: owner,
    };
    let untrusted_peer_with_reused_pid = PeerIdentity {
        pid: owner_peer.pid,
        uid: 0,
        user: UserIdentity::Sid("S-1-5-21-2000".into()),
    };

    assert_eq!(
        handler.access_mode_for_peer(&owner_peer),
        Some(AccessMode::ReadWrite)
    );
    assert_eq!(
        handler.access_mode_for_peer(&untrusted_peer_with_reused_pid),
        None
    );
}
