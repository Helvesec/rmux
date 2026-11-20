use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rmux_core::{
    command_target_metadata, ManagedSessionStore, OptionStore, TargetFindContext, UnresolvedTarget,
};
use rmux_proto::request::ResolveTargetType;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, OptionName, PaneTarget, ResolveTargetResponse, Response, RmuxError,
    ScopeSelector, SessionId, Target, WindowTarget,
};

use super::RequestHandler;
use crate::pane_terminals::HandlerState;

#[cfg(test)]
#[path = "../handler_targets/inherited_tests.rs"]
mod inherited_tests;

mod request_response;
pub(in crate::handler) use request_response::target_for_request_response;

pub(in crate::handler) fn target_to_scope(target: &Target) -> ScopeSelector {
    match target {
        Target::Session(session_name) => ScopeSelector::Session(session_name.clone()),
        Target::Window(target) => ScopeSelector::Window(target.clone()),
        Target::Pane(target) => ScopeSelector::Pane(target.clone()),
    }
}

pub(in crate::handler) fn active_session_target(
    sessions: &rmux_core::ManagedSessionStore,
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
        self.resolve_target_for_requester_with(requester_pid, request, |_, target| Ok(target))
            .await
    }

    pub(in crate::handler) async fn resolve_target_for_requester_with<T>(
        &self,
        requester_pid: u32,
        request: rmux_proto::ResolveTargetRequest,
        resolved: impl FnOnce(&mut HandlerState, Target) -> Result<T, RmuxError>,
    ) -> Result<T, RmuxError> {
        let needs_current_session = request
            .target
            .as_deref()
            .map(|raw| unresolved_target_needs_current_session(raw, request.target_type))
            .unwrap_or(true);
        let needs_current_occurrence = request
            .target
            .as_deref()
            .is_some_and(|raw| unresolved_target_uses_current_occurrence(raw, request.target_type));
        let needs_current_target = needs_current_session || needs_current_occurrence;
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
        let requester_environment = if needs_current_target {
            requester_environment_context(requester_pid, &socket_path)
        } else {
            Default::default()
        };
        self.resolve_target_for_requester_with_environment(
            request,
            requester_environment,
            attached_session,
            preferred_session,
            resolved,
        )
        .await
    }

    pub(in crate::handler) async fn resolve_target_for_requester_with_environment<T>(
        &self,
        request: rmux_proto::ResolveTargetRequest,
        requester_environment: RequesterEnvironmentContext,
        attached_session: Option<rmux_proto::SessionName>,
        preferred_session: Option<rmux_proto::SessionName>,
        resolved: impl FnOnce(&mut HandlerState, Target) -> Result<T, RmuxError>,
    ) -> Result<T, RmuxError> {
        let mut state = self.state.lock().await;
        let unresolved = match request.target.as_ref() {
            Some(target) => UnresolvedTarget::new(target.clone()),
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
        let current_target = inherited_pane_target(
            &state.sessions,
            requester_environment,
            request.target.as_deref(),
        )?
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
        let target =
            state
                .sessions
                .resolve_unresolved_target(&unresolved, find_type, flags, &context)?;
        resolved(&mut state, target)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::handler) struct RequesterEnvironmentContext {
    pub pane_id: Option<u32>,
    pub session_id: Option<SessionId>,
    pub source_depth: Option<usize>,
}

tokio::task_local! {
    static REQUESTER_ENVIRONMENT_MEMO:
        std::cell::RefCell<Option<(u32, PathBuf, RequesterEnvironmentContext)>>;
}

/// Runs one request with a memo for [`requester_environment_context`]: the
/// requester stays connected for the whole request, so its exec-time
/// environment cannot change, and the request reads `/proc` once instead of
/// once per admission, validation and target-resolution step.
pub(in crate::handler) async fn with_requester_environment_memo<F: std::future::Future>(
    request: F,
) -> F::Output {
    REQUESTER_ENVIRONMENT_MEMO
        .scope(std::cell::RefCell::new(None), request)
        .await
}

pub(in crate::handler) fn requester_environment_context(
    requester_pid: u32,
    server_socket_path: &Path,
) -> RequesterEnvironmentContext {
    if requester_pid == std::process::id() {
        return RequesterEnvironmentContext::default();
    }

    let memoized = REQUESTER_ENVIRONMENT_MEMO.try_with(|memo| {
        memo.borrow()
            .as_ref()
            .filter(|(pid, path, _)| *pid == requester_pid && path == server_socket_path)
            .map(|(_, _, context)| *context)
    });
    if let Ok(Some(context)) = memoized {
        return context;
    }
    let context = rmux_os::process::environment(requester_pid)
        .map(|environment| requester_environment_context_from_map(&environment, server_socket_path))
        .unwrap_or_default();
    let _ = REQUESTER_ENVIRONMENT_MEMO.try_with(|memo| {
        *memo.borrow_mut() = Some((requester_pid, server_socket_path.to_path_buf(), context));
    });
    context
}

fn requester_environment_context_from_map(
    environment: &HashMap<String, String>,
    server_socket_path: &Path,
) -> RequesterEnvironmentContext {
    if !environment.contains_key("RMUX") {
        return tmux_only_environment_context(environment, server_socket_path);
    }
    if !environment_rmux_socket_matches(environment, server_socket_path) {
        return RequesterEnvironmentContext::default();
    }
    let pane_id = environment
        .get("RMUX_PANE")
        .or_else(|| environment.get("TMUX_PANE"))
        .and_then(|pane| pane_number(pane));
    let session_id = environment
        .get("RMUX")
        .and_then(|value| value.rsplit_once(','))
        .and_then(|(_, value)| value.parse::<u32>().ok())
        .map(SessionId::new);
    let source_depth = environment
        .get("RMUX_SOURCE_DEPTH")
        .and_then(|depth| depth.parse::<usize>().ok());
    RequesterEnvironmentContext {
        pane_id,
        session_id,
        source_depth,
    }
}

pub(in crate::handler) fn inherited_pane_target(
    sessions: &ManagedSessionStore,
    context: RequesterEnvironmentContext,
    requested_target: Option<&str>,
) -> Result<Option<Target>, RmuxError> {
    let Some(pane_id) = context.pane_id else {
        return Ok(None);
    };
    let Some(session_id) = context.session_id else {
        return Ok(pane_id_target(sessions, pane_id));
    };
    if let Some(target) = pane_id_target_in_session_identity(sessions, session_id, pane_id) {
        return Ok(Some(target));
    }
    // A vanished pane is an obsolete environment hint, not a target operand.
    // Keep rejecting a lost session or a live pane in the wrong session.
    // The caller must still resolve any explicitly requested target normally.
    if sessions.session_by_id(session_id).is_some()
        && !sessions.iter().any(|session| {
            session
                .window_index_for_pane_id(rmux_core::PaneId::new(pane_id))
                .is_some()
        })
    {
        return Ok(None);
    }
    Err(RmuxError::invalid_target(
        requested_target.unwrap_or_default(),
        format!("inherited pane %{pane_id} is not present in inherited session {session_id}"),
    ))
}

pub(in crate::handler) fn pane_id_target(
    sessions: &ManagedSessionStore,
    pane_id: u32,
) -> Option<Target> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::new(format!("%{pane_id}")),
            rmux_core::TargetFindType::Pane,
            rmux_core::TargetFindFlags::CANFAIL,
            &TargetFindContext::new(None),
        )
        .ok()
}

fn pane_id_target_in_session_identity(
    sessions: &ManagedSessionStore,
    session_id: SessionId,
    pane_id: u32,
) -> Option<Target> {
    let session_name = sessions.session_by_id(session_id)?.name().clone();
    pane_id_target_in_session(sessions, &session_name, rmux_proto::PaneId::new(pane_id))
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

/// Without `RMUX`, a requester outside any pane names its pane through
/// `TMUX_PANE` alone, as tmux 3.7b reads it in `cmd_find_inside_pane`. tmux
/// ignores the socket in `TMUX`; rmux refuses a `TMUX` that names another
/// server, because there the pane id belongs to that server (rmux run inside
/// a tmux pane). `RMUX_PANE` and `RMUX_SOURCE_DEPTH` stay bound to `RMUX`.
fn tmux_only_environment_context(
    environment: &HashMap<String, String>,
    server_socket_path: &Path,
) -> RequesterEnvironmentContext {
    let names_other_server = environment
        .get("TMUX")
        .and_then(|value| rmux_socket_path_from_env(value))
        .is_some_and(|socket| !rmux_os::path::socket_paths_match(&socket, server_socket_path));
    if names_other_server {
        return RequesterEnvironmentContext::default();
    }
    RequesterEnvironmentContext {
        pane_id: environment
            .get("TMUX_PANE")
            .and_then(|pane| pane_number(pane)),
        session_id: None,
        source_depth: None,
    }
}

fn pane_number(pane: &str) -> Option<u32> {
    pane.strip_prefix('%')?.parse::<u32>().ok()
}

fn rmux_socket_path_from_env(value: &str) -> Option<PathBuf> {
    let path = value.split_once(',').map_or(value, |(path, _)| path);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(in crate::handler) fn with_visible_pane_bases(
    context: TargetFindContext,
    sessions: &ManagedSessionStore,
    options: &OptionStore,
) -> TargetFindContext {
    let mut pane_base_indices = HashMap::new();
    for (session_name, session) in sessions.iter().map(|session| (session.name(), session)) {
        for window_index in session.windows().map(|(index, _)| index) {
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
        if matches!(raw, "~" | "{marked}") {
            return false;
        }
        let session_qualified = raw
            .split_once(':')
            .is_some_and(|(session, _)| !session.is_empty());
        return !is_global_pane_id(raw) && !session_qualified;
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

fn unresolved_target_uses_current_occurrence(raw: &str, target_type: ResolveTargetType) -> bool {
    target_type == ResolveTargetType::Pane && is_global_pane_id(raw)
}

fn is_global_pane_id(raw: &str) -> bool {
    raw.strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::AttachControl;
    use rmux_proto::request::Request;
    use rmux_proto::{
        HookLifecycle, HookName, KillPaneRequest, LinkWindowRequest, MoveWindowRequest,
        MoveWindowTarget, NewSessionExtRequest, NewSessionRequest, NewWindowRequest,
        PaneKillRequest, PaneTargetRef, ResolveTargetRequest, Response, ScopeSelector,
        SelectPaneRequest, SetHookRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
        Target, TerminalSize,
    };

    fn session_name(value: &str) -> rmux_proto::SessionName {
        rmux_proto::SessionName::new(value).expect("valid session name")
    }

    #[test]
    fn explicit_pane_targets_do_not_require_current_session_context() {
        for target in ["%42", "work:1.2", "work:1", "~", "{marked}"] {
            assert!(!unresolved_target_needs_current_session(
                target,
                ResolveTargetType::Pane
            ));
        }
        for target in ["", ".", "1", ":1.2", "{up-of}"] {
            assert!(unresolved_target_needs_current_session(
                target,
                ResolveTargetType::Pane
            ));
        }
        assert!(unresolved_target_uses_current_occurrence(
            "%42",
            ResolveTargetType::Pane
        ));
        for target in ["work:1.2", ":1.2", "{up-of}"] {
            assert!(!unresolved_target_uses_current_occurrence(
                target,
                ResolveTargetType::Pane
            ));
        }
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: alpha.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: bg.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
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
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: work.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
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
                session_id: Some(SessionId::new(0)),
                source_depth: Some(3),
            }
        );

        let other_socket = socket_path.with_file_name("rmux-requester-context-other.sock");
        assert_eq!(
            requester_environment_context_from_map(&environment, &other_socket),
            RequesterEnvironmentContext::default()
        );
    }

    #[tokio::test]
    async fn pane_kill_after_target_stays_in_the_addressed_detached_session() {
        let handler = RequestHandler::new();
        let alpha = session_name("after-pane-kill-alpha");
        let zeta = session_name("after-pane-kill-zeta");
        for session in [&alpha, &zeta] {
            let response = handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&response);
        }
        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(zeta.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
        let removed_pane_id = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&zeta)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(1))
                .expect("second zeta pane exists")
                .id()
        };
        let hook = handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(zeta.clone()),
                hook: HookName::AfterKillPane,
                command: "set-buffer -a -b pane-kill-after local,".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await;
        assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");
        let request = Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(zeta.clone(), removed_pane_id),
            kill_all_except: false,
        });
        let response = handler.handle(request.clone()).await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");

        let state = handler.state.lock().await;
        assert_eq!(
            state
                .buffers
                .show(Some("pane-kill-after"))
                .expect("addressed session-local after-kill-pane hook ran")
                .1,
            b"local,"
        );
    }

    #[tokio::test]
    async fn pane_kill_after_target_follows_a_surviving_real_winlink() {
        let handler = RequestHandler::new();
        let decoy = session_name("after-pane-kill-decoy");
        let source = session_name("after-pane-kill-source");
        let survivor = session_name("after-pane-kill-survivor");
        for session in [&decoy, &source, &survivor] {
            let response = handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&response);
        }
        let pane_id = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&source)
                .and_then(|session| session.window_at(0))
                .and_then(|window| window.pane(0))
                .expect("source pane exists")
                .id()
        };
        let linked = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(source.clone(), 0),
                target: WindowTarget::with_window(survivor.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await;
        assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
        handler.wait_for_initial_panes_for_test().await;

        let hook = handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(survivor.clone()),
                hook: HookName::AfterKillPane,
                command: "set-buffer -b pane-kill-survivor-target survivor".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await;
        assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");

        let request = Request::PaneKill(PaneKillRequest {
            target: PaneTargetRef::by_id(source.clone(), pane_id),
            kill_all_except: false,
        });
        let response = handler.handle(request.clone()).await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");

        let state = handler.state.lock().await;
        assert!(state.sessions.session(&decoy).is_some());
        assert!(state.sessions.session(&source).is_none());
        assert!(state.sessions.session(&survivor).is_some());
        assert_eq!(
            state
                .buffers
                .show(Some("pane-kill-survivor-target"))
                .expect("surviving session-local after-kill-pane hook ran")
                .1,
            b"survivor"
        );
    }

    #[tokio::test]
    async fn kill_pane_after_target_falls_back_to_a_surviving_affected_session() {
        let handler = RequestHandler::new();
        let source = session_name("after-kill-fallback-source");
        let survivor = session_name("after-kill-fallback-survivor");
        for session in [&source, &survivor] {
            crate::session_fixture::assert_new_session(
                &handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
            );
        }
        assert!(matches!(
            handler
                .handle(Request::LinkWindow(LinkWindowRequest {
                    source: WindowTarget::with_window(source.clone(), 0),
                    target: WindowTarget::with_window(survivor.clone(), 1),
                    after: false,
                    before: false,
                    kill_destination: false,
                    detached: true,
                }))
                .await,
            Response::LinkWindow(_)
        ));
        handler.wait_for_initial_panes_for_test().await;
        assert!(matches!(
            handler
                .handle(Request::SetHook(SetHookRequest {
                    scope: ScopeSelector::Session(survivor.clone()),
                    hook: HookName::AfterKillPane,
                    command: "set-buffer -b kill-pane-affected-fallback survivor".to_owned(),
                    lifecycle: HookLifecycle::Persistent,
                }))
                .await,
            Response::SetHook(_)
        ));

        let response = handler
            .handle(Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(source.clone(), 0, 0),
                kill_all_except: false,
            }))
            .await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");

        let state = handler.state.lock().await;
        assert!(state.sessions.session(&source).is_none());
        assert!(state.sessions.session(&survivor).is_some());
        assert_eq!(
            state
                .buffers
                .show(Some("kill-pane-affected-fallback"))
                .expect("surviving affected session hook runs")
                .1,
            b"survivor"
        );
    }

    #[tokio::test]
    async fn kill_pane_after_target_uses_the_surviving_pane_in_the_same_window() {
        let handler = RequestHandler::new();
        let decoy = session_name("after-kill-pane-decoy");
        let target_session = session_name("after-kill-pane-target");
        for session in [&decoy, &target_session] {
            let response = handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await;
            crate::session_fixture::assert_new_session(&response);
        }
        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(target_session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
        let surviving_target = PaneTarget::with_window(target_session.clone(), 0, 0);
        let hook = handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Pane(surviving_target.clone()),
                hook: HookName::AfterKillPane,
                command: "set-buffer -b kill-pane-survivor-target survivor".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await;
        assert!(matches!(hook, Response::SetHook(_)), "{hook:?}");

        let request = Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(target_session, 0, 1),
            kill_all_except: false,
        });
        let response = handler.handle(request.clone()).await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");

        let state = handler.state.lock().await;
        assert!(state.sessions.session(&decoy).is_some());
        assert_eq!(
            state
                .buffers
                .show(Some("kill-pane-survivor-target"))
                .expect("surviving pane-local after-kill-pane hook ran")
                .1,
            b"survivor"
        );
    }

    #[tokio::test]
    async fn kill_pane_after_target_ignores_a_reused_low_pane_slot() {
        let handler = RequestHandler::new();
        let session = session_name("after-kill-pane-reindex");
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        for _ in 0..2 {
            assert!(matches!(
                handler
                    .handle(Request::SplitWindow(SplitWindowRequest {
                        target: SplitWindowTarget::Session(session.clone()),
                        direction: SplitDirection::Vertical,
                        before: false,
                        environment: None,
                    }))
                    .await,
                Response::SplitWindow(_)
            ));
        }
        assert!(matches!(
            handler
                .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                    target: PaneTarget::with_window(session.clone(), 0, 2),
                    title: None,
                    style: None,
                    input_disabled: None,
                    preserve_zoom: false,
                })))
                .await,
            Response::SelectPane(_)
        ));
        let (active_pane_id, reused_slot_pane_id) = {
            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(0))
                .expect("three-pane window exists");
            (
                window.pane(2).expect("active pane exists").id(),
                window.pane(1).expect("middle pane exists").id(),
            )
        };
        let response = handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(session.clone()),
                hook: HookName::AfterKillPane,
                command: "set-option -p @after-kill-reindex active".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await;
        assert!(matches!(response, Response::SetHook(_)), "{response:?}");

        let (outcome, hooks) = handler
            .dispatch_captured(
                std::process::id(),
                u64::from(std::process::id()),
                Request::KillPane(KillPaneRequest {
                    target: PaneTarget::with_window(session.clone(), 0, 0),
                    kill_all_except: false,
                }),
            )
            .await;
        assert!(
            matches!(outcome.response, Response::KillPane(_)),
            "{:?}",
            outcome.response
        );
        assert_eq!(hooks.len(), 1, "one exact after-kill-pane hook is queued");
        assert_eq!(hooks[0].hook, HookName::AfterKillPane);
        assert!(hooks[0].exact_pane_target.is_some());
        let queued_slot = match hooks[0].current_target.as_ref() {
            Some(Target::Pane(target)) => target.clone(),
            other => panic!("expected queued pane target, got {other:?}"),
        };

        {
            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(0))
                .expect("window survives");
            assert_eq!(
                window.active_pane().expect("active pane survives").id(),
                active_pane_id
            );
            assert_eq!(
                window.pane(0).expect("low slot is reused").id(),
                reused_slot_pane_id
            );
        }

        let split = handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
                direction: SplitDirection::Vertical,
                before: true,
                environment: None,
            }))
            .await;
        assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
        let exact_target = {
            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(0))
                .expect("window survives delayed reindex");
            let pane = window
                .panes()
                .iter()
                .find(|pane| pane.id() == active_pane_id)
                .expect("exact pane survives delayed reindex");
            PaneTarget::with_window(session.clone(), 0, pane.index())
        };
        assert_ne!(
            exact_target, queued_slot,
            "delayed split must reuse the queued slot"
        );

        handler
            .run_inline_hooks(std::process::id(), hooks, None)
            .await;

        let state = handler.state.lock().await;
        assert_eq!(
            state
                .pane_explicit_option_value_by_name(&exact_target, "@after-kill-reindex")
                .expect("exact pane option resolves")
                .1,
            Some("active".to_owned())
        );
        assert_eq!(
            state
                .pane_explicit_option_value_by_name(&queued_slot, "@after-kill-reindex")
                .expect("queued-slot pane option resolves")
                .1,
            None
        );
    }

    #[tokio::test]
    async fn kill_all_except_after_target_keeps_the_stable_pane_for_all_entry_forms() {
        for (label, request_kind) in [("cli", 0_u8), ("sdk-slot", 1), ("sdk-id", 2)] {
            let handler = RequestHandler::new();
            let session = session_name(&format!("after-kill-others-{label}"));
            crate::session_fixture::assert_new_session(
                &handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
            );
            for _ in 0..2 {
                assert!(matches!(
                    handler
                        .handle(Request::SplitWindow(SplitWindowRequest {
                            target: SplitWindowTarget::Session(session.clone()),
                            direction: SplitDirection::Vertical,
                            before: false,
                            environment: None,
                        }))
                        .await,
                    Response::SplitWindow(_)
                ));
            }
            let target = PaneTarget::with_window(session.clone(), 0, 1);
            let pane_id = {
                let state = handler.state.lock().await;
                state
                    .sessions
                    .session(&session)
                    .and_then(|session| session.window_at(0))
                    .and_then(|window| window.pane(1))
                    .expect("kept pane exists")
                    .id()
            };
            let option = format!("@after-kill-others-{label}");
            assert!(matches!(
                handler
                    .handle(Request::SetHook(SetHookRequest {
                        scope: ScopeSelector::Session(session.clone()),
                        hook: HookName::AfterKillPane,
                        command: format!("set-option -p {option} hit"),
                        lifecycle: HookLifecycle::Persistent,
                    }))
                    .await,
                Response::SetHook(_)
            ));
            let request = match request_kind {
                0 => Request::KillPane(KillPaneRequest {
                    target,
                    kill_all_except: true,
                }),
                1 => Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::slot(target),
                    kill_all_except: true,
                }),
                _ => Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::by_id(session.clone(), pane_id),
                    kill_all_except: true,
                }),
            };
            let response = handler.handle(request).await;
            assert!(matches!(response, Response::KillPane(_)), "{response:?}");

            let state = handler.state.lock().await;
            let window = state
                .sessions
                .session(&session)
                .and_then(|session| session.window_at(0))
                .expect("window survives kill-all-except");
            assert_eq!(window.pane_count(), 1);
            assert_eq!(window.pane(0).expect("kept pane survives").id(), pane_id);
            assert_eq!(
                state
                    .pane_explicit_option_value_by_name(
                        &PaneTarget::with_window(session.clone(), 0, 0),
                        &option,
                    )
                    .expect("kept pane option resolves")
                    .1,
                Some("hit".to_owned())
            );
        }
    }

    #[tokio::test]
    async fn pane_kill_slot_after_target_chooses_one_stable_real_winlink() {
        let handler = RequestHandler::new();
        let source = session_name("after-pane-slot-source");
        let first = session_name("after-pane-slot-a");
        let second = session_name("after-pane-slot-b");
        for session in [&source, &first, &second] {
            crate::session_fixture::assert_new_session(
                &handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
            );
        }
        for survivor in [&first, &second] {
            let response = handler
                .handle(Request::LinkWindow(LinkWindowRequest {
                    source: WindowTarget::with_window(source.clone(), 0),
                    target: WindowTarget::with_window(survivor.clone(), 1),
                    after: false,
                    before: false,
                    kill_destination: false,
                    detached: true,
                }))
                .await;
            assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");
        }
        for (session, value) in [(&first, "first"), (&second, "second")] {
            assert!(matches!(
                handler
                    .handle(Request::SetHook(SetHookRequest {
                        scope: ScopeSelector::Session(session.clone()),
                        hook: HookName::AfterKillPane,
                        command: format!("set-option -p @after-pane-slot {value}"),
                        lifecycle: HookLifecycle::Persistent,
                    }))
                    .await,
                Response::SetHook(_)
            ));
        }

        let (outcome, hooks) = handler
            .dispatch_captured(
                std::process::id(),
                u64::from(std::process::id()),
                Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::slot(PaneTarget::with_window(source.clone(), 0, 0)),
                    kill_all_except: false,
                }),
            )
            .await;
        assert!(
            matches!(outcome.response, Response::KillPane(_)),
            "{:?}",
            outcome.response
        );
        assert_eq!(hooks.len(), 1);
        assert!(hooks[0].exact_pane_target.is_some());
        assert_eq!(
            hooks[0].current_target,
            Some(Target::Pane(PaneTarget::with_window(first.clone(), 1, 0)))
        );
        let moved = handler
            .handle(Request::MoveWindow(MoveWindowRequest {
                source: Some(WindowTarget::with_window(first.clone(), 1)),
                target: MoveWindowTarget::Window(WindowTarget::with_window(first.clone(), 5)),
                renumber: false,
                kill_destination: false,
                detached: true,
                after: false,
                before: false,
            }))
            .await;
        assert!(matches!(moved, Response::MoveWindow(_)), "{moved:?}");
        handler
            .run_inline_hooks(std::process::id(), hooks, None)
            .await;

        let state = handler.state.lock().await;
        assert!(state.sessions.session(&source).is_none());
        assert!(state.sessions.session(&first).is_some());
        assert!(state.sessions.session(&second).is_some());
        assert!(state
            .sessions
            .session(&first)
            .and_then(|session| session.window_at(1))
            .is_none());
        assert_eq!(
            state
                .pane_explicit_option_value_by_name(
                    &PaneTarget::with_window(first.clone(), 5, 0),
                    "@after-pane-slot",
                )
                .expect("moved exact pane option resolves")
                .1,
            Some("first".to_owned())
        );
        assert_eq!(
            state
                .pane_explicit_option_value_by_name(
                    &PaneTarget::with_window(second, 1, 0),
                    "@after-pane-slot",
                )
                .expect("second winlink pane option resolves")
                .1,
            Some("first".to_owned())
        );
    }

    #[tokio::test]
    async fn pane_kill_group_alias_after_target_follows_the_surviving_member() {
        let handler = RequestHandler::new();
        let owner = session_name("after-pane-group-owner");
        let peer = session_name("after-pane-group-peer");
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: owner.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
                    session_name: Some(peer.clone()),
                    working_directory: None,
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                    group_target: Some(owner.clone()),
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
                .await,
        );
        assert!(matches!(
            handler
                .handle(Request::SetHook(SetHookRequest {
                    scope: ScopeSelector::Session(owner.clone()),
                    hook: HookName::AfterKillPane,
                    command: "set-buffer -b pane-group-owner owner".to_owned(),
                    lifecycle: HookLifecycle::Persistent,
                }))
                .await,
            Response::SetHook(_)
        ));

        let response = handler
            .handle(Request::PaneKill(PaneKillRequest {
                target: PaneTargetRef::slot(PaneTarget::with_window(peer.clone(), 0, 0)),
                kill_all_except: false,
            }))
            .await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");

        let state = handler.state.lock().await;
        assert!(state.sessions.session(&peer).is_none());
        assert!(state.sessions.session(&owner).is_some());
        assert_eq!(
            state
                .buffers
                .show(Some("pane-group-owner"))
                .expect("surviving group member hook ran")
                .1,
            b"owner"
        );
    }

    #[tokio::test]
    async fn pane_kill_all_except_keeps_the_addressed_duplicate_winlink_index() {
        let handler = RequestHandler::new();
        let session = session_name("after-pane-duplicate-index");
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        assert!(matches!(
            handler
                .handle(Request::LinkWindow(LinkWindowRequest {
                    source: WindowTarget::with_window(session.clone(), 0),
                    target: WindowTarget::with_window(session.clone(), 2),
                    after: false,
                    before: false,
                    kill_destination: false,
                    detached: true,
                }))
                .await,
            Response::LinkWindow(_)
        ));
        assert!(matches!(
            handler
                .handle(Request::SetHook(SetHookRequest {
                    scope: ScopeSelector::Session(session.clone()),
                    hook: HookName::AfterKillPane,
                    command: "set-option -p -F @after-duplicate-index '#{window_index}'".to_owned(),
                    lifecycle: HookLifecycle::Persistent,
                }))
                .await,
            Response::SetHook(_)
        ));

        let response = handler
            .handle(Request::PaneKill(PaneKillRequest {
                target: PaneTargetRef::slot(PaneTarget::with_window(session.clone(), 2, 0)),
                kill_all_except: true,
            }))
            .await;
        assert!(matches!(response, Response::KillPane(_)), "{response:?}");

        let state = handler.state.lock().await;
        assert_eq!(
            state
                .pane_explicit_option_value_by_name(
                    &PaneTarget::with_window(session, 2, 0),
                    "@after-duplicate-index",
                )
                .expect("duplicate winlink pane option resolves")
                .1,
            Some("2".to_owned())
        );
    }

    #[tokio::test]
    async fn after_kill_pane_exact_target_survives_duplicate_winlink_slot_aba() {
        for (label, use_sdk) in [("cli", false), ("sdk", true)] {
            let handler = RequestHandler::new();
            let session = session_name(&format!("after-kill-occurrence-aba-{label}"));
            crate::session_fixture::assert_new_session(
                &handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
            );
            assert!(matches!(
                handler
                    .handle(Request::SplitWindow(SplitWindowRequest {
                        target: SplitWindowTarget::Session(session.clone()),
                        direction: SplitDirection::Vertical,
                        before: false,
                        environment: None,
                    }))
                    .await,
                Response::SplitWindow(_)
            ));
            assert!(matches!(
                handler
                    .handle(Request::LinkWindow(LinkWindowRequest {
                        source: WindowTarget::with_window(session.clone(), 0),
                        target: WindowTarget::with_window(session.clone(), 2),
                        after: false,
                        before: false,
                        kill_destination: false,
                        detached: true,
                    }))
                    .await,
                Response::LinkWindow(_)
            ));
            handler.wait_for_initial_panes_for_test().await;
            let option = format!("@after-kill-occurrence-aba-{label}");
            assert!(matches!(
                handler
                    .handle(Request::SetHook(SetHookRequest {
                        scope: ScopeSelector::Session(session.clone()),
                        hook: HookName::AfterKillPane,
                        command: format!("set-option -g -F {option} '#{{window_index}}'"),
                        lifecycle: HookLifecycle::Persistent,
                    }))
                    .await,
                Response::SetHook(_)
            ));

            let target = PaneTarget::with_window(session.clone(), 2, 1);
            let request = if use_sdk {
                Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::slot(target),
                    kill_all_except: false,
                })
            } else {
                Request::KillPane(KillPaneRequest {
                    target,
                    kill_all_except: false,
                })
            };
            let (outcome, hooks) = handler
                .dispatch_captured(std::process::id(), u64::from(std::process::id()), request)
                .await;
            assert!(
                matches!(outcome.response, Response::KillPane(_)),
                "{:?}",
                outcome.response
            );
            assert_eq!(hooks.len(), 1);
            assert!(hooks[0]
                .exact_pane_target
                .is_some_and(|identity| identity.window_occurrence_id.is_some()));

            assert!(matches!(
                handler
                    .handle(Request::MoveWindow(MoveWindowRequest {
                        source: Some(WindowTarget::with_window(session.clone(), 2)),
                        target: MoveWindowTarget::Window(WindowTarget::with_window(
                            session.clone(),
                            5,
                        )),
                        renumber: false,
                        kill_destination: false,
                        detached: true,
                        after: false,
                        before: false,
                    }))
                    .await,
                Response::MoveWindow(_)
            ));
            assert!(matches!(
                handler
                    .handle(Request::LinkWindow(LinkWindowRequest {
                        source: WindowTarget::with_window(session.clone(), 0),
                        target: WindowTarget::with_window(session.clone(), 2),
                        after: false,
                        before: false,
                        kill_destination: false,
                        detached: true,
                    }))
                    .await,
                Response::LinkWindow(_)
            ));

            handler
                .run_inline_hooks(std::process::id(), hooks, None)
                .await;

            let state = handler.state.lock().await;
            assert_eq!(
                state.options.resolve_name(Some(&session), &option),
                Some("5".to_owned()),
                "exact hook follows the original winlink occurrence",
            );
        }
    }

    #[tokio::test]
    async fn last_pane_after_hook_without_a_survivor_is_fail_closed() {
        for (label, use_sdk) in [("cli", false), ("sdk", true)] {
            let handler = RequestHandler::new();
            let source = session_name(&format!("after-last-pane-{label}-source"));
            let decoy = session_name(&format!("after-last-pane-{label}-decoy"));
            for session in [&source, &decoy] {
                crate::session_fixture::assert_new_session(
                    &handler
                        .handle(Request::NewSession(NewSessionRequest {
                            session_name: session.clone(),
                            detached: true,
                            size: Some(TerminalSize { cols: 80, rows: 24 }),
                            environment: None,
                        }))
                        .await,
                );
            }
            let buffer = format!("after-last-pane-{label}");
            assert!(matches!(
                handler
                    .handle(Request::SetHook(SetHookRequest {
                        scope: ScopeSelector::Global,
                        hook: HookName::AfterKillPane,
                        command: format!("set-buffer -a -b {buffer} unexpected,"),
                        lifecycle: HookLifecycle::Persistent,
                    }))
                    .await,
                Response::SetHook(_)
            ));
            let target = PaneTarget::with_window(source.clone(), 0, 0);
            let request = if use_sdk {
                Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::slot(target),
                    kill_all_except: false,
                })
            } else {
                Request::KillPane(KillPaneRequest {
                    target,
                    kill_all_except: false,
                })
            };
            let response = handler.handle(request).await;
            assert!(matches!(response, Response::KillPane(_)), "{response:?}");

            let state = handler.state.lock().await;
            assert!(state.sessions.session(&source).is_none());
            assert!(state.sessions.session(&decoy).is_some());
            assert!(
                state.buffers.show(Some(&buffer)).is_err(),
                "missing target must not fall back to the decoy session"
            );
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum SessionLookup {
    Found(rmux_proto::SessionName),
    Missing,
}

pub(in crate::handler) fn resolve_existing_session_target(
    sessions: &rmux_core::ManagedSessionStore,
    command_name: &str,
    target: &rmux_proto::SessionName,
) -> Result<rmux_proto::SessionName, RmuxError> {
    match resolve_session_lookup(sessions, command_name, target)? {
        SessionLookup::Found(session_name) => Ok(session_name),
        SessionLookup::Missing => Err(RmuxError::SessionNotFound(target.to_string())),
    }
}

pub(in crate::handler) fn resolve_session_lookup(
    sessions: &rmux_core::ManagedSessionStore,
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
    sessions: &rmux_core::ManagedSessionStore,
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
                .map(|session| (session.name(), session))
                .map(|(session_name, _)| session_name)
                .min_by(|left, right| left.as_str().cmp(right.as_str()))
                .and_then(|session_name| active_session_target(&state.sessions, session_name))
        })
}

fn pane_id_target_in_session(
    sessions: &ManagedSessionStore,
    session_name: &rmux_proto::SessionName,
    pane_id: rmux_proto::PaneId,
) -> Option<Target> {
    let session = sessions.session(session_name)?;
    let window_index = session.window_index_for_pane_id(pane_id)?;
    let pane_index = session
        .window_at(window_index)?
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)?
        .index();
    Some(Target::Pane(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane_index,
    )))
}
