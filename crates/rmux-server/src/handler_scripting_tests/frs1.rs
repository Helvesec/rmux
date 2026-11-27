use super::*;
use crate::handler::QueuedLifecycleEvent;
use rmux_core::{FloatingPaneRect, LifecycleEvent, Window};
use rmux_proto::{KillPaneRequest, ResizePaneAdjustment, ResizePaneTargetActionRequest};
use std::{
    future::{poll_fn, Future},
    task::Poll,
};

pub(super) async fn setup(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    run_hook_probe_queue(&handler, "set-option -g status off").await;
    #[cfg(unix)]
    run_hook_probe_queue(&handler, "set-option -g default-shell /bin/sh").await;
    let name = session_name(name);
    create_background_identity_session(&handler, name.clone()).await;
    queue(
        &handler,
        &format!("new-pane -d -t {name}:0.0 -x30 -y8 -X3 -Y4"),
    )
    .await
    .unwrap();
    (handler, name)
}

pub(super) async fn setup_quiet_window(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    use_platform_test_shell(&handler).await;
    queue(&handler, "set-option -g status off").await.unwrap();
    let command = crate::test_shell::stdin_discard_command();
    queue(
        &handler,
        &format!(
            "set-option -g default-command {}",
            crate::test_shell::command_quote(&command)
        ),
    )
    .await
    .unwrap();
    let name = session_name(name);
    // Keep live PTYs, but no interactive prompts or automatic name producer.
    // Replacement panes inherit the same silent command. Full Window equality
    // below still observes both name and activity_at.
    queue(
        &handler,
        &format!("new-session -d -s {name} -n snapshot-witness -x80 -y24"),
    )
    .await
    .unwrap();
    queue(
        &handler,
        &format!("new-pane -d -t {name}:0.0 -x30 -y8 -X3 -Y4"),
    )
    .await
    .unwrap();
    wait_for_quiet_window_terminals(&handler, &name).await;
    (handler, name)
}

pub(super) async fn wait_for_quiet_window_terminals(handler: &RequestHandler, name: &SessionName) {
    let current = window(handler, name).await;
    for pane in current.panes() {
        handler
            .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
                name.clone(),
                0,
                pane.index(),
            ))
            .await;
    }
}

pub(super) async fn queue(
    handler: &RequestHandler,
    command: &str,
) -> Result<rmux_proto::CommandOutput, RmuxError> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new().parse(command).unwrap(),
        )
        .await
}

pub(super) async fn window(handler: &RequestHandler, name: &SessionName) -> Window {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(name)
        .unwrap()
        .window()
        .clone()
}

pub(super) fn floating_rect(window: &Window, pane_index: u32) -> FloatingPaneRect {
    let pane_id = window.pane(pane_index).unwrap().id();
    window.floating_stack().placement(pane_id).unwrap().rect()
}

// swap-pane rejects floating endpoints in tmux. These race witnesses need
// exchanged identities, not that forbidden command: tile the endpoints first,
// swap them, then restore each slot's floating placement under the same lock.
pub(super) fn swap_fixture_identities(
    sessions: &mut rmux_core::ManagedSessionStore,
    target: &rmux_proto::WindowTarget,
    source: u32,
    destination: u32,
) {
    let window = sessions.window(target).unwrap();
    let placements: Vec<_> = window
        .floating_stack()
        .placements()
        .iter()
        .filter_map(|placement| {
            let index = window
                .panes()
                .iter()
                .find(|pane| pane.id() == placement.pane_id())
                .unwrap()
                .index();
            [source, destination]
                .contains(&index)
                .then_some((index, *placement))
        })
        .collect();
    {
        let mut layout = sessions.window_layout_context_mut(target).unwrap();
        for (index, _) in &placements {
            layout
                .tile_floating_pane(
                    *index,
                    Some(0),
                    rmux_proto::SplitDirection::Horizontal,
                    false,
                )
                .unwrap();
        }
    }
    let _ = sessions
        .swap_panes(
            target,
            source,
            target,
            destination,
            rmux_core::PaneSwapOptions::new(true, false),
        )
        .unwrap();
    let mut layout = sessions.window_layout_context_mut(target).unwrap();
    for (index, placement) in placements {
        layout.float_pane(index, placement.rect()).unwrap();
        layout
            .set_floating_pane_visible(index, placement.visible())
            .unwrap();
    }
}

fn layout_events(events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>) -> usize {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }))
        .count()
}

pub(super) async fn cleanup(handler: &RequestHandler) {
    assert!(matches!(
        handler
            .handle(Request::KillServer(rmux_proto::KillServerRequest))
            .await,
        Response::KillServer(_)
    ));
}

#[tokio::test]
async fn frs1_absolute_resize_scopes_runtime_and_preserves_neighbors() {
    let (handler, name) = setup("frs").await;
    let initial = window(&handler, &name).await;
    let tiled = initial.pane(0).unwrap().geometry();
    let active = initial.active_pane_index();
    let float_id = initial.pane(1).unwrap().id();
    let mut events = handler.subscribe_lifecycle_events();
    let runtime_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();

    queue(&handler, "resizep -t frs:0.1 -x25").await.unwrap();
    let resized = window(&handler, &name).await;
    assert_eq!(
        floating_rect(&resized, 1),
        FloatingPaneRect::new(3, 4, 25, 8).unwrap(),
        "absolute resize must update the captured floating pane placement"
    );
    assert_eq!(resized.pane(0).unwrap().geometry(), tiled);
    assert_eq!(resized.active_pane_index(), active);
    assert_eq!(resized.pane(1).unwrap().id(), float_id);
    assert_eq!(layout_events(&mut events), 1);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before + 1
    );

    let runtime_before_noop = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    queue(&handler, "resize-pane -t frs:0.1 -x25")
        .await
        .unwrap();
    assert_eq!(layout_events(&mut events), 0);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before_noop
    );

    queue(&handler, "resize-pane -t frs:0.0 -Z").await.unwrap();
    assert!(window(&handler, &name).await.is_zoomed());
    let _ = layout_events(&mut events);
    let runtime_before_unzoom = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    queue(&handler, "resize-pane -t frs:0.1 -y6").await.unwrap();
    let unzoomed = window(&handler, &name).await;
    assert!(!unzoomed.is_zoomed());
    assert_eq!(
        floating_rect(&unzoomed, 1),
        FloatingPaneRect::new(3, 4, 25, 6).unwrap()
    );
    assert_eq!(layout_events(&mut events), 1);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before_unzoom + 1
    );

    queue(&handler, "resize-pane -t frs:0.0 -Z").await.unwrap();
    let zero_before = window(&handler, &name).await;
    let runtime_before_zero = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    assert!(queue(&handler, "resize-pane -t frs:0.1 -x0 -y5")
        .await
        .is_ok());
    let normalized = window(&handler, &name).await;
    assert!(!normalized.is_zoomed());
    assert_eq!(
        floating_rect(&normalized, 1),
        FloatingPaneRect::new(3, 4, 1, 5).unwrap()
    );
    assert_eq!(
        normalized.pane(0).unwrap().geometry(),
        rmux_core::PaneGeometry::new(0, 0, 80, 27)
    );
    assert_eq!(
        normalized.pane(1).unwrap().geometry(),
        rmux_core::PaneGeometry::new(3, 4, 1, 5)
    );
    assert_eq!(
        normalized.active_pane_index(),
        zero_before.active_pane_index()
    );
    assert_eq!(normalized.panes().len(), zero_before.panes().len());
    for (before, after) in zero_before.panes().iter().zip(normalized.panes()) {
        assert_eq!(after.id(), before.id());
    }
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before_zero + 1
    );
    let invalid_before = normalized;
    assert!(queue(&handler, "resize-pane -t frs:0.1 -x20 -R2")
        .await
        .is_err());
    assert_eq!(window(&handler, &name).await, invalid_before);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before_zero + 1
    );

    queue(&handler, "resize-pane -t frs:0.1 -x50%")
        .await
        .unwrap();
    let percent = window(&handler, &name).await;
    assert!(!percent.is_zoomed());
    assert_eq!(floating_rect(&percent, 1).cols(), 40);

    let linked = session_name("frs-linked");
    create_background_identity_session(&handler, linked.clone()).await;
    queue(&handler, "link-window -d -s frs:0 -t frs-linked:1")
        .await
        .unwrap();
    queue(&handler, "resizep -t frs-linked:1.1 -x33")
        .await
        .unwrap();
    let owner = window(&handler, &name).await;
    let alias = handler
        .state
        .lock()
        .await
        .sessions
        .session(&linked)
        .unwrap()
        .window_at(1)
        .unwrap()
        .clone();
    assert_eq!(owner.floating_stack(), alias.floating_stack());
    assert_eq!(floating_rect(&owner, 1).cols(), 33);

    assert!(matches!(
        handler
            .handle(Request::PaneResize(rmux_proto::PaneResizeRequest {
                target: rmux_proto::PaneTargetRef::by_id(name.clone(), float_id),
                adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 32 },
            }))
            .await,
        Response::ResizePane(_)
    ));
    let owner = window(&handler, &name).await;
    assert_eq!(floating_rect(&owner, 1).cols(), 32);

    let rollback_before = owner;
    let runtime_before_failure = {
        let mut state = handler.state.lock().await;
        let count = state.window_runtime_resize_count_for_test();
        state.fail_next_resize_for_test();
        count
    };
    assert!(queue(&handler, "resize-pane -t frs:0.1 -x31")
        .await
        .is_err());
    assert_eq!(window(&handler, &name).await, rollback_before);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before_failure + 2
    );
    cleanup(&handler).await;
}

#[tokio::test]
async fn frs1_target_action_revalidates_identity_and_write_access() {
    let (handler, name) = setup("frs-race").await;
    let removed_id = window(&handler, &name).await.pane(1).unwrap().id();
    let pause = super::super::super::target_action_support::install_target_resolution_pause(
        name.clone(),
        removed_id,
    );
    let resize_handler = handler.clone();
    let resize = tokio::spawn(async move {
        resize_handler
            .handle(Request::ResizePaneTargetAction(
                ResizePaneTargetActionRequest {
                    target: Some("frs-race:0.1".to_owned()),
                    adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 19 },
                },
            ))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("resize reaches the post-resolution pause");
    assert!(matches!(
        handler
            .handle(Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, 1),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    queue(&handler, "new-pane -d -t frs-race:0.0 -x18 -y5 -X2 -Y3")
        .await
        .unwrap();
    let replacement = window(&handler, &name).await;
    assert_ne!(replacement.pane(1).unwrap().id(), removed_id);
    pause.release.notify_one();
    assert!(matches!(
        resize.await.unwrap(),
        Response::Error(rmux_proto::ErrorResponse { .. })
    ));
    assert_eq!(floating_rect(&window(&handler, &name).await, 1).cols(), 18);

    let replacement_id = replacement.pane(1).unwrap().id();
    let requester_pid = std::process::id();
    let _control =
        super::super::register_control_for_session(&handler, requester_pid, name.clone()).await;
    let pause = super::super::super::target_action_support::install_target_resolution_pause(
        name.clone(),
        replacement_id,
    );
    let resize_handler = handler.clone();
    let resize = tokio::spawn(async move {
        resize_handler
            .handle(Request::ResizePaneTargetAction(
                ResizePaneTargetActionRequest {
                    target: Some("frs-race:0.1".to_owned()),
                    adjustment: ResizePaneAdjustment::AbsoluteHeight { rows: 7 },
                },
            ))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("resize reaches the authority revalidation pause");
    handler
        .active_control
        .lock()
        .await
        .by_pid
        .get_mut(&requester_pid)
        .unwrap()
        .can_write = false;
    pause.release.notify_one();
    assert!(matches!(
        resize.await.unwrap(),
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::Server(message)
        }) if message == "client is read-only"
    ));
    assert_eq!(floating_rect(&window(&handler, &name).await, 1).rows(), 5);
    cleanup(&handler).await;
}

#[tokio::test]
async fn frs1_resize_captures_identity_with_initial_slot_resolution() {
    let (handler, name) = setup("frs-r1-gap").await;
    queue(&handler, "new-pane -d -t frs-r1-gap:0.0 -x18 -y5 -X2 -Y3")
        .await
        .unwrap();
    let before = window(&handler, &name).await;
    let original = before.pane(1).unwrap().id();
    let replacement = before.pane(2).unwrap().id();

    let held = handler.state.lock().await;
    let mut resize = Box::pin(handler.handle_resize_pane_target_action(
        std::process::id(),
        ResizePaneTargetActionRequest {
            target: Some("frs-r1-gap:0.1".to_owned()),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 19 },
        },
    ));
    poll_fn(|cx| {
        assert!(resize.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut interloper = Box::pin(handler.state.lock());
    poll_fn(|cx| {
        assert!(interloper.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;

    drop(held);
    poll_fn(|cx| {
        assert!(resize.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut state = interloper.await;
    let target = rmux_proto::WindowTarget::with_window(name.clone(), 0);
    swap_fixture_identities(&mut state.sessions, &target, 1, 2);
    drop(state);

    let response = resize.await;
    let after = window(&handler, &name).await;
    cleanup(&handler).await;
    assert_eq!(after.pane(1).unwrap().id(), replacement);
    assert_eq!(after.pane(2).unwrap().id(), original);
    assert!(
        matches!(response, Response::Error(_)),
        "unexpected resize response: {response:?}"
    );
    assert_ne!(floating_rect(&after, 1).cols(), 19);
}
