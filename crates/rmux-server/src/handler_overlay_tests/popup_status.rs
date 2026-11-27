use super::*;
use rmux_core::{input::InputParser, GridRenderOptions, Screen};

fn surface(screen: &Screen) -> (Vec<Vec<u8>>, (u32, u32), bool) {
    let options = GridRenderOptions {
        with_sequences: true,
        include_empty_cells: true,
        trim_spaces: false,
        ..Default::default()
    };
    (
        (0..54)
            .map(|row| {
                screen
                    .render_visible_line_independent(row, options)
                    .unwrap()
            })
            .collect(),
        screen.cursor_position(),
        screen.mode() & rmux_core::input::mode::MODE_CURSOR != 0,
    )
}

#[tokio::test]
async fn popup_status_full_height_idle_ticks_preserve_surface() {
    let handler = RequestHandler::new();
    let name = session_name("popup-status-idle");
    let pid = 920_244;
    let mut rx = create_in_process_attached_session(&handler, &name, pid).await;
    open_terminal_surface(&handler, pid).await;
    let (identity, popup, backlog, _) = popup_snapshot(&handler, pid).await;
    drop(drain_frames(&mut rx, &backlog));
    let mut content = b"\x1b[0m\x1b[2J".to_vec();
    for row in 0..54 {
        content.extend_from_slice(
            format!("\x1b[{};1Hidle-{row:02}|{}", row + 1, "X".repeat(217)).as_bytes(),
        );
    }
    content.extend_from_slice(b"\x1b[8;12H\x1b[?25h");
    popup.surface.lock().unwrap().append_for_test(&content);
    let done = Arc::clone(&popup.surface.lock().unwrap().flush_done_for_test);
    handler.popup_reader_tick(identity, popup.id).await.unwrap();
    timeout(Duration::from_secs(2), done.notified())
        .await
        .unwrap();
    let frames = drain_frames(&mut rx, &backlog);
    let mut screen = Screen::new(
        TerminalSize {
            cols: 225,
            rows: 54,
        },
        0,
    );
    let mut parser = InputParser::new();
    parser.parse(&frames.last().unwrap().frame, &mut screen);
    let before = surface(&screen);
    assert_eq!(before.1, (11, 7));
    assert!(before.2);
    // The native tmux 3.7b fixture checks this same idle surface over three
    // one-second status ticks. Exercise both public refresh entry paths here.
    for tick in 0..4 {
        if tick == 3 {
            // A status frame may already have been rendered when the popup
            // opens. Delivery must check the current overlay under its lock.
            handler
                .send_attached_status_if_unobscured(
                    pid,
                    identity.attach_id(),
                    &name,
                    identity.session_id(),
                    (
                        b"\x1b[54;1Hstale status".to_vec(),
                        None,
                        Some(crate::renderer::OverlayRect {
                            x: 0,
                            y: 53,
                            width: 225,
                            height: 1,
                        }),
                    ),
                )
                .await
                .unwrap();
        } else if tick % 2 == 0 {
            handler
                .refresh_attached_client_status_for_identity(pid, identity.attach_id(), &name)
                .await
                .unwrap();
        } else {
            handler
                .refresh_attached_client_status(pid, &name)
                .await
                .unwrap();
        }
        while let Ok(control) = rx.try_recv() {
            crate::pane_io::release_attach_control_backlog(
                &backlog,
                control.received_backlog_units(),
            );
            match control.materialize_received() {
                AttachControl::Write(bytes) => parser.parse(&bytes, &mut screen),
                AttachControl::Overlay(frame) => parser.parse(&frame.frame, &mut screen),
                other => panic!("unexpected idle control: {other:?}"),
            }
        }
        assert_eq!(surface(&screen), before, "status tick {tick} changed popup");
    }
    handler.clear_interactive_overlay(pid, true).await.unwrap();
    drop(drain_frames(&mut rx, &backlog));
    handler
        .refresh_attached_client_status(pid, &name)
        .await
        .unwrap();
    assert!(
        matches!(
            rx.try_recv().unwrap().materialize_received(),
            AttachControl::Write(_)
        ),
        "status refresh resumes after popup closes"
    );
}
