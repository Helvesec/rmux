use rmux_core::{PaneJoinOptions, PaneSwapOptions, SessionPaneTarget};
use rmux_proto::{
    CopyModeRequest, HookLifecycle, HookName, LinkWindowRequest, NewSessionRequest,
    NewWindowRequest, PaneTarget, Request, RespawnPaneRequest, Response, ScopeSelector,
    SessionName, SetHookMutationRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
    Target, TerminalSize, UnlinkWindowRequest, WindowTarget,
};

use super::{LeaseResolution, LifecycleTargetLease};
use crate::handler::RequestHandler;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn terminal_size() -> TerminalSize {
    TerminalSize { cols: 80, rows: 24 }
}

fn assert_retired(lease: &LifecycleTargetLease, state: &crate::pane_terminals::HandlerState) {
    assert!(
        matches!(lease.resolve(state), LeaseResolution::Retired(_)),
        "lease should remain a read-only retired target"
    );
}

async fn create_handler_session(handler: &RequestHandler, name: &str) -> SessionName {
    let name = session_name(name);
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: name.clone(),
            detached: true,
            size: Some(terminal_size()),
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    name
}

async fn set_global_activity_hook(handler: &RequestHandler, index: u32, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AlertActivity,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: Some(index),
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

#[tokio::test]
async fn retired_alert_target_keeps_its_deferred_hook_chain_alive() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "retained-alert-dispatch").await;
    let fallback_session = create_handler_session(&handler, "retained-alert-fallback").await;
    for (index, command) in [
        (0, "rename-window stale-retired-target"),
        (1, "set-buffer -b retained-alert-dispatch continued"),
    ] {
        set_global_activity_hook(&handler, index, command).await;
    }

    let event = {
        let mut state = handler.state.lock().await;
        let event = super::super::prepare_lifecycle_event(
            &mut state,
            &rmux_core::LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(session_name.clone(), 0),
            },
        );
        assert!(event.retained_current_target.is_some());
        state
            .sessions
            .remove_session(&session_name)
            .expect("remove retained alert session");
        state.retire_removed_lifecycle_targets();
        event
    };

    handler.dispatch_lifecycle_hook(event).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .buffers
            .show(Some("retained-alert-dispatch"))
            .expect("retired target does not discard global hook work")
            .1,
        b"continued"
    );
    assert_ne!(
        state
            .sessions
            .session(&fallback_session)
            .and_then(|session| session.window_at(0))
            .and_then(rmux_core::Window::name),
        Some("stale-retired-target"),
        "an implicit command must not retarget an unrelated live session"
    );
}

#[tokio::test]
async fn live_alert_target_follows_a_surviving_window_alias_before_dispatch() {
    let handler = RequestHandler::new();
    let alpha = session_name("retained-live-alpha");
    let beta = session_name("retained-live-beta");
    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(alpha.clone(), terminal_size())
            .expect("create source session");
        state
            .sessions
            .session_mut(&alpha)
            .expect("source session exists")
            .create_window(terminal_size())
            .expect("create spare source window");
        state
            .sessions
            .create_session(beta.clone(), terminal_size())
            .expect("create alias session");
        let source_window = state
            .sessions
            .session(&alpha)
            .and_then(|session| session.window_at(0))
            .cloned()
            .expect("source window exists");
        state
            .sessions
            .session_mut(&beta)
            .expect("alias session exists")
            .link_window(1, source_window, false, false)
            .expect("link retained window into alias session");
    }
    set_global_activity_hook(&handler, 0, "rename-window retained-live-target").await;

    let event = {
        let mut state = handler.state.lock().await;
        let event = super::super::prepare_lifecycle_event(
            &mut state,
            &rmux_core::LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(alpha.clone(), 0),
            },
        );
        state
            .sessions
            .session_mut(&alpha)
            .expect("source session exists")
            .remove_window_allowing_empty(0)
            .expect("unlink original alias");
        state.retire_removed_lifecycle_targets();
        event
    };

    handler.dispatch_lifecycle_hook(event).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(1))
            .and_then(rmux_core::Window::name),
        Some("retained-live-target")
    );
}

#[tokio::test]
async fn replaced_alert_target_rejects_the_deferred_hook_chain() {
    let handler = RequestHandler::new();
    let session_name = create_handler_session(&handler, "retained-replaced-alert").await;
    set_global_activity_hook(
        &handler,
        0,
        "set-buffer -b retained-replaced-alert should-not-run",
    )
    .await;

    let event = {
        let mut state = handler.state.lock().await;
        let event = super::super::prepare_lifecycle_event(
            &mut state,
            &rmux_core::LifecycleEvent::AlertActivity {
                target: WindowTarget::with_window(session_name.clone(), 0),
            },
        );
        let session = state
            .sessions
            .session_mut(&session_name)
            .expect("session exists");
        session
            .remove_window_allowing_empty(0)
            .expect("remove retained window");
        session
            .insert_window_with_initial_pane(0, terminal_size())
            .expect("replace retained numeric slot");
        state.retire_removed_lifecycle_targets();
        event
    };

    handler.dispatch_lifecycle_hook(event).await;

    let state = handler.state.lock().await;
    assert!(
        state.buffers.show(Some("retained-replaced-alert")).is_err(),
        "replacement rejects the complete deferred hook chain"
    );
}

#[path = "retained_target_tests/queue.rs"]
mod queue;
#[path = "retained_target_tests/registry.rs"]
mod registry;
#[path = "retained_target_tests/special_paths.rs"]
mod special_paths;
