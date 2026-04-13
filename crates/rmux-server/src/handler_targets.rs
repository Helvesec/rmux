use rmux_core::{command_target_metadata, TargetFindContext, UnresolvedTarget};
use rmux_proto::request::{Request, ResolveTargetType};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, ResolveTargetResponse, Response, RmuxError, ScopeSelector, Target, WindowTarget,
};

use super::RequestHandler;

pub(in crate::handler) fn target_to_scope(target: &Target) -> ScopeSelector {
    match target {
        Target::Session(session_name) => ScopeSelector::Session(session_name.clone()),
        Target::Window(target) => ScopeSelector::Window(target.clone()),
        Target::Pane(target) => ScopeSelector::Pane(target.clone()),
    }
}

pub(in crate::handler) fn active_session_target(
    sessions: &rmux_core::SessionStore,
    session_name: &rmux_proto::SessionName,
) -> Option<Target> {
    let session = sessions.session(session_name)?;
    let window_index = session.active_window_index();
    let window = session.window_at(window_index)?;
    let pane = window.active_pane()?;
    Some(Target::Pane(rmux_proto::PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane.index(),
    )))
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_resolve_target(
        &self,
        request: rmux_proto::ResolveTargetRequest,
    ) -> Response {
        let preferred_session = if request
            .target
            .as_deref()
            .is_none_or(unresolved_target_needs_current_session)
        {
            self.preferred_session_name().await.ok()
        } else {
            None
        };
        let state = self.state.lock().await;
        let unresolved = match request.target {
            Some(target) => UnresolvedTarget::new(target),
            None => UnresolvedTarget::none(),
        };
        let find_type = match request.target_type {
            ResolveTargetType::Session => rmux_core::TargetFindType::Session,
            ResolveTargetType::Window => rmux_core::TargetFindType::Window,
            ResolveTargetType::Pane => rmux_core::TargetFindType::Pane,
        };
        let mut flags = rmux_core::TargetFindFlags::NONE;
        if request.window_index {
            flags = flags.union(rmux_core::TargetFindFlags::WINDOW_INDEX);
        }
        if request.prefer_unattached {
            flags = flags.union(rmux_core::TargetFindFlags::PREFER_UNATTACHED);
        }
        let current_target = preferred_session
            .as_ref()
            .and_then(|session_name| active_session_target(&state.sessions, session_name));
        match state.sessions.resolve_unresolved_target(
            &unresolved,
            find_type,
            flags,
            &TargetFindContext::new(current_target),
        ) {
            Ok(target) => Response::ResolveTarget(ResolveTargetResponse { target }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }
}

fn unresolved_target_needs_current_session(raw: &str) -> bool {
    raw.is_empty()
        || raw == "."
        || raw.starts_with(':')
        || raw.starts_with(['+', '-'])
        || (raw.contains('.') && !raw.contains(':'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum SessionLookup {
    Found(rmux_proto::SessionName),
    Missing,
}

pub(in crate::handler) fn resolve_existing_session_target(
    sessions: &rmux_core::SessionStore,
    command_name: &str,
    target: &rmux_proto::SessionName,
) -> Result<rmux_proto::SessionName, RmuxError> {
    match resolve_session_lookup(sessions, command_name, target)? {
        SessionLookup::Found(session_name) => Ok(session_name),
        SessionLookup::Missing => Err(RmuxError::SessionNotFound(target.to_string())),
    }
}

pub(in crate::handler) fn resolve_session_lookup(
    sessions: &rmux_core::SessionStore,
    command_name: &str,
    target: &rmux_proto::SessionName,
) -> Result<SessionLookup, RmuxError> {
    let target_spec = command_target_metadata(command_name)
        .and_then(|metadata| metadata.target)
        .expect("session command must declare a target lookup spec");

    match sessions.resolve_unresolved_target(
        &UnresolvedTarget::new(target.to_string()),
        target_spec.find_type,
        target_spec.flags,
        &TargetFindContext::new(None),
    ) {
        Ok(resolved) => Ok(SessionLookup::Found(resolved.session_name().clone())),
        Err(error) if session_lookup_is_missing(&error) => Ok(SessionLookup::Missing),
        Err(error) => Err(error),
    }
}

fn session_lookup_is_missing(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::InvalidTarget { reason, .. } if reason.starts_with("can't find session: ")
    )
}

pub(in crate::handler) fn active_window_target(
    sessions: &rmux_core::SessionStore,
    target: &WindowTarget,
) -> Option<Target> {
    let session = sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    if let Some(pane) = window.active_pane() {
        return Some(Target::Pane(rmux_proto::PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            pane.index(),
        )));
    }
    Some(Target::Window(target.clone()))
}

pub(in crate::handler) fn target_for_scope_selector(
    state: &crate::pane_terminals::HandlerState,
    scope: &ScopeSelector,
) -> Option<Target> {
    match scope {
        ScopeSelector::Global => None,
        ScopeSelector::Session(session_name) => {
            active_session_target(&state.sessions, session_name)
        }
        ScopeSelector::Window(target) => active_window_target(&state.sessions, target),
        ScopeSelector::Pane(target) => Some(Target::Pane(target.clone())),
    }
}

pub(in crate::handler) fn target_for_option_scope(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Option<Target> {
    match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => None,
        OptionScopeSelector::Session(session_name) => {
            active_session_target(&state.sessions, session_name)
        }
        OptionScopeSelector::Window(target) => active_window_target(&state.sessions, target),
        OptionScopeSelector::Pane(target) => Some(Target::Pane(target.clone())),
    }
}

pub(in crate::handler) fn fallback_current_target(
    state: &crate::pane_terminals::HandlerState,
    attached_session: Option<&rmux_proto::SessionName>,
) -> Option<Target> {
    attached_session
        .and_then(|session_name| active_session_target(&state.sessions, session_name))
        .or_else(|| {
            state
                .sessions
                .iter()
                .map(|(session_name, _)| session_name)
                .min_by(|left, right| left.as_str().cmp(right.as_str()))
                .and_then(|session_name| active_session_target(&state.sessions, session_name))
        })
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
        Response::SplitWindow(success) => Some(Target::Pane(success.pane.clone())),
        Response::LastPane(success) => Some(Target::Pane(success.target.clone())),
        Response::SelectPane(success) => Some(Target::Pane(success.target.clone())),
        Response::MovePane(success) => Some(Target::Pane(success.target.clone())),
        Response::BreakPane(success) => Some(Target::Pane(success.target.clone())),
        Response::PipePane(success) => Some(Target::Pane(success.target.clone())),
        Response::RespawnPane(success) => Some(Target::Pane(success.target.clone())),
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
            Request::SendPrefix(request) => request
                .target
                .as_ref()
                .map(|target| Target::Pane(target.clone()))
                .or_else(|| fallback_current_target(state, attached_session)),
            Request::KillPane(request) => Some(Target::Pane(request.target.clone())),
            Request::ResizePane(request) => Some(Target::Pane(request.target.clone())),
            Request::CapturePane(request) => Some(Target::Pane(request.target.clone())),
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
