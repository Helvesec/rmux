use super::*;

#[tokio::test]
async fn copy_mode_begin_selection_with_mouse_context_preserves_the_original_anchor() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();
    let target = PaneTarget::new(alpha.clone(), 0);

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 5 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let entered = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(entered, Response::CopyMode(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let (window_id, pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(0).expect("pane exists");
        (window.id(), pane.id())
    };

    let mut original_anchor = None;
    for (x, expected_key_count) in [(1, 1usize), (4, 1usize)] {
        let mut active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&requester_pid)
            .expect("attached client exists");
        active.mouse.current_event = Some(AttachedMouseEvent {
            raw: MouseForwardEvent {
                b: 32,
                lb: 0,
                x,
                y: 1,
                lx: x,
                ly: 1,
                sgr_b: 32,
                sgr_type: 'M',
                ignore: false,
            },
            session_id: 0,
            window_id: Some(window_id.as_u32()),
            pane_id: Some(pane_id),
            pane_target: Some(target.clone()),
            location: MouseLocation::Pane,
            status_at: None,
            status_lines: 0,
            ignore: false,
        });
        drop(active_attach);

        let response = handler
            .handle(Request::SendKeysExt(SendKeysExtRequest {
                target: Some(target.clone()),
                keys: vec!["begin-selection".to_owned()],
                expand_formats: false,
                hex: false,
                literal: false,
                dispatch_key_table: false,
                copy_mode_command: true,
                forward_mouse_event: false,
                reset_terminal: false,
                repeat_count: None,
            }))
            .await;
        assert_eq!(
            response,
            Response::SendKeys(SendKeysResponse {
                key_count: expected_key_count,
            })
        );

        let summary = {
            let state = handler.state.lock().await;
            state
                .pane_copy_mode_summary(&alpha, pane_id)
                .expect("copy mode summary")
        };
        assert!(summary.selection_active);
        let selection_start = summary
            .selection_start
            .expect("begin-selection should set a selection anchor");
        let selection_end = summary
            .selection_end
            .expect("begin-selection should set a selection end");

        if let Some(anchor) = original_anchor {
            assert_eq!(
                selection_start, anchor,
                "a second mouse-backed begin-selection must preserve the original anchor"
            );
            assert_eq!(
                selection_end.x,
                u32::from(x),
                "a second mouse-backed begin-selection should extend to the new mouse column"
            );
        } else {
            assert_eq!(
                selection_start.x,
                u32::from(x),
                "the first mouse-backed begin-selection should anchor at the mouse column"
            );
            assert_eq!(
                selection_end, selection_start,
                "the first mouse-backed begin-selection should start with a collapsed selection"
            );
            original_anchor = Some(selection_start);
        }
    }
}
