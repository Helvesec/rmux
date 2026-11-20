//! The current target a completed request leaves behind, derived from its response or request.

use rmux_core::ManagedSessionStore;
use rmux_proto::request::Request;
use rmux_proto::{PaneTargetRef, Response, Target};

use super::{
    active_session_target, active_window_target, fallback_current_target,
    pane_id_target_in_session, target_for_option_scope, target_for_scope_selector,
};

fn pane_kill_request_target(
    sessions: &ManagedSessionStore,
    target: &PaneTargetRef,
) -> Option<Target> {
    match target {
        PaneTargetRef::Slot(target) => Some(Target::Pane(target.clone())),
        PaneTargetRef::Id {
            session_name,
            pane_id,
        } => pane_id_target_in_session(sessions, session_name, *pane_id)
            .or_else(|| active_session_target(sessions, session_name)),
    }
}

pub(in crate::handler) fn target_for_request_response(
    state: &crate::pane_terminals::HandlerState,
    request: &Request,
    response: &Response,
    attached_session: Option<&rmux_proto::SessionName>,
) -> Option<Target> {
    match response {
        Response::NewSession(success) => {
            active_session_target(&state.sessions, &success.session_name)
        }
        Response::NewWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::NextWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::PreviousWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::LastWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::SelectWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::RenameWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::LinkWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::RotateWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::UnlinkWindow(success) => active_window_target(&state.sessions, &success.target),
        Response::ExportWindowSnapshot(success) => {
            active_window_target(&state.sessions, &success.snapshot.target)
        }
        Response::RestoreWindowSnapshot(success) => {
            active_window_target(&state.sessions, &success.target)
        }
        Response::SplitWindow(success) => Some(Target::Pane(success.pane.clone())),
        Response::LastPane(success) => Some(Target::Pane(success.target.clone())),
        Response::SelectPane(success) => Some(Target::Pane(success.target.clone())),
        Response::MovePane(success) => Some(Target::Pane(success.target.clone())),
        Response::BreakPane(success) => Some(Target::Pane(success.target.clone())),
        Response::PipePane(success) => Some(Target::Pane(success.target.clone())),
        Response::RespawnPane(success) => Some(Target::Pane(success.target.clone())),
        Response::KillPane(_) => None,
        Response::RenameSession(success) => {
            active_session_target(&state.sessions, &success.session_name)
        }
        _ => match request {
            Request::NewSession(request) => {
                active_session_target(&state.sessions, &request.session_name)
            }
            Request::AttachSession(request) => {
                active_session_target(&state.sessions, &request.target)
            }
            Request::HasSession(request) => active_session_target(&state.sessions, &request.target),
            Request::KillSession(request) => {
                active_session_target(&state.sessions, &request.target)
            }
            Request::RenameSession(request) => {
                active_session_target(&state.sessions, &request.new_name)
            }
            Request::NewWindow(request) => active_session_target(&state.sessions, &request.target),
            Request::KillWindow(request) => active_window_target(&state.sessions, &request.target),
            Request::LinkWindow(request) => active_window_target(&state.sessions, &request.target),
            Request::ListWindows(request) => {
                active_session_target(&state.sessions, &request.target)
            }
            Request::ExportWindowSnapshot(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::ExportWindowSnapshotV2(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::RestoreWindowSnapshot(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::RestoreWindowSnapshotV2(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::RotateWindow(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::ResizeWindow(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::RespawnWindow(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            Request::MovePane(request) => Some(Target::Pane(request.target.clone())),
            Request::PipePane(request) => Some(Target::Pane(request.target.clone())),
            Request::RespawnPane(request) => Some(Target::Pane(request.target.clone())),
            Request::SendKeys(request) => Some(Target::Pane(request.target.clone())),
            Request::CopyMode(request) => request
                .target
                .as_ref()
                .map(|target| Target::Pane(target.clone()))
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SendKeysExt(request) => request
                .target
                .as_ref()
                .map(|target| Target::Pane(target.clone()))
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SendKeysExt2(request) => request
                .target
                .as_ref()
                .map(|target| Target::Pane(target.clone()))
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SendPrefix(request) => request
                .target
                .as_ref()
                .map(|target| Target::Pane(target.clone()))
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::KillPane(request) => Some(Target::Pane(request.target.clone())),
            Request::PaneKill(request) => {
                pane_kill_request_target(&state.sessions, &request.target)
            }
            Request::ResizePaneAxes(request) => Some(Target::Pane(request.target.clone())),
            Request::ResizePane(request) => Some(Target::Pane(request.target.clone())),
            Request::CapturePane(request) => Some(Target::Pane(request.target.clone())),
            Request::PaneSnapshot(request) => Some(Target::Pane(request.target.clone())),
            Request::PasteBuffer(request) => Some(Target::Pane(request.target.clone())),
            Request::ClearHistory(request) => Some(Target::Pane(request.target.clone())),
            Request::DisplayPanes(request) => {
                active_session_target(&state.sessions, &request.target)
            }
            Request::ListPanes(request) => active_session_target(&state.sessions, &request.target),
            Request::SwitchClientExt(request) => request
                .target
                .as_ref()
                .and_then(|session_name| active_session_target(&state.sessions, session_name))
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::DisplayMessage(request) => request
                .target
                .as_ref()
                .and_then(|target| match target {
                    Target::Session(session_name) => {
                        active_session_target(&state.sessions, session_name)
                    }
                    Target::Window(target) => active_window_target(&state.sessions, target),
                    Target::Pane(target) => Some(Target::Pane(target.clone())),
                })
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::DisplayMessageExt(request) => request
                .target
                .as_ref()
                .and_then(|target| match target {
                    Target::Session(session_name) => {
                        active_session_target(&state.sessions, session_name)
                    }
                    Target::Window(target) => active_window_target(&state.sessions, target),
                    Target::Pane(target) => Some(Target::Pane(target.clone())),
                })
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SetOption(request) => target_for_scope_selector(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SetEnvironment(request) => target_for_scope_selector(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SetHook(request) => target_for_scope_selector(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SetHookMutation(request) => target_for_scope_selector(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::ShowOptions(request) => target_for_option_scope(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::ShowEnvironment(request) => target_for_scope_selector(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::ShowHooks(request) => target_for_scope_selector(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::SetOptionByName(request) => target_for_option_scope(state, &request.scope)
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::UnlinkWindow(request) => {
                active_window_target(&state.sessions, &request.target)
            }
            _ => fallback_current_target(state, attached_session),
        },
    }
}
