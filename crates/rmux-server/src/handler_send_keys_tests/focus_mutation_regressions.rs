use super::*;
use crate::handler::QueuedLifecycleEvent;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    LinkWindowRequest, MoveWindowRequest, MoveWindowTarget, NewWindowRequest,
    RotateWindowDirection, RotateWindowRequest,
};

async fn enable_focus_mode(handler: &RequestHandler, target: &PaneTarget) {
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            b"\x1b[?1004h",
        )
        .expect("pane enables focus reporting");
}

async fn register_focused_attach(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &rmux_proto::SessionName,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    control_rx
}

async fn set_global_focus_hook(handler: &RequestHandler, hook: HookName, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn append_global_focus_hook(handler: &RequestHandler, hook: HookName, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: true,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn assert_buffer(handler: &RequestHandler, name: &str, expected: &str) {
    let state = handler.state.lock().await;
    let (_, bytes) = state
        .buffers
        .show(Some(name))
        .expect("historical hook buffer exists");
    assert_eq!(String::from_utf8_lossy(bytes), expected);
}

fn focus_and_window_events(
    lifecycle: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>,
) -> Vec<LifecycleEvent> {
    std::iter::from_fn(|| lifecycle.try_recv().ok())
        .map(|event| event.event)
        .filter(|event| {
            matches!(
                event,
                LifecycleEvent::PaneFocusIn { .. }
                    | LifecycleEvent::PaneFocusOut { .. }
                    | LifecycleEvent::WindowLinked { .. }
                    | LifecycleEvent::WindowUnlinked { .. }
                    | LifecycleEvent::SessionWindowChanged { .. }
                    | LifecycleEvent::WindowLayoutChanged { .. }
            )
        })
        .collect()
}

async fn add_detached_window(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(created) = response else {
        panic!("expected detached new-window success, got {response:?}");
    };
    created.target
}

#[path = "focus_mutation_regressions/move_window.rs"]
mod move_window;
#[path = "focus_mutation_regressions/queued_internal_effect.rs"]
mod queued_internal_effect;
#[path = "focus_mutation_regressions/removed_identity.rs"]
mod removed_identity;
#[path = "focus_mutation_regressions/rotate_window.rs"]
mod rotate_window;
