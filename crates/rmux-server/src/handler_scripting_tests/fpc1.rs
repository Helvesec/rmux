use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use crate::handler::QueuedLifecycleEvent;
use rmux_core::{
    FloatingPaneRect, LifecycleEvent, ResolvedPaneBorderStatus, ResolvedPaneLayer,
    ResolvedSceneContext, ResolvedZoomVisibility,
};
use rmux_proto::{KillPaneRequest, RespawnPaneRequest};
use std::fs;
use std::future::{poll_fn, Future};
use std::task::Poll;
use std::time::Duration;

#[cfg(target_os = "macos")]
mod native_resize;

fn layout_events(events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>) -> usize {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }))
        .count()
}

fn stack_ids(window: &rmux_core::Window) -> Vec<rmux_core::PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

fn pane_ids(window: &rmux_core::Window) -> Vec<rmux_core::PaneId> {
    window.panes().iter().map(rmux_core::Pane::id).collect()
}

async fn queue(
    handler: &RequestHandler,
    command: &str,
) -> Result<rmux_proto::CommandOutput, RmuxError> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .with_exact_commands(rmux_core::command_inventory::RMUX_EXTENSION_COMMANDS)
                .parse(command)
                .unwrap(),
        )
        .await
}

async fn placement_offer(
    handler: &RequestHandler,
    arguments: &[&str],
) -> RuntimeCommandExecutionResponse {
    let mut request = RuntimeCommandExecutionRequest::new(
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        None,
    );
    request
        .handshake
        .required_capabilities
        .push("commands.set_pane_placement.v1".to_owned());
    super::execution_result(
        handler
            .handle(Request::RuntimeCommandExecution(Box::new(request)))
            .await,
    )
}

#[tokio::test]
async fn fpc1_public_offer_round_trips_one_live_pane_without_replacement() {
    let (handler, name) = super::frs1::setup("fpc-public").await;
    let before = super::frs1::window(&handler, &name).await;
    let pane_id = before.pane(0).unwrap().id();

    assert!(matches!(
        placement_offer(
            &handler,
            &["set-pane-placement", "-t", "fpc-public:0.0", "floating"],
        )
        .await,
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
    let floating = super::frs1::window(&handler, &name).await;
    assert_eq!(floating.pane(0).unwrap().id(), pane_id);
    assert!(floating.floating_stack().contains(pane_id));

    assert!(matches!(
        placement_offer(
            &handler,
            &["set-pane-placement", "-t", "fpc-public:0.0", "tiled"],
        )
        .await,
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
    let tiled = super::frs1::window(&handler, &name).await;
    assert_eq!(tiled.pane(0).unwrap().id(), pane_id);
    assert!(!tiled.floating_stack().contains(pane_id));

    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_queue_round_trip_preserves_identity_selection_stack_and_process() {
    let (handler, name) = super::frs1::setup("fpc-round").await;
    super::frs1::queue(&handler, "split-window -d -h -t fpc-round:0.0")
        .await
        .unwrap();
    let initial = super::frs1::window(&handler, &name).await;
    let stack = initial.floating_stack();
    let source = initial
        .panes()
        .iter()
        .find(|pane| pane.index() != 0 && !stack.contains(pane.id()))
        .expect("new split is the second tiled pane");
    let source_id = source.id();
    let source_index = source.index();
    let source_rect = FloatingPaneRect::new(
        i32::from(source.geometry().x()),
        i32::from(source.geometry().y()),
        i32::from(source.geometry().cols()),
        i32::from(source.geometry().rows()),
    )
    .unwrap();
    let witness_float = stack.placements()[0].pane_id();
    let witness_index = initial
        .panes()
        .iter()
        .find(|pane| pane.id() == witness_float)
        .unwrap()
        .index();
    let witness_rect = super::frs1::floating_rect(&initial, witness_index);
    let active = initial.active_pane_index();
    let last = initial.last_pane_index();
    let ids = pane_ids(&initial);
    let lifecycle = handler
        .state
        .lock()
        .await
        .pane_lifecycle(source_id)
        .unwrap()
        .clone();
    let mut events = handler.subscribe_lifecycle_events();

    queue(
        &handler,
        &format!("set-pane-placement -t fpc-round:0.{source_index} floating"),
    )
    .await
    .unwrap();
    let floating = super::frs1::window(&handler, &name).await;
    assert_eq!(pane_ids(&floating), ids);
    assert_eq!(floating.active_pane_index(), active);
    assert_eq!(floating.last_pane_index(), last);
    assert_eq!(stack_ids(&floating), vec![witness_float, source_id]);
    assert_eq!(
        super::frs1::floating_rect(&floating, source_index),
        source_rect
    );
    assert_eq!(
        super::frs1::floating_rect(&floating, witness_index),
        witness_rect
    );
    let scene = floating.resolved_scene(ResolvedSceneContext::new(
        floating.size(),
        ResolvedPaneBorderStatus::Off,
        ResolvedZoomVisibility::Rendered,
    ));
    let scene_source = scene.pane_by_id(source_id).unwrap();
    assert!(matches!(
        scene_source.layer(),
        ResolvedPaneLayer::Floating(_)
    ));
    assert_eq!(scene_source.content().x(), source_rect.x());
    assert_eq!(scene_source.content().y(), source_rect.y());

    queue(
        &handler,
        &format!("set-pane-placement -t fpc-round:0.{source_index} -a fpc-round:0.0 -h tiled"),
    )
    .await
    .unwrap();
    let horizontal = super::frs1::window(&handler, &name).await;
    let anchor = horizontal.pane(0).unwrap().geometry();
    let source = horizontal.pane(source_index).unwrap().geometry();
    assert_eq!(anchor.rows(), horizontal.size().rows);
    assert_eq!(source.rows(), horizontal.size().rows);
    assert_eq!(source.x(), anchor.cols() + 1);
    assert_eq!((anchor.cols(), source.cols()), (40, 39));
    assert_eq!(stack_ids(&horizontal), vec![witness_float]);
    let scene = horizontal.resolved_scene(ResolvedSceneContext::new(
        horizontal.size(),
        ResolvedPaneBorderStatus::Off,
        ResolvedZoomVisibility::Rendered,
    ));
    assert_eq!(
        scene.pane_by_id(source_id).unwrap().layer(),
        ResolvedPaneLayer::Tiled
    );
    assert_eq!(horizontal.active_pane_index(), active);
    assert_eq!(horizontal.last_pane_index(), last);

    queue(
        &handler,
        &format!("set-pane-placement -t fpc-round:0.{source_index} floating"),
    )
    .await
    .unwrap();
    queue(
        &handler,
        &format!("set-pane-placement -t fpc-round:0.{source_index} -a fpc-round:0.0 -v -b tiled"),
    )
    .await
    .unwrap();
    let vertical_before = super::frs1::window(&handler, &name).await;
    let source = vertical_before.pane(source_index).unwrap().geometry();
    let anchor = vertical_before.pane(0).unwrap().geometry();
    assert_eq!(source.y(), 0);
    assert_eq!(anchor.y(), source.rows() + 1);
    assert_eq!((source.rows(), anchor.rows()), (12, 11));
    assert_eq!(source.cols(), vertical_before.size().cols);
    assert_eq!(anchor.cols(), vertical_before.size().cols);
    assert_eq!(pane_ids(&vertical_before), ids);
    assert_eq!(layout_events(&mut events), 4);
    let state = handler.state.lock().await;
    let lifecycle_after = state.pane_lifecycle(source_id).unwrap();
    assert_eq!(lifecycle_after.pane_id, lifecycle.pane_id);
    assert_eq!(lifecycle_after.window_id, lifecycle.window_id);
    assert_eq!(lifecycle_after.process, lifecycle.process);
    assert_eq!(lifecycle_after.generation, lifecycle.generation);
    assert_eq!(lifecycle_after.command(), lifecycle.command());
    drop(state);

    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_noops_and_refusals_preserve_zoom_without_resize_or_notification() {
    let (handler, name) = super::frs1::setup("fpc-noop").await;
    super::frs1::queue(&handler, "resize-pane -t fpc-noop:0.0 -Z")
        .await
        .unwrap();
    let zoomed = super::frs1::window(&handler, &name).await;
    assert!(zoomed.is_zoomed());
    let runtime_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();

    queue(&handler, "set-pane-placement -t fpc-noop:0.0 tiled")
        .await
        .unwrap();
    for invalid in [
        "set-pane-placement -t fpc-noop:0.0 -a fpc-noop:0.0 tiled",
        "set-pane-placement -t fpc-noop:0.0 -h tiled",
        "set-pane-placement -t fpc-noop:0.0 -h floating",
        "set-pane-placement -t fpc-noop:0.0 -t fpc-noop:0.1 floating",
        "set-pane-placement -t fpc-noop:0.0 floating trailing",
    ] {
        assert!(queue(&handler, invalid).await.is_err(), "{invalid}");
        assert_eq!(
            super::frs1::window(&handler, &name).await,
            zoomed,
            "{invalid}"
        );
    }
    assert_eq!(super::frs1::window(&handler, &name).await, zoomed);
    assert_eq!(layout_events(&mut events), 0);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_count
    );

    queue(&handler, "set-pane-placement -t fpc-noop:0.0 floating")
        .await
        .unwrap();
    let all_floating = super::frs1::window(&handler, &name).await;
    assert!(!all_floating.is_zoomed());
    assert!(all_floating.layout_dump().is_empty());
    assert_eq!(layout_events(&mut events), 1);

    super::frs1::queue(&handler, "resize-pane -t fpc-noop:0.1 -Z")
        .await
        .unwrap();
    let floating_zoomed = super::frs1::window(&handler, &name).await;
    assert!(floating_zoomed.is_zoomed());
    let _ = layout_events(&mut events);
    queue(&handler, "set-pane-placement -t fpc-noop:0.0 floating")
        .await
        .unwrap();
    assert_eq!(
        super::frs1::window(&handler, &name).await,
        floating_zoomed,
        "an already-floating no-op must preserve another pane's zoom"
    );
    assert_eq!(layout_events(&mut events), 0);

    let before_invalid_return = floating_zoomed;
    assert!(
        queue(&handler, "set-pane-placement -t fpc-noop:0.0 -h tiled",)
            .await
            .is_err()
    );
    assert_eq!(
        super::frs1::window(&handler, &name).await,
        before_invalid_return
    );
    queue(&handler, "set-pane-placement -t fpc-noop:0.0 tiled")
        .await
        .unwrap();
    let only_tile = super::frs1::window(&handler, &name).await;
    assert!(!only_tile.is_zoomed());
    assert_eq!(
        only_tile.pane(0).unwrap().geometry().cols(),
        only_tile.size().cols
    );
    assert_eq!(
        only_tile.pane(0).unwrap().geometry().rows(),
        only_tile.size().rows
    );
    assert_eq!(stack_ids(&only_tile), vec![only_tile.pane(1).unwrap().id()]);
    assert_eq!(layout_events(&mut events), 1);

    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_offer_alias_and_batch_admission_are_exact_and_atomic() {
    let (handler, name) = super::frs1::setup("fpc-offer").await;
    let before = super::frs1::window(&handler, &name).await;
    assert!(matches!(
        super::execution_result(
            handler
                .handle(Request::RuntimeCommandExecution(Box::new(
                    RuntimeCommandExecutionRequest::new(
                        vec![
                            "set-pane-placement".to_owned(),
                            "-t".to_owned(),
                            "fpc-offer:0.0".to_owned(),
                            "floating".to_owned(),
                        ],
                        None,
                    ),
                )))
                .await,
        ),
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    assert_eq!(super::frs1::window(&handler, &name).await, before);

    set_alias(
        &handler,
        "detach=set-pane-placement -t fpc-offer:0.0 floating",
    )
    .await;
    let mut alias = RuntimeCommandExecutionRequest::new(vec!["detach".to_owned()], None);
    alias
        .handshake
        .required_capabilities
        .push("commands.set_pane_placement.v1".to_owned());
    assert!(matches!(
        super::execution_result(
            handler
                .handle(Request::RuntimeCommandExecution(Box::new(alias)))
                .await,
        ),
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));

    let mut batch = RuntimeCommandExecutionRequest::new(
        [
            "set-environment",
            "-g",
            "FPC_BAD",
            "yes",
            ";",
            "set-pane-placement",
            "-t",
            "fpc-offer:0.0",
            "tiled",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        None,
    );
    batch
        .handshake
        .required_capabilities
        .push("commands.set_pane_placement.v1".to_owned());
    assert!(matches!(
        super::execution_result(
            handler
                .handle(Request::RuntimeCommandExecution(Box::new(batch)))
                .await,
        ),
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("FPC_BAD"),
        None
    );
    assert!(super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(before.pane(0).unwrap().id()));
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_source_and_anchor_replacements_are_refused_then_fresh_work_progresses() {
    let (handler, name) = super::frs1::setup_quiet_window("fpc-source-race").await;
    super::frs1::queue(&handler, "split-window -d -h -t fpc-source-race:0.0")
        .await
        .unwrap();
    let initial = super::frs1::window(&handler, &name).await;
    let stack = initial.floating_stack();
    let source = initial
        .panes()
        .iter()
        .find(|pane| pane.index() != 0 && !stack.contains(pane.id()))
        .unwrap();
    let source_index = source.index();
    let source_id = source.id();
    let pause = install_queue_exact_target_capture_pause(&handler, "set-pane-placement");
    let queued_handler = handler.clone();
    let stale = tokio::spawn(async move {
        queue(
            &queued_handler,
            &format!("set-pane-placement -t fpc-source-race:0.{source_index} floating"),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("source conversion reaches post-capture pause");
    assert!(matches!(
        handler
            .handle(Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, source_index),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    super::frs1::queue(&handler, "split-window -d -h -t fpc-source-race:0.0")
        .await
        .unwrap();
    let replacement = super::frs1::window(&handler, &name).await;
    let replacement = replacement
        .panes()
        .iter()
        .find(|pane| pane.index() != 0 && !replacement.floating_stack().contains(pane.id()))
        .unwrap();
    assert_ne!(replacement.id(), source_id);
    let replacement_index = replacement.index();
    super::frs1::wait_for_quiet_window_terminals(&handler, &name).await;
    let before_stale_commit = super::frs1::window(&handler, &name).await;
    let resize_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();
    pause.release.notify_one();
    assert!(stale.await.unwrap().is_err());
    assert_eq!(
        super::frs1::window(&handler, &name).await,
        before_stale_commit
    );
    assert_eq!(layout_events(&mut events), 0);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_count
    );
    queue(
        &handler,
        &format!("set-pane-placement -t fpc-source-race:0.{replacement_index} floating"),
    )
    .await
    .unwrap();
    assert!(super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(replacement.id()));
    super::frs1::cleanup(&handler).await;

    let (handler, name) = super::frs1::setup_quiet_window("fpc-anchor-race").await;
    super::frs1::queue(&handler, "split-window -d -h -t fpc-anchor-race:0.0")
        .await
        .unwrap();
    let initial = super::frs1::window(&handler, &name).await;
    let source_id = initial.floating_stack().placements()[0].pane_id();
    let source_index = initial
        .panes()
        .iter()
        .find(|pane| pane.id() == source_id)
        .unwrap()
        .index();
    let anchor = initial
        .panes()
        .iter()
        .find(|pane| pane.index() != 0 && !initial.floating_stack().contains(pane.id()))
        .unwrap();
    let anchor_index = anchor.index();
    let anchor_id = anchor.id();
    let pause = install_queue_exact_target_capture_pause(&handler, "set-pane-placement");
    let queued_handler = handler.clone();
    let stale = tokio::spawn(async move {
        queue(
            &queued_handler,
            &format!(
                "set-pane-placement -t fpc-anchor-race:0.{source_index} -a fpc-anchor-race:0.{anchor_index} -h tiled"
            ),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("anchor conversion reaches post-capture pause");
    assert!(matches!(
        handler
            .handle(Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, anchor_index),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    super::frs1::queue(&handler, "split-window -d -h -t fpc-anchor-race:0.0")
        .await
        .unwrap();
    super::frs1::wait_for_quiet_window_terminals(&handler, &name).await;
    let current = super::frs1::window(&handler, &name).await;
    let replacement = current
        .panes()
        .iter()
        .find(|pane| {
            pane.index() != 0
                && pane.id() != source_id
                && !current.floating_stack().contains(pane.id())
        })
        .unwrap();
    assert_ne!(replacement.id(), anchor_id);
    let replacement_index = replacement.index();
    let current_source_index = current
        .panes()
        .iter()
        .find(|pane| pane.id() == source_id)
        .unwrap()
        .index();
    let before_stale_commit = current.clone();
    pause.release.notify_one();
    assert!(stale.await.unwrap().is_err());
    assert_eq!(
        super::frs1::window(&handler, &name).await,
        before_stale_commit
    );
    queue(
        &handler,
        &format!(
            "set-pane-placement -t fpc-anchor-race:0.{current_source_index} -a fpc-anchor-race:0.{replacement_index} -h tiled"
        ),
    )
    .await
    .unwrap();
    assert!(!super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(source_id));
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_respawn_generation_and_revoked_commit_authority_are_revalidated() {
    let (handler, name) = super::frs1::setup("fpc-generation").await;
    let source_id = super::frs1::window(&handler, &name)
        .await
        .pane(0)
        .unwrap()
        .id();
    let pause = install_queue_exact_target_capture_pause(&handler, "set-pane-placement");
    let queued_handler = handler.clone();
    let stale = tokio::spawn(async move {
        queue(
            &queued_handler,
            "set-pane-placement -t fpc-generation:0.0 floating",
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("conversion reaches generation pause");
    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, 0),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![crate::test_shell::stdin_discard_command()]),
                process_command: None,
            })))
            .await,
        Response::RespawnPane(_)
    ));
    let generation = handler
        .state
        .lock()
        .await
        .pane_lifecycle(source_id)
        .unwrap()
        .generation;
    pause.release.notify_one();
    assert!(stale.await.unwrap().is_err());
    queue(
        &handler,
        "set-pane-placement -t fpc-generation:0.0 floating",
    )
    .await
    .unwrap();
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .pane_lifecycle(source_id)
            .unwrap()
            .generation,
        generation
    );
    super::frs1::cleanup(&handler).await;

    let (handler, name) = super::frs1::setup_quiet_window("fpc-rights").await;
    let pid = std::process::id();
    let _control = super::super::register_control_for_session(&handler, pid, name.clone()).await;
    let before = super::frs1::window(&handler, &name).await;
    let runtime_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();
    let pause = install_queue_exact_target_capture_pause(&handler, "set-pane-placement");
    let mut converting = Box::pin(queue(
        &handler,
        "set-pane-placement -t fpc-rights:0.0 floating",
    ));
    tokio::select! {
        _ = pause.reached.notified() => {},
        result = &mut converting => panic!("expected capture pause: {result:?}"),
    }
    let state = handler.state.lock().await;
    pause.release.notify_one();
    poll_fn(|cx| {
        assert!(converting.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    handler
        .active_control
        .lock()
        .await
        .by_pid
        .get_mut(&pid)
        .unwrap()
        .can_write = false;
    drop(state);
    assert!(converting.await.is_err());
    assert_eq!(super::frs1::window(&handler, &name).await, before);
    assert_eq!(layout_events(&mut events), 0);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_count
    );
    handler
        .active_control
        .lock()
        .await
        .by_pid
        .get_mut(&pid)
        .unwrap()
        .can_write = true;
    queue(&handler, "set-pane-placement -t fpc-rights:0.0 floating")
        .await
        .unwrap();
    assert!(super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(before.pane(0).unwrap().id()));
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_linked_explicit_targets_share_one_window_commit() {
    let (handler, name) = super::frs1::setup("fpc-linked").await;
    super::frs1::queue(&handler, "split-window -d -h -t fpc-linked:0.0")
        .await
        .unwrap();
    let linked = session_name("fpc-linked-alias");
    create_background_identity_session(&handler, linked.clone()).await;
    super::frs1::queue(
        &handler,
        "link-window -d -s fpc-linked:0 -t fpc-linked-alias:1",
    )
    .await
    .unwrap();
    let initial = super::frs1::window(&handler, &name).await;
    let anchor = initial
        .panes()
        .iter()
        .find(|pane| pane.index() != 0 && !initial.floating_stack().contains(pane.id()))
        .unwrap();
    let anchor_index = anchor.index();
    let resize_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();

    queue(&handler, "set-pane-placement -t fpc-linked:0.0 floating")
        .await
        .unwrap();
    queue(
        &handler,
        &format!(
            "set-pane-placement -t fpc-linked:0.0 -a fpc-linked-alias:1.{anchor_index} -h tiled"
        ),
    )
    .await
    .unwrap();
    let state = handler.state.lock().await;
    let owner = state.sessions.session(&name).unwrap().window_at(0).unwrap();
    let alias = state
        .sessions
        .session(&linked)
        .unwrap()
        .window_at(1)
        .unwrap();
    assert_eq!(owner, alias);
    assert_eq!(owner.pane(0).unwrap().geometry().rows(), owner.size().rows);
    assert_eq!(
        state.window_runtime_resize_count_for_test(),
        resize_count + 2,
        "each conversion resizes one canonical runtime family, not each link"
    );
    drop(state);
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_small_anchor_and_source_file_use_the_public_transaction() {
    let (handler, name) = super::frs1::setup("fpc-small").await;
    super::frs1::queue(&handler, "split-window -d -h -l 1 -t fpc-small:0.0")
        .await
        .unwrap();
    let initial = super::frs1::window(&handler, &name).await;
    let stack = initial.floating_stack();
    let source_id = stack.placements()[0].pane_id();
    let source_index = initial
        .panes()
        .iter()
        .find(|pane| pane.id() == source_id)
        .unwrap()
        .index();
    let narrow = initial
        .panes()
        .iter()
        .find(|pane| !stack.contains(pane.id()) && pane.geometry().cols() == 1)
        .expect("one-cell tiled anchor");
    let before = initial.clone();
    let runtime_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();
    assert!(queue(
        &handler,
        &format!(
            "set-pane-placement -t fpc-small:0.{source_index} -a fpc-small:0.{} -h tiled",
            narrow.index()
        ),
    )
    .await
    .is_err());
    assert_eq!(super::frs1::window(&handler, &name).await, before);
    assert_eq!(layout_events(&mut events), 0);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_count
    );
    super::frs1::cleanup(&handler).await;

    let (handler, name) = super::frs1::setup("fpc-source-file").await;
    let root = temp_root("fpc-placement");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("placement.conf"),
        "set-pane-placement -t fpc-source-file:0.0 floating\n",
    )
    .unwrap();
    let response = handler
        .handle(source_file_request(
            vec!["placement.conf".to_owned()],
            Some(root.clone()),
        ))
        .await;
    assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
    let source_id = super::frs1::window(&handler, &name)
        .await
        .pane(0)
        .unwrap()
        .id();
    assert!(super::frs1::window(&handler, &name)
        .await
        .floating_stack()
        .contains(source_id));
    fs::remove_dir_all(root).unwrap();
    super::frs1::cleanup(&handler).await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn fpc1_native_resize_calls_are_exact_coalesced_and_skip_empty_panes() {
    use native_resize::{arm_resize_recording, recorded_resize_ids};

    let (handler, name) = super::frs1::setup("fpc-native-scope").await;
    let initial = super::frs1::window(&handler, &name).await;
    let tile_id = initial.pane(0).unwrap().id();
    let float_id = initial.pane(1).unwrap().id();

    arm_resize_recording(&handler).await;
    queue(
        &handler,
        "set-pane-placement -t fpc-native-scope:0.1 -a fpc-native-scope:0.0 -h tiled",
    )
    .await
    .unwrap();
    assert_eq!(
        recorded_resize_ids(&handler).await,
        vec![tile_id.as_u32(), float_id.as_u32()]
    );

    arm_resize_recording(&handler).await;
    queue(
        &handler,
        "set-pane-placement -t fpc-native-scope:0.1 floating",
    )
    .await
    .unwrap();
    assert_eq!(
        recorded_resize_ids(&handler).await,
        vec![tile_id.as_u32()],
        "the source keeps its copied size while its tiled neighbor expands"
    );

    super::frs1::queue(&handler, "select-pane -t fpc-native-scope:0.1")
        .await
        .unwrap();
    super::frs1::queue(&handler, "resize-pane -t fpc-native-scope:0.1 -Z")
        .await
        .unwrap();
    arm_resize_recording(&handler).await;
    queue(
        &handler,
        "set-pane-placement -t fpc-native-scope:0.0 floating",
    )
    .await
    .unwrap();
    assert_eq!(
        recorded_resize_ids(&handler).await,
        vec![float_id.as_u32()],
        "zoom exit and conversion are coalesced into one final resize"
    );
    super::frs1::cleanup(&handler).await;

    let (handler, name) = super::frs1::setup("fpc-empty-scope").await;
    super::frs1::queue(&handler, "split-window -I -d -h -t fpc-empty-scope:0.0")
        .await
        .unwrap();
    let initial = super::frs1::window(&handler, &name).await;
    let stack = initial.floating_stack();
    let empty = initial
        .panes()
        .iter()
        .find(|pane| pane.index() != 0 && !stack.contains(pane.id()))
        .unwrap();
    let empty_id = empty.id();
    let empty_index = empty.index();
    queue(
        &handler,
        &format!("set-pane-placement -t fpc-empty-scope:0.{empty_index} floating"),
    )
    .await
    .unwrap();
    arm_resize_recording(&handler).await;
    queue(
        &handler,
        &format!(
            "set-pane-placement -t fpc-empty-scope:0.{empty_index} -a fpc-empty-scope:0.0 -v tiled"
        ),
    )
    .await
    .unwrap();
    let calls = recorded_resize_ids(&handler).await;
    assert_eq!(calls, vec![initial.pane(0).unwrap().id().as_u32()]);
    assert!(!calls.contains(&empty_id.as_u32()));
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fpc1_resize_failure_before_first_call_restores_model_without_event() {
    let (handler, name) = super::frs1::setup("fpc-before-resize").await;
    let before = super::frs1::window(&handler, &name).await;
    let runtime_count = {
        let mut state = handler.state.lock().await;
        let count = state.window_runtime_resize_count_for_test();
        state.fail_next_resize_for_test();
        count
    };
    let mut events = handler.subscribe_lifecycle_events();
    assert!(queue(
        &handler,
        "set-pane-placement -t fpc-before-resize:0.1 -a fpc-before-resize:0.0 -h tiled",
    )
    .await
    .is_err());
    assert_eq!(super::frs1::window(&handler, &name).await, before);
    assert_eq!(layout_events(&mut events), 0);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_count + 2,
        "failed resize plus successful compensation are separately accounted"
    );
    super::frs1::cleanup(&handler).await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn fpc1_partial_resize_uses_the_same_exact_scope_for_compensation() {
    use native_resize::ResizeCase;

    for (suffix, case, expected_calls) in [
        ("partial", ResizeCase::ScopedPartial, 4),
        ("residual", ResizeCase::ScopedResidual, 3),
    ] {
        let session = format!("fpc-{suffix}");
        let (handler, name) = super::frs1::setup(&session).await;
        let before = super::frs1::window(&handler, &name).await;
        let expected_ids = vec![
            before.pane(0).unwrap().id().as_u32(),
            before.pane(1).unwrap().id().as_u32(),
        ];
        let mut events = handler.subscribe_lifecycle_events();
        handler
            .state
            .lock()
            .await
            .arm_resize_outcome_test(std::path::Path::new("."), case);
        assert!(queue(
            &handler,
            &format!("set-pane-placement -t {session}:0.1 -a {session}:0.0 -h tiled"),
        )
        .await
        .is_err());
        assert_eq!(super::frs1::window(&handler, &name).await, before);
        assert_eq!(layout_events(&mut events), 0);
        let trace = handler.state.lock().await.finish_resize_outcome_test();
        let calls = trace["calls"].as_array().unwrap();
        assert_eq!(calls.len(), expected_calls);
        assert_eq!(
            calls
                .iter()
                .take(2)
                .map(|call| call["pane_id"].as_u64().unwrap() as u32)
                .collect::<Vec<_>>(),
            expected_ids
        );
        let compensation = &trace["typed"]["compensation"];
        let compensation_plan = compensation
            .get("completed")
            .or_else(|| compensation.get("failure").map(|failure| &failure["plan"]))
            .unwrap();
        let compensation_ids = compensation_plan["panes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pane| pane["id"].as_u64().unwrap() as u32)
            .collect::<Vec<_>>();
        assert_eq!(compensation_ids, expected_ids);
        super::frs1::cleanup(&handler).await;
    }
}
