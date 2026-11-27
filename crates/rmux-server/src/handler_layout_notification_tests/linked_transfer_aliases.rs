use rmux_core::LifecycleEvent;
use rmux_proto::{
    HookLifecycle, HookName, LinkWindowRequest, Request, Response, ScopeSelector, SessionName,
    SetHookRequest, WindowTarget,
};
use tokio::sync::oneshot;

use super::{
    create_session, register_control_client, run_detached_command, settle_control_notifications,
    wait_for_buffer_text, RequestHandler,
};
use crate::handler::pane_group_transfer_tests::create_grouped_session;

// FMT1 R2 retired the transfer reflow: a destination with no room refuses
// instead of being widened, so a join or move never publishes `window-resized`
// any more. What still has to retain the requested alias is the layout change,
// and a destination with no room must publish nothing at all.
#[tokio::test]
async fn linked_and_grouped_join_move_keep_requested_layout_alias() {
    for operation in ["join-pane", "move-pane"] {
        for family in ["linked", "grouped"] {
            for width in [1, 3] {
                assert_transfer_alias(operation, family, width).await;
            }
        }
    }
}

async fn assert_transfer_alias(operation: &str, family: &str, width: u16) {
    let label = format!("{operation}-{family}-{width}");
    let expect_refusal = width == 1;
    let alpha = session_name(&format!("resize-alias-alpha-{label}"));
    let beta = session_name(&format!("resize-alias-beta-{label}"));
    let source = session_name(&format!("resize-alias-source-{label}"));
    let buffer = format!("resize-alias-events-{label}");
    let handler = RequestHandler::new();
    let lifecycle_dispatch = handler
        .take_lifecycle_dispatch_receiver()
        .expect("test owns lifecycle dispatch");
    let (hook_shutdown_tx, hook_shutdown_rx) = oneshot::channel();
    let hook_handler = handler.clone();
    let hook_task = tokio::spawn(async move {
        hook_handler
            .consume_lifecycle_hooks(lifecycle_dispatch, hook_shutdown_rx)
            .await;
    });

    create_session(&handler, &alpha).await;
    if family == "grouped" {
        create_grouped_session(&handler, beta.as_str(), &alpha).await;
    } else {
        create_session(&handler, &beta).await;
        let linked = handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 0),
                after: false,
                before: false,
                kill_destination: true,
                detached: true,
            }))
            .await;
        assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    }
    let requested = alias_opposite_hashmap_first(&handler, &alpha, &beta).await;
    create_session(&handler, &source).await;
    run_detached_command(
        &handler,
        &format!("set-window-option -t {requested}:0 window-size manual"),
    )
    .await;
    run_detached_command(
        &handler,
        &format!("resize-window -t {requested}:0 -x {width} -y 1"),
    )
    .await;
    run_detached_command(&handler, &format!("set-buffer -b {buffer} ''")).await;
    set_layout_hook(&handler, &buffer).await;

    let (control_pid, mut notifications) = register_control_client(&handler, &requested).await;
    let _ = settle_control_notifications(&mut notifications).await;
    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    let command = format!("{operation} -h -d -s {source}:0.0 -t {requested}:0.0");
    let error = super::run_control_command_result(&handler, control_pid, &command).await;
    if expect_refusal {
        let error =
            error.unwrap_or_else(|| panic!("{label}: a destination with no room must refuse"));
        assert!(
            error.to_string().contains("cannot split target pane"),
            "{label}: unexpected refusal {error}"
        );
    } else {
        assert!(error.is_none(), "{label}: {error:?}");
    }
    let control_lines = settle_control_notifications(&mut notifications).await;
    let hook_events = wait_for_buffer_text(&handler, &buffer, requested.as_str()).await;
    let mut events = Vec::new();
    while let Ok(Ok(event)) =
        tokio::time::timeout(super::CONTROL_NOTIFICATION_POLL, lifecycle_events.recv()).await
    {
        events.push(event.event);
    }

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, LifecycleEvent::WindowResized { .. })),
        "{label}: a transfer never resizes its destination window; events={events:?}"
    );
    let layout_targets = events
        .iter()
        .filter_map(|event| match event {
            LifecycleEvent::WindowLayoutChanged { target } => Some(target.clone()),
            _ => None,
        })
        .filter(|target| target.window_index() == 0)
        .collect::<Vec<_>>();
    if expect_refusal {
        assert!(
            layout_targets.is_empty(),
            "{label}: a refusal publishes nothing; events={events:?}"
        );
        assert!(
            hook_events.is_none(),
            "{label}: a refusal fires no hook; got {hook_events:?}"
        );
    } else {
        assert!(
            layout_targets.contains(&WindowTarget::with_window(requested.clone(), 0)),
            "{label}: the lifecycle layout change must retain the requested alias; events={events:?}"
        );
        assert_eq!(
            hook_events,
            Some(requested.to_string()),
            "{label}: the hook format context must retain the requested session alias"
        );
    }
    assert_eq!(
        control_lines
            .iter()
            .filter(|line| line.starts_with("%layout-change "))
            .count(),
        usize::from(!expect_refusal),
        "{label}: linked/grouped aliases must still receive one control layout"
    );

    let _ = hook_shutdown_tx.send(());
    hook_task.await.expect("lifecycle hook task");
}

async fn alias_opposite_hashmap_first(
    handler: &RequestHandler,
    alpha: &SessionName,
    beta: &SessionName,
) -> SessionName {
    let state = handler.state.lock().await;
    let shared_window_id = state
        .sessions
        .session(alpha)
        .expect("alpha")
        .window_at(0)
        .expect("alpha window")
        .id();
    let first = state
        .sessions
        .iter()
        .map(|session| (session.name(), session))
        .find_map(|(session_name, session)| {
            session
                .window_at(0)
                .is_some_and(|window| window.id() == shared_window_id)
                .then(|| session_name.clone())
        })
        .expect("shared alias");
    if first == *alpha {
        beta.clone()
    } else {
        alpha.clone()
    }
}

async fn set_layout_hook(handler: &RequestHandler, buffer: &str) {
    let response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::WindowLayoutChanged,
            command: format!("run-shell -C 'set-buffer -b {buffer} #{{session_name}}'"),
            lifecycle: HookLifecycle::Persistent,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("test session name")
}
