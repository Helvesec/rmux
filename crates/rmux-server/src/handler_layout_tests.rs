use super::RequestHandler;
use rmux_core::{FloatingPaneRect, PaneGeometry, PaneId};
use rmux_proto::{
    ExportWindowSnapshotRequest, LayoutName, NewSessionRequest, NextLayoutRequest, PaneTarget,
    PreviousLayoutRequest, Request, ResizePaneAdjustment, ResizePaneRequest, Response,
    RestoreWindowSnapshotRequest, RestoreWindowSnapshotResponse, RmuxWindowSnapshot,
    SelectCustomLayoutRequest, SelectLayoutRequest, SelectLayoutTarget, SelectOldLayoutRequest,
    SessionId, SessionName, SplitWindowRequest, SplitWindowTarget, SpreadLayoutRequest,
    TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn layout_string(body: &str) -> String {
    let mut checksum = 0_u16;
    for byte in body.bytes() {
        checksum = (checksum >> 1) + ((checksum & 1) << 15);
        checksum = checksum.wrapping_add(u16::from(byte));
    }
    format!("{checksum:04x},{body}")
}

fn floating_rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

async fn create_session(handler: &RequestHandler, session_name: &SessionName, size: TerminalSize) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(size),

            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
}

async fn split_pane_zero(handler: &RequestHandler, session_name: &SessionName, expected_pane: u32) {
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(session_name.clone(), 0)),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert_eq!(
        split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session_name.clone(), expected_pane),
        })
    );
}

async fn setup_snapshot_window(
    handler: &RequestHandler,
    session_name: &SessionName,
) -> WindowTarget {
    create_session(
        handler,
        session_name,
        TerminalSize {
            cols: 120,
            rows: 40,
        },
    )
    .await;
    split_pane_zero(handler, session_name, 1).await;
    split_pane_zero(handler, session_name, 1).await;

    let target = WindowTarget::with_window(session_name.clone(), 0);
    {
        let mut state = handler.state.lock().await;
        let mut window = state
            .sessions
            .window_layout_context_mut(&target)
            .expect("window layout context");
        window.select_layout_with_main_pane_size(LayoutName::EvenVertical, None, None);
        window.float_pane(1, floating_rect(-3, 4, 30, 10)).unwrap();
        window.float_pane(2, floating_rect(45, -2, 22, 7)).unwrap();
        window.select_floating_pane(1).unwrap();
        window.raise_floating_pane(2).unwrap();
        window.set_floating_pane_visible(1, false).unwrap();
    }
    target
}

async fn export_window_snapshot(
    handler: &RequestHandler,
    target: WindowTarget,
) -> RmuxWindowSnapshot {
    let response = handler
        .handle(Request::ExportWindowSnapshot(ExportWindowSnapshotRequest {
            target,
        }))
        .await;
    let Response::ExportWindowSnapshot(response) = response else {
        panic!("expected export-window-snapshot response, got {response:?}");
    };
    let expected_stdout = format!(
        "{}\n",
        serde_json::to_string(&response.snapshot).expect("snapshot serializes")
    );
    assert_eq!(response.output.stdout(), expected_stdout.as_bytes());
    response.snapshot.clone()
}

fn last_pane_id(window: &rmux_core::Window) -> Option<PaneId> {
    window
        .last_pane_index()
        .and_then(|pane_index| window.pane_id(pane_index))
}

fn floating_order(window: &rmux_core::Window) -> Vec<PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

#[tokio::test]
async fn rmux_window_snapshot_restores_layout_float_stack_and_selection_without_position_resize() {
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot");
    let target = setup_snapshot_window(&handler, &alpha).await;
    let snapshot = export_window_snapshot(&handler, target.clone()).await;
    let resize_count_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();

    {
        let mut state = handler.state.lock().await;
        let mut window = state
            .sessions
            .window_layout_context_mut(&target)
            .expect("window layout context");
        window.select_floating_pane(2).unwrap();
        window.raise_floating_pane(1).unwrap();
        window.set_floating_pane_visible(1, true).unwrap();
        window.move_floating_pane(1, 8, 6).unwrap();
        window.move_floating_pane(2, 50, 3).unwrap();
    }

    let response = handler
        .handle(Request::RestoreWindowSnapshot(Box::new(
            RestoreWindowSnapshotRequest {
                target: target.clone(),
                snapshot: snapshot.clone(),
            },
        )))
        .await;
    assert_eq!(
        response,
        Response::RestoreWindowSnapshot(RestoreWindowSnapshotResponse {
            target: target.clone(),
            window_id: snapshot.window_id,
        })
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.window_runtime_resize_count_for_test(),
            resize_count_before,
            "position/order/visibility/selection-only restores must not resize runtime PTYs"
        );
        let window = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window();
        assert_eq!(window.active_pane().unwrap().id(), snapshot.active_pane);
        assert_eq!(last_pane_id(window), snapshot.last_pane);
        assert_eq!(
            floating_order(window),
            snapshot
                .floating
                .iter()
                .map(|placement| placement.pane_id)
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(export_window_snapshot(&handler, target).await, snapshot);
}

#[tokio::test]
async fn rmux_window_snapshot_restore_resizes_runtime_only_for_size_deltas() {
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot-resize");
    let target = setup_snapshot_window(&handler, &alpha).await;
    let snapshot = export_window_snapshot(&handler, target.clone()).await;

    {
        let mut state = handler.state.lock().await;
        let mut window = state
            .sessions
            .window_layout_context_mut(&target)
            .expect("window layout context");
        window.resize_floating_pane(1, 35, 10).unwrap();
        window.resize_floating_pane(2, 22, 9).unwrap();
    }
    let resize_count_before = handler
        .state
        .lock()
        .await
        .window_runtime_resize_count_for_test();

    let response = handler
        .handle(Request::RestoreWindowSnapshot(Box::new(
            RestoreWindowSnapshotRequest {
                target: target.clone(),
                snapshot: snapshot.clone(),
            },
        )))
        .await;
    assert!(
        matches!(response, Response::RestoreWindowSnapshot(_)),
        "{response:?}"
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.window_runtime_resize_count_for_test(),
            resize_count_before + 1,
            "size deltas should use one scoped runtime resize transaction"
        );
    }
    assert_eq!(export_window_snapshot(&handler, target).await, snapshot);
}

#[tokio::test]
async fn rmux_window_snapshot_restore_rejects_session_and_target_mismatch_without_effect() {
    let handler = RequestHandler::new();
    let alpha = session_name("snapshot-guard");
    let target = setup_snapshot_window(&handler, &alpha).await;
    let exported = export_window_snapshot(&handler, target.clone()).await;

    for snapshot in [
        {
            let mut snapshot = exported.clone();
            snapshot.target = WindowTarget::with_window(session_name("other"), 0);
            snapshot
        },
        {
            let mut snapshot = exported.clone();
            snapshot.session_id = SessionId::new(snapshot.session_id.as_u32() + 1);
            snapshot
        },
        {
            let mut snapshot = exported.clone();
            snapshot.runtimes[0].incarnation[0] ^= 1;
            snapshot
        },
        {
            let mut snapshot = exported.clone();
            snapshot.runtimes.pop();
            snapshot
        },
        {
            let mut snapshot = exported.clone();
            snapshot.runtimes.push(snapshot.runtimes[0].clone());
            snapshot
        },
    ] {
        let resize_count_before = handler
            .state
            .lock()
            .await
            .window_runtime_resize_count_for_test();
        let response = handler
            .handle(Request::RestoreWindowSnapshot(Box::new(
                RestoreWindowSnapshotRequest {
                    target: target.clone(),
                    snapshot,
                },
            )))
            .await;
        assert!(matches!(response, Response::Error(_)), "{response:?}");
        assert_eq!(
            export_window_snapshot(&handler, target.clone()).await,
            exported
        );
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .window_runtime_resize_count_for_test(),
            resize_count_before
        );
    }
}

#[tokio::test]
async fn select_layout_even_layouts_apply_tmux_geometry_through_the_handler() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    create_session(
        &handler,
        &alpha,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
    )
    .await;

    for expected_pane in [1, 1] {
        split_pane_zero(&handler, &alpha, expected_pane).await;
    }
    let active_before = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .active_pane_index()
    };

    let even_horizontal = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
            layout: LayoutName::EvenHorizontal,
        }))
        .await;
    assert_eq!(
        even_horizontal,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window();
        assert_eq!(window.layout(), LayoutName::EvenHorizontal);
        assert_eq!(
            window.pane(0).expect("pane 0 exists").geometry(),
            PaneGeometry::new(0, 0, 33, 40)
        );
        assert_eq!(
            window.pane(1).expect("pane 1 exists").geometry(),
            PaneGeometry::new(34, 0, 33, 40)
        );
        assert_eq!(
            window.pane(2).expect("pane 2 exists").geometry(),
            PaneGeometry::new(68, 0, 32, 40)
        );
        assert_eq!(window.active_pane_index(), active_before);
    }

    let even_vertical = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
            layout: LayoutName::EvenVertical,
        }))
        .await;
    assert_eq!(
        even_vertical,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenVertical,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    let window = session.window();
    assert_eq!(window.layout(), LayoutName::EvenVertical);
    assert_eq!(
        window.pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 100, 13)
    );
    assert_eq!(
        window.pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(0, 14, 100, 13)
    );
    assert_eq!(
        window.pane(2).expect("pane 2 exists").geometry(),
        PaneGeometry::new(0, 28, 100, 12)
    );
    assert_eq!(window.active_pane_index(), active_before);
    drop(state);

    for (layout, expected) in [
        (
            LayoutName::MainHorizontal,
            [
                PaneGeometry::new(0, 0, 100, 24),
                PaneGeometry::new(0, 25, 50, 15),
                PaneGeometry::new(51, 25, 49, 15),
            ],
        ),
        (
            LayoutName::MainVertical,
            [
                PaneGeometry::new(0, 0, 80, 40),
                PaneGeometry::new(81, 0, 19, 20),
                PaneGeometry::new(81, 21, 19, 19),
            ],
        ),
    ] {
        let response = handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
                layout,
            }))
            .await;
        assert_eq!(
            response,
            Response::SelectLayout(rmux_proto::SelectLayoutResponse { layout })
        );

        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window();
        assert_eq!(
            window
                .panes()
                .iter()
                .map(|pane| pane.geometry())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(window.active_pane_index(), active_before);
    }
}

#[tokio::test]
async fn next_layout_uses_tmux_cycle_order_and_wraps() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha, TerminalSize { cols: 80, rows: 24 }).await;

    let selected = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
            layout: LayoutName::EvenHorizontal,
        }))
        .await;
    assert_eq!(
        selected,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    for (offset, expected) in [
        LayoutName::EvenVertical,
        LayoutName::MainHorizontal,
        LayoutName::MainHorizontalMirrored,
        LayoutName::MainVertical,
        LayoutName::MainVerticalMirrored,
        LayoutName::Tiled,
        LayoutName::EvenHorizontal,
        LayoutName::EvenVertical,
    ]
    .into_iter()
    .enumerate()
    {
        let response = handler
            .handle(Request::NextLayout(NextLayoutRequest {
                target: WindowTarget::new(alpha.clone()),
            }))
            .await;
        assert_eq!(
            response,
            Response::NextLayout(rmux_proto::NextLayoutResponse { layout: expected }),
            "unexpected next-layout response at cycle offset {offset}"
        );
    }
}

#[tokio::test]
async fn previous_layout_from_even_horizontal_wraps_to_tiled() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha, TerminalSize { cols: 80, rows: 24 }).await;

    let selected = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
            layout: LayoutName::EvenHorizontal,
        }))
        .await;
    assert_eq!(
        selected,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    let previous = handler
        .handle(Request::PreviousLayout(PreviousLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;
    assert_eq!(
        previous,
        Response::PreviousLayout(rmux_proto::PreviousLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );
}

#[tokio::test]
async fn next_and_previous_layout_auto_unzoom_zoomed_windows() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, &alpha, TerminalSize { cols: 80, rows: 24 }).await;
    split_pane_zero(&handler, &alpha, 1).await;

    let selected = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
            layout: LayoutName::Tiled,
        }))
        .await;
    assert_eq!(
        selected,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );

    let zoomed = handler
        .handle(Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 1),
            adjustment: ResizePaneAdjustment::Zoom,
        }))
        .await;
    assert!(matches!(zoomed, Response::ResizePane(_)));

    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert!(session.window().is_zoomed());
    }

    let next = handler
        .handle(Request::NextLayout(NextLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;
    assert_eq!(
        next,
        Response::NextLayout(rmux_proto::NextLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert!(!session.window().is_zoomed());
        assert_eq!(session.window().layout(), LayoutName::EvenHorizontal);
    }

    let zoomed = handler
        .handle(Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 1),
            adjustment: ResizePaneAdjustment::Zoom,
        }))
        .await;
    assert!(matches!(zoomed, Response::ResizePane(_)));

    let previous = handler
        .handle(Request::PreviousLayout(PreviousLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;
    assert_eq!(
        previous,
        Response::PreviousLayout(rmux_proto::PreviousLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert!(!session.window().is_zoomed());
    assert_eq!(session.window().layout(), LayoutName::Tiled);
}

#[tokio::test]
async fn custom_old_and_spread_layouts_share_the_addressed_window_transaction() {
    let handler = RequestHandler::new();
    let alpha = session_name("layout-family");
    create_session(
        &handler,
        &alpha,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
    )
    .await;
    split_pane_zero(&handler, &alpha, 1).await;
    split_pane_zero(&handler, &alpha, 1).await;

    let target = WindowTarget::new(alpha.clone());
    let selected = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(target.clone()),
            layout: LayoutName::EvenHorizontal,
        }))
        .await;
    assert_eq!(
        selected,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    let state_before_invalid = {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window();
        (window.layout_dump(), window.old_layout().map(str::to_owned))
    };
    let invalid = handler
        .handle(Request::SelectCustomLayout(SelectCustomLayoutRequest {
            target: SelectLayoutTarget::Window(target.clone()),
            layout: "not-a-layout".to_owned(),
        }))
        .await;
    assert!(matches!(invalid, Response::Error(_)), "{invalid:?}");
    {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window();
        assert_eq!(
            (window.layout_dump(), window.old_layout().map(str::to_owned)),
            state_before_invalid,
            "custom validation failure must roll back the preceding old-layout save"
        );
    }

    let named_layout = state_before_invalid.0;
    let custom_layout =
        layout_string("100x40,0,0{60x40,0,0,0,39x40,61,0[39x5,61,0,2,39x34,61,6,1]}");
    let selected = handler
        .handle(Request::SelectCustomLayout(SelectCustomLayoutRequest {
            target: SelectLayoutTarget::Window(target.clone()),
            layout: custom_layout.clone(),
        }))
        .await;
    assert_eq!(
        selected,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    let old = handler
        .handle(Request::SelectOldLayout(SelectOldLayoutRequest {
            target: SelectLayoutTarget::Window(target.clone()),
        }))
        .await;
    assert_eq!(
        old,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );
    {
        let state = handler.state.lock().await;
        let window = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window();
        assert_eq!(window.layout_dump(), named_layout);
        assert_eq!(window.old_layout(), Some(custom_layout.as_str()));
    }

    let selected = handler
        .handle(Request::SelectCustomLayout(SelectCustomLayoutRequest {
            target: SelectLayoutTarget::Window(target.clone()),
            layout: custom_layout.clone(),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectLayout(_)),
        "{selected:?}"
    );
    let spread = handler
        .handle(Request::SpreadLayout(SpreadLayoutRequest {
            target: SelectLayoutTarget::Window(target),
        }))
        .await;
    assert_eq!(
        spread,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&alpha)
        .expect("session exists")
        .window();
    assert_eq!(window.old_layout(), Some(custom_layout.as_str()));
    assert_ne!(window.layout_dump(), custom_layout);
}

#[tokio::test]
async fn resize_pane_preserves_custom_layout_trees_through_the_handler() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(
        &handler,
        &alpha,
        TerminalSize {
            cols: 100,
            rows: 40,
        },
    )
    .await;
    split_pane_zero(&handler, &alpha, 1).await;
    split_pane_zero(&handler, &alpha, 1).await;

    let custom_layout =
        layout_string("100x40,0,0{60x40,0,0,0,39x40,61,0[39x19,61,0,2,39x20,61,20,1]}");
    let selected = handler
        .handle(Request::SelectCustomLayout(SelectCustomLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
            layout: custom_layout,
        }))
        .await;
    assert!(matches!(selected, Response::SelectLayout(_)));

    let resized = handler
        .handle(Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }))
        .await;
    assert!(matches!(resized, Response::ResizePane(_)));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window().layout_dump(),
        layout_string("100x40,0,0{34x40,0,0,0,65x40,35,0[65x19,35,0,2,65x20,35,20,1]}")
    );
}

/// Finite native witness: one handler, three explicit quiet PTYs; run only in native lot.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn l1_033_native_snapshot_v2_delta_scope_and_rollback() {
    use rmux_proto::{
        ExportWindowSnapshotV2Request, RestoreWindowSnapshotV2Request, RmuxWindowSnapshotV2,
    };
    async fn snapshot(handler: &RequestHandler, target: &WindowTarget) -> RmuxWindowSnapshotV2 {
        match handler
            .handle(Request::ExportWindowSnapshotV2(
                ExportWindowSnapshotV2Request {
                    target: target.clone(),
                },
            ))
            .await
        {
            Response::ExportWindowSnapshotV2(r) => r.snapshot,
            r => panic!("V2 export: {r:?}"),
        }
    }
    async fn counts(handler: &RequestHandler, name: &SessionName) -> Vec<u64> {
        let state = handler.state.lock().await;
        (0..3)
            .map(|i| state.pane_native_resize_calls_for_test(&PaneTarget::new(name.clone(), i)))
            .collect()
    }
    async fn record(handler: &RequestHandler, name: &SessionName) {
        let state = handler.state.lock().await;
        for i in 0..state.sessions.session(name).unwrap().window().panes().len() {
            let pid = state.pane_pid_in_window(name, 0, i as u32).unwrap();
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
            let birth = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .nth(19)
                .unwrap();
            println!("owned pane pid={pid} birth={birth}");
        }
    }
    let handler = RequestHandler::new();
    let name = session_name("l1-033-native");
    let target = WindowTarget::with_window(name.clone(), 0);
    let command = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "while IFS= read -r line; do :; done".to_owned(),
    ];
    let request = serde_json::from_value(serde_json::json!({"session_name":name,"detached":true,"size":{"cols":80,"rows":12},"command":command})).unwrap();
    let response = handler
        .handle(Request::NewSessionExt(Box::new(request)))
        .await;
    crate::session_fixture::assert_new_session(&response);
    record(&handler, &name).await;
    for _ in 0..2 {
        let request = rmux_proto::SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(name.clone(), 0)),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
            command: Some(command.clone()),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        };
        let response = handler
            .handle(Request::SplitWindowExt(Box::new(request)))
            .await;
        assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
        record(&handler, &name).await;
    }
    {
        let mut state = handler.state.lock().await;
        let mut window = state.sessions.window_layout_context_mut(&target).unwrap();
        window.select_layout_with_main_pane_size(LayoutName::EvenVertical, None, None);
        window.float_pane(1, floating_rect(-3, 1, 12, 4)).unwrap();
        window.float_pane(2, floating_rect(4, 2, 12, 4)).unwrap();
    }
    let original = snapshot(&handler, &target).await;
    let before = counts(&handler, &name).await;
    let resize = || {
        Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::new(name.clone(), 1),
            adjustment: ResizePaneAdjustment::AbsoluteHeight { rows: 3 },
        })
    };
    let response = handler.handle(resize()).await;
    assert!(matches!(response, Response::ResizePane(_)), "{response:?}");
    let after = counts(&handler, &name).await;
    assert_eq!(after, vec![before[0], before[1] + 1, before[2] + 1]);
    let changed = snapshot(&handler, &target).await;
    assert_ne!(changed, original);
    assert!(matches!(
        handler.handle(resize()).await,
        Response::ResizePane(_)
    ));
    assert_eq!(counts(&handler, &name).await, after);
    for i in 0..2 {
        let mut invalid = changed.clone();
        if i == 0 {
            invalid.runtimes[0].incarnation[0] ^= 1;
        } else {
            invalid.resize_topology.floating_attachment_order.clear();
        }
        let response = handler
            .handle(Request::RestoreWindowSnapshotV2(Box::new(
                RestoreWindowSnapshotV2Request {
                    target: target.clone(),
                    snapshot: invalid,
                },
            )))
            .await;
        assert!(matches!(response, Response::Error(_)), "{response:?}");
        assert_eq!(snapshot(&handler, &target).await, changed);
        assert_eq!(counts(&handler, &name).await, after);
    }
    handler.state.lock().await.fail_next_resize_for_test();
    let response = handler
        .handle(Request::RestoreWindowSnapshotV2(Box::new(
            RestoreWindowSnapshotV2Request {
                target: target.clone(),
                snapshot: original.clone(),
            },
        )))
        .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");
    assert_eq!(
        snapshot(&handler, &target).await,
        changed,
        "runtime failure rolls back geometry and topology"
    );
    let before_restore = counts(&handler, &name).await;
    let response = handler
        .handle(Request::RestoreWindowSnapshotV2(Box::new(
            RestoreWindowSnapshotV2Request {
                target: target.clone(),
                snapshot: original.clone(),
            },
        )))
        .await;
    assert!(
        matches!(response, Response::RestoreWindowSnapshotV2(_)),
        "{response:?}"
    );
    assert_eq!(snapshot(&handler, &target).await, original);
    assert_eq!(
        counts(&handler, &name).await,
        vec![
            before_restore[0],
            before_restore[1] + 1,
            before_restore[2] + 1
        ]
    );
    let response = handler.handle(resize()).await;
    assert!(matches!(response, Response::ResizePane(_)), "{response:?}");
    assert_eq!(
        snapshot(&handler, &target).await,
        changed,
        "next resize behavior survived V2 restore"
    );
    let response = handler
        .handle(Request::KillSession(rmux_proto::KillSessionRequest {
            target: name,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    println!("native V2 exact delta, no-op, stale runtime, rollback, next-resize PASS; cleanup={response:?}");
}
