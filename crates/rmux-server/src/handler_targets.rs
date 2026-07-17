use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rmux_core::{
    command_target_metadata, OptionStore, SessionStore, TargetFindContext, UnresolvedTarget,
};
use rmux_proto::request::{Request, ResolveTargetType};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, OptionName, PaneTarget, PaneTargetRef, ResolveTargetResponse, Response,
    RmuxError, ScopeSelector, Target, WindowTarget,
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
        HookLifecycle, HookName, KillPaneRequest, LinkWindowRequest, MoveWindowRequest,
        MoveWindowTarget, NewSessionExtRequest, NewSessionRequest, NewWindowRequest,
        PaneKillRequest, ResolveTargetRequest, Response, ScopeSelector, SelectPaneRequest,
        SetHookRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget, Target,
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
            assert!(matches!(response, Response::NewSession(_)), "{response:?}");
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
            assert!(matches!(response, Response::NewSession(_)), "{response:?}");
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
            assert!(matches!(
                handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
                Response::NewSession(_)
            ));
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
            assert!(matches!(response, Response::NewSession(_)), "{response:?}");
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
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
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
            assert!(matches!(
                handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
                Response::NewSession(_)
            ));
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
            assert!(matches!(
                handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
                        detached: true,
                        size: Some(TerminalSize { cols: 80, rows: 24 }),
                        environment: None,
                    }))
                    .await,
                Response::NewSession(_)
            ));
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
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: owner.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
        ));
        assert!(matches!(
            handler
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
            Response::NewSession(_)
        ));
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
        assert!(matches!(
            handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
            Response::NewSession(_)
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
            assert!(matches!(
                handler
                    .handle(Request::NewSession(NewSessionRequest {
                        session_name: session.clone(),
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
                assert!(matches!(
                    handler
                        .handle(Request::NewSession(NewSessionRequest {
                            session_name: session.clone(),
                            detached: true,
                            size: Some(TerminalSize { cols: 80, rows: 24 }),
                            environment: None,
                        }))
                        .await,
                    Response::NewSession(_)
                ));
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

fn pane_id_target_in_session(
    sessions: &SessionStore,
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

fn pane_kill_request_target(sessions: &SessionStore, target: &PaneTargetRef) -> Option<Target> {
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
