use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rmux_core::{
    command_target_metadata, OptionStore, SessionStore, TargetFindContext, UnresolvedTarget,
};
use rmux_proto::request::{Request, ResolveTargetType};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, OptionName, ResolveTargetResponse, Response, RmuxError, ScopeSelector, Target,
    WindowTarget,
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
        requester_pid: u32,
        request: rmux_proto::ResolveTargetRequest,
    ) -> Response {
        match self
            .resolve_target_for_requester(requester_pid, request)
            .await
        {
            Ok(target) => Response::ResolveTarget(ResolveTargetResponse { target }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(in crate::handler) async fn resolve_target_for_requester(
        &self,
        requester_pid: u32,
        request: rmux_proto::ResolveTargetRequest,
    ) -> Result<Target, RmuxError> {
        let needs_current_target = request
            .target
            .as_deref()
            .map(|raw| unresolved_target_needs_current_session(raw, request.target_type))
            .unwrap_or(true);
        let attached_session = if needs_current_target {
            self.current_session_candidate(requester_pid).await
        } else {
            None
        };
        let preferred_session = if needs_current_target {
            self.preferred_session_name().await.ok()
        } else {
            None
        };
        let socket_path = self.socket_path();
        let requester_pane_id = needs_current_target
            .then(|| requester_environment_pane_id(requester_pid, &socket_path))
            .flatten();
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
        let current_target = requester_pane_id
            .and_then(|pane_id| pane_id_target(&state.sessions, pane_id))
            .or_else(|| {
                attached_session
                    .as_ref()
                    .and_then(|session_name| active_session_target(&state.sessions, session_name))
            })
            .or_else(|| {
                preferred_session
                    .as_ref()
                    .and_then(|session_name| active_session_target(&state.sessions, session_name))
            });
        let marked_target = state.marked_pane_target().map(Target::Pane);
        let context = with_visible_pane_bases(
            TargetFindContext::new(current_target).with_marked_target(marked_target),
            &state.sessions,
            &state.options,
        );
        state
            .sessions
            .resolve_unresolved_target(&unresolved, find_type, flags, &context)
    }
}

pub(in crate::handler) fn requester_environment_pane_id(
    requester_pid: u32,
    server_socket_path: &Path,
) -> Option<u32> {
    requester_environment_context(requester_pid, server_socket_path).pane_id
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::handler) struct RequesterEnvironmentContext {
    pub pane_id: Option<u32>,
    pub source_depth: Option<usize>,
}

pub(in crate::handler) fn requester_environment_context(
    requester_pid: u32,
    server_socket_path: &Path,
) -> RequesterEnvironmentContext {
    if requester_pid == std::process::id() {
        return RequesterEnvironmentContext::default();
    }

    let Some(environment) = rmux_os::process::environment(requester_pid) else {
        return RequesterEnvironmentContext::default();
    };
    requester_environment_context_from_map(&environment, server_socket_path)
}

fn requester_environment_context_from_map(
    environment: &HashMap<String, String>,
    server_socket_path: &Path,
) -> RequesterEnvironmentContext {
    if !environment_rmux_socket_matches(environment, server_socket_path) {
        return RequesterEnvironmentContext::default();
    }
    let pane_id = environment
        .get("RMUX_PANE")
        .or_else(|| environment.get("TMUX_PANE"))
        .and_then(|pane| pane.strip_prefix('%'))
        .and_then(|pane| pane.parse::<u32>().ok());
    let source_depth = environment
        .get("RMUX_SOURCE_DEPTH")
        .and_then(|depth| depth.parse::<usize>().ok());
    RequesterEnvironmentContext {
        pane_id,
        source_depth,
    }
}

pub(in crate::handler) fn pane_id_target(sessions: &SessionStore, pane_id: u32) -> Option<Target> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::new(format!("%{pane_id}")),
            rmux_core::TargetFindType::Pane,
            rmux_core::TargetFindFlags::CANFAIL,
            &TargetFindContext::new(None),
        )
        .ok()
}

fn environment_rmux_socket_matches(
    environment: &HashMap<String, String>,
    server_socket_path: &Path,
) -> bool {
    let Some(value) = environment.get("RMUX") else {
        return false;
    };
    let Some(inherited_socket) = rmux_socket_path_from_env(value) else {
        return false;
    };
    rmux_os::path::socket_paths_match(&inherited_socket, server_socket_path)
}

fn rmux_socket_path_from_env(value: &str) -> Option<PathBuf> {
    let path = value.split_once(',').map_or(value, |(path, _)| path);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(in crate::handler) fn with_visible_pane_bases(
    context: TargetFindContext,
    sessions: &SessionStore,
    options: &OptionStore,
) -> TargetFindContext {
    let mut pane_base_indices = HashMap::new();
    for (session_name, session) in sessions.iter() {
        for window_index in session.windows().keys().copied() {
            let pane_base_index = options
                .resolve_for_window(session_name, window_index, OptionName::PaneBaseIndex)
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            if pane_base_index > 0 {
                pane_base_indices.insert((session_name.clone(), window_index), pane_base_index);
            }
        }
    }
    context.with_pane_base_indices(pane_base_indices)
}

fn unresolved_target_needs_current_session(raw: &str, target_type: ResolveTargetType) -> bool {
    if target_type == ResolveTargetType::Pane {
        return true;
    }

    raw.is_empty()
        || raw == "."
        || raw.starts_with('@')
        || raw.starts_with(':')
        || raw.starts_with(['+', '-'])
        || (target_type == ResolveTargetType::Window
            && raw.bytes().all(|byte| byte.is_ascii_digit()))
        || (raw.contains('.') && !raw.contains(':'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::AttachControl;
    use rmux_proto::{
        NewSessionRequest, NewWindowRequest, PaneTarget, ResolveTargetRequest, Response,
        SelectPaneRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget, Target,
        TerminalSize,
    };

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    async fn resolve_pane(handler: &RequestHandler, target: &str) -> Target {
        let response = handler
            .handle(Request::ResolveTarget(ResolveTargetRequest {
                target: Some(target.to_owned()),
                target_type: ResolveTargetType::Pane,
                window_index: false,
                prefer_unattached: false,
            }))
            .await;
        let Response::ResolveTarget(response) = response else {
            panic!("pane target {target:?} should resolve, got {response:?}");
        };
        response.target
    }

    #[tokio::test]
    async fn resolve_target_uses_current_window_for_relative_pane_forms() {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: alpha.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                    direction: SplitDirection::Vertical,
                    before: false,
                    environment: None,
                }))
                .await,
            Response::SplitWindow(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                    target: PaneTarget::with_window(alpha.clone(), 0, 1),
                    title: None,
                    style: None,
                    input_disabled: None,
                    preserve_zoom: false,
                })))
                .await,
            Response::SelectPane(_)
        ));
        {
            let state = handler.state.lock().await;
            let session = state.sessions.session(&alpha).expect("alpha exists");
            let window = session.window_at(0).expect("window 0 exists");
            assert_eq!(window.active_pane_index(), 1);
            let top = window.pane(0).expect("pane 0 exists").geometry();
            let bottom = window.pane(1).expect("pane 1 exists").geometry();
            assert!(top.y() < bottom.y(), "pane 0 should sit above pane 1");
        }

        assert_eq!(
            resolve_pane(&handler, "1").await,
            Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 1))
        );
        assert_eq!(
            resolve_pane(&handler, "{up-of}").await,
            Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0))
        );
        assert_eq!(
            resolve_pane(&handler, "{down-of}").await,
            Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0))
        );
        assert!(matches!(
            handler
                .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                    target: PaneTarget::with_window(alpha.clone(), 0, 0),
                    title: None,
                    style: None,
                    input_disabled: None,
                    preserve_zoom: false,
                })))
                .await,
            Response::SelectPane(_)
        ));
        assert_eq!(
            resolve_pane(&handler, "{down-of}").await,
            Target::Pane(PaneTarget::with_window(alpha, 0, 1))
        );
    }

    #[tokio::test]
    async fn bare_numeric_window_targets_use_current_session_before_global_matches() {
        let handler = RequestHandler::new();
        let bg = session_name("bg");
        let work = session_name("work");
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: bg.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::NewWindow(Box::new(NewWindowRequest {
                    target: bg,
                    name: Some("bg1".to_owned()),
                    detached: true,
                    start_directory: None,
                    environment: None,
                    command: None,
                    process_command: None,
                    target_window_index: Some(1),
                    insert_at_target: false,
                })))
                .await,
            Response::NewWindow(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: work.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
        let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel::<AttachControl>();
        handler
            .register_attach(std::process::id(), work, control_tx)
            .await;

        let response = handler
            .handle(Request::ResolveTarget(ResolveTargetRequest {
                target: Some("1".to_owned()),
                target_type: ResolveTargetType::Window,
                window_index: false,
                prefer_unattached: false,
            }))
            .await;

        match response {
            Response::Error(error) => assert!(
                error.error.to_string().contains("can't find window: 1"),
                "unexpected error: {}",
                error.error
            ),
            other => panic!("bare numeric target must not resolve globally: {other:?}"),
        }
    }

    #[test]
    fn requester_environment_context_requires_matching_socket() {
        let socket_path = std::env::temp_dir().join(format!(
            "rmux-requester-context-{}.sock",
            std::process::id()
        ));
        let mut environment = HashMap::new();
        environment.insert(
            "RMUX".to_owned(),
            format!("{},123,0", socket_path.display()),
        );
        environment.insert("RMUX_PANE".to_owned(), "%42".to_owned());
        environment.insert("RMUX_SOURCE_DEPTH".to_owned(), "3".to_owned());

        assert_eq!(
            requester_environment_context_from_map(&environment, &socket_path),
            RequesterEnvironmentContext {
                pane_id: Some(42),
                source_depth: Some(3),
            }
        );

        let other_socket = socket_path.with_file_name("rmux-requester-context-other.sock");
        assert_eq!(
            requester_environment_context_from_map(&environment, &other_socket),
            RequesterEnvironmentContext::default()
        );
    }
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
