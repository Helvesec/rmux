use super::*;
use crate::handler::scripting_support::install_queue_exact_target_capture_pause;
use crate::handler::QueuedLifecycleEvent;
use rmux_core::{
    FloatingPaneRect, LifecycleEvent, ResolvedPaneBorderStatus, ResolvedSceneContext,
    ResolvedZoomVisibility,
};
use rmux_proto::{
    KillPaneRequest, RespawnPaneRequest, RuntimeCommandExecutionRequest,
    RuntimeCommandExecutionResponse,
};
use std::time::Duration;

fn layout_events(events: &mut tokio::sync::broadcast::Receiver<QueuedLifecycleEvent>) -> usize {
    std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event.event, LifecycleEvent::WindowLayoutChanged { .. }))
        .count()
}

pub(super) fn stack_order(window: &rmux_core::Window) -> Vec<rmux_core::PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

fn pane_geometries(window: &rmux_core::Window) -> Vec<rmux_core::PaneGeometry> {
    window
        .panes()
        .iter()
        .map(rmux_core::Pane::geometry)
        .collect()
}

async fn offer(
    handler: &RequestHandler,
    args: &[&str],
    require_position: bool,
) -> RuntimeCommandExecutionResponse {
    let mut request = RuntimeCommandExecutionRequest::new(
        args.iter().map(|argument| (*argument).to_owned()).collect(),
        None,
    );
    if require_position {
        request = request.requiring_resize_pane_position();
    }
    execution_result(
        handler
            .handle(Request::RuntimeCommandExecution(Box::new(request)))
            .await,
    )
}

#[tokio::test]
async fn fmp1_public_translation_moves_only_the_canonical_float_placement() {
    let (handler, name) = super::frs1::setup("fmp-public").await;
    let before = super::frs1::window(&handler, &name).await;
    let pane_id = before.pane(1).unwrap().id();
    let pane_size = {
        let geometry = before.pane(1).unwrap().geometry();
        (geometry.cols(), geometry.rows())
    };
    let tiled_geometry = before.pane(0).unwrap().geometry();
    let active_pane = before.active_pane_index();
    let initial_stack_order = stack_order(&before);
    let resize_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();

    super::frs1::queue(&handler, "resizep -t fmp-public:0.1 -X 12 -Y -2")
        .await
        .unwrap();

    let moved = super::frs1::window(&handler, &name).await;
    assert_eq!(
        super::frs1::floating_rect(&moved, 1),
        FloatingPaneRect::new(12, -2, 30, 8).unwrap()
    );
    assert_eq!(moved.pane(1).unwrap().id(), pane_id);
    assert_eq!(
        {
            let geometry = moved.pane(1).unwrap().geometry();
            (geometry.cols(), geometry.rows())
        },
        pane_size
    );
    assert_eq!(moved.pane(0).unwrap().geometry(), tiled_geometry);
    assert_eq!(moved.active_pane_index(), active_pane);
    assert_eq!(stack_order(&moved), initial_stack_order);
    assert_eq!(layout_events(&mut events), 1);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_count
    );

    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fmp1_axes_noop_and_extreme_clipping_share_the_canonical_scene() {
    let (handler, name) = super::frs1::setup("fmp-axes").await;
    let resize_count = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();

    super::frs1::queue(&handler, "resize-pane -t fmp-axes:0.1 -X -5")
        .await
        .unwrap();
    assert_eq!(
        super::frs1::floating_rect(&super::frs1::window(&handler, &name).await, 1),
        FloatingPaneRect::new(-5, 4, 30, 8).unwrap()
    );
    super::frs1::queue(&handler, "resizep -t fmp-axes:0.1 -Y 24")
        .await
        .unwrap();
    super::frs1::queue(&handler, "resizep -t fmp-axes:0.1 -X -5 -Y 24")
        .await
        .unwrap();
    assert_eq!(layout_events(&mut events), 2, "no-op must not notify");

    super::frs1::queue(
        &handler,
        "resize-pane -t fmp-axes:0.1 -X 2147483647 -Y -2147483648",
    )
    .await
    .unwrap();
    let moved = super::frs1::window(&handler, &name).await;
    let rect = FloatingPaneRect::new(i32::MAX, i32::MIN, 30, 8).unwrap();
    assert_eq!(super::frs1::floating_rect(&moved, 1), rect);
    let scene = moved.resolved_scene(ResolvedSceneContext::new(
        moved.size(),
        ResolvedPaneBorderStatus::Top,
        ResolvedZoomVisibility::Rendered,
    ));
    let floating = scene.pane_by_index(1).unwrap();
    assert_eq!(floating.content().x(), i32::MAX);
    assert_eq!(floating.content().y(), i32::MIN);
    assert!(floating.content_clip().is_empty());
    assert_eq!(layout_events(&mut events), 1);
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        resize_count
    );
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fmp1_zoom_moves_only_return_geometry_for_target_and_hidden_float() {
    let (handler, name) = super::frs1::setup("fmp-zoom").await;
    super::frs1::queue(&handler, "select-pane -t fmp-zoom:0.1")
        .await
        .unwrap();
    super::frs1::queue(&handler, "resize-pane -t fmp-zoom:0.1 -Z")
        .await
        .unwrap();
    let zoomed = super::frs1::window(&handler, &name).await;
    assert!(zoomed.is_zoomed());
    let effective_before = pane_geometries(&zoomed);
    let stack_before = stack_order(&zoomed);
    let active_before = zoomed.active_pane_index();
    let runtime_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    let mut events = handler.subscribe_lifecycle_events();

    super::frs1::queue(&handler, "resizep -t fmp-zoom:0.1 -X 11 -Y -3")
        .await
        .unwrap();
    let moved = super::frs1::window(&handler, &name).await;
    assert!(moved.is_zoomed());
    assert_eq!(pane_geometries(&moved), effective_before);
    assert_eq!(stack_order(&moved), stack_before);
    assert_eq!(moved.active_pane_index(), active_before);
    assert_eq!(
        super::frs1::floating_rect(&moved, 1),
        FloatingPaneRect::new(11, -3, 30, 8).unwrap()
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before
    );
    super::frs1::queue(&handler, "resizep -t fmp-zoom:0.1 -X 11 -Y -3")
        .await
        .unwrap();
    assert_eq!(layout_events(&mut events), 1);

    super::frs1::queue(&handler, "resize-pane -t fmp-zoom:0.1 -Z")
        .await
        .unwrap();
    let restored = super::frs1::window(&handler, &name).await;
    assert!(!restored.is_zoomed());
    assert_eq!(
        super::frs1::floating_rect(&restored, 1),
        FloatingPaneRect::new(11, -3, 30, 8).unwrap()
    );

    super::frs1::queue(&handler, "new-pane -d -t fmp-zoom:0.0 -x18 -y5 -X2 -Y3")
        .await
        .unwrap();
    super::frs1::queue(&handler, "select-pane -t fmp-zoom:0.0")
        .await
        .unwrap();
    super::frs1::queue(&handler, "resize-pane -t fmp-zoom:0.0 -Z")
        .await
        .unwrap();
    let other_zoomed = super::frs1::window(&handler, &name).await;
    let effective_before = pane_geometries(&other_zoomed);
    let runtime_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    super::frs1::queue(&handler, "resize-pane -t fmp-zoom:0.1 -X -9")
        .await
        .unwrap();
    let hidden_moved = super::frs1::window(&handler, &name).await;
    assert!(hidden_moved.is_zoomed());
    assert_eq!(pane_geometries(&hidden_moved), effective_before);
    assert_eq!(
        super::frs1::floating_rect(&hidden_moved, 1),
        FloatingPaneRect::new(-9, -3, 30, 8).unwrap()
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before
    );
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fmp1_rejects_mixed_tiled_and_stale_targets_without_effects() {
    let (handler, name) = super::frs1::setup_quiet_window("fmp-guards").await;
    for suffix in [
        "-X 2 -x 20",
        "-Y 2 -R",
        "-X 2 -M",
        "-X 2 -T",
        "-X 2 -Z",
        "-X 2 -X 3",
        "-Y 10%",
        "-X 2147483648",
        "-X 2 3",
    ] {
        let before = super::frs1::window(&handler, &name).await;
        let command = format!("resize-pane -t fmp-guards:0.1 {suffix}");
        assert!(
            super::frs1::queue(&handler, &command).await.is_err(),
            "{suffix}"
        );
        assert_eq!(
            super::frs1::window(&handler, &name).await,
            before,
            "{suffix}"
        );
    }
    let before = super::frs1::window(&handler, &name).await;
    assert!(
        super::frs1::queue(&handler, "resize-pane -t fmp-guards:0.0 -X 4")
            .await
            .is_err()
    );
    assert_eq!(super::frs1::window(&handler, &name).await, before);

    let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
    let moving_handler = handler.clone();
    let moving = tokio::spawn(async move {
        super::frs1::queue(&moving_handler, "resize-pane -t fmp-guards:0.1 -X 17").await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("move reaches post-capture pause");
    assert!(matches!(
        handler
            .handle(Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, 1),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    super::frs1::queue(&handler, "new-pane -d -t fmp-guards:0.0 -x18 -y5 -X2 -Y3")
        .await
        .unwrap();
    let replacement = super::frs1::window(&handler, &name).await;
    let replacement_id = replacement.pane(1).unwrap().id();
    pause.release.notify_one();
    assert!(moving.await.unwrap().is_err());
    let after = super::frs1::window(&handler, &name).await;
    assert_eq!(after.pane(1).unwrap().id(), replacement_id);
    assert_eq!(
        super::frs1::floating_rect(&after, 1),
        FloatingPaneRect::new(2, 3, 18, 5).unwrap()
    );

    let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
    let moving_handler = handler.clone();
    let moving = tokio::spawn(async move {
        super::frs1::queue(&moving_handler, "resize-pane -t fmp-guards:0.1 -Y 9").await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("move reaches generation pause");
    assert!(matches!(
        handler
            .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, 1),
                kill: true,
                start_directory: None,
                environment: None,
                command: Some(vec![crate::test_shell::stdin_discard_command()]),
                process_command: None,
            })))
            .await,
        Response::RespawnPane(_)
    ));
    pause.release.notify_one();
    assert!(moving.await.unwrap().is_err());
    assert_eq!(
        super::frs1::floating_rect(&super::frs1::window(&handler, &name).await, 1),
        FloatingPaneRect::new(2, 3, 18, 5).unwrap()
    );

    let requester_pid = std::process::id();
    let _control =
        super::super::register_control_for_session(&handler, requester_pid, name.clone()).await;
    handler
        .active_control
        .lock()
        .await
        .by_pid
        .get_mut(&requester_pid)
        .unwrap()
        .can_write = false;
    assert!(
        super::frs1::queue(&handler, "resize-pane -t fmp-guards:0.1 -Y 8")
            .await
            .is_err()
    );
    assert_eq!(super::frs1::window(&handler, &name).await, after);

    handler
        .active_control
        .lock()
        .await
        .by_pid
        .get_mut(&requester_pid)
        .unwrap()
        .can_write = true;
    let pause = install_queue_exact_target_capture_pause(&handler, "resize-pane");
    let moving_handler = handler.clone();
    let moving = tokio::spawn(async move {
        super::frs1::queue(&moving_handler, "resize-pane -t fmp-guards:0.1 -X 5").await
    });
    tokio::time::timeout(Duration::from_secs(5), pause.reached.notified())
        .await
        .expect("move reaches disappearance pause");
    assert!(matches!(
        handler
            .handle(Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(name.clone(), 0, 1),
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));
    pause.release.notify_one();
    assert!(moving.await.unwrap().is_err());
    assert!(super::frs1::window(&handler, &name).await.pane(1).is_none());
    super::frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn fmp1_linked_id_alias_offer_and_batch_admission_preserve_neighbors() {
    let (handler, name) = super::frs1::setup("fmp-offer").await;
    super::frs1::queue(&handler, "select-pane -t fmp-offer:0.1")
        .await
        .unwrap();
    let before = super::frs1::window(&handler, &name).await;
    let pane_id = before.pane(1).unwrap().id();
    let stack = stack_order(&before);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<crate::pane_io::AttachControl>();
    handler
        .register_attach(std::process::id(), name.clone(), tx)
        .await;
    let linked = session_name("fmp-offer-linked");
    create_background_identity_session(&handler, linked.clone()).await;
    super::frs1::queue(
        &handler,
        "link-window -d -s fmp-offer:0 -t fmp-offer-linked:1",
    )
    .await
    .unwrap();
    super::frs1::queue(&handler, &format!("resizep -t {pane_id} -X 14"))
        .await
        .unwrap();
    super::frs1::queue(&handler, "resize-pane -t fmp-offer-linked:1.1 -Y 7")
        .await
        .unwrap();
    let owner = super::frs1::window(&handler, &name).await;
    assert_eq!(stack_order(&owner), stack);
    assert_eq!(owner.active_pane_index(), 1);
    assert_eq!(
        super::frs1::floating_rect(&owner, 1),
        FloatingPaneRect::new(14, 7, 30, 8).unwrap()
    );
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
    assert_eq!(alias.floating_stack(), owner.floating_stack());

    assert!(matches!(
        offer(
            &handler,
            &["resizep", "-t", "fmp-offer:0.1", "-X", "9"],
            false,
        )
        .await,
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    set_alias(&handler, "slide=resize-pane -tfmp-offer:0.1 -X9 -Y6").await;
    assert!(matches!(
        offer(&handler, &["slide"], true).await,
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
    let batch = [
        "set-environment",
        "-g",
        "FMP_BAD",
        "yes",
        ";",
        "resize-pane",
        "-t",
        "fmp-offer:0.1",
        "-X",
        "3",
    ];
    assert!(matches!(
        offer(&handler, &batch, true).await,
        RuntimeCommandExecutionResponse::Unsupported(_)
    ));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("FMP_BAD"),
        None
    );

    let runtime_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();
    super::frs1::queue(&handler, "resize-pane -t fmp-offer:0.1 -x 29")
        .await
        .unwrap();
    assert_eq!(
        super::frs1::floating_rect(&super::frs1::window(&handler, &name).await, 1).cols(),
        29
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test(),
        runtime_before + 1,
        "legacy FRS1 resize must keep its terminal resize"
    );
    super::frs1::cleanup(&handler).await;
}
