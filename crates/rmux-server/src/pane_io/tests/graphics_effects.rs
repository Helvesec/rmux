use super::*;

const EFFECTS: &[u8] =
    b"\x1b[6 q\x1b[H\x1b]8;;https://example.com/image\x1b\\RESTORED\x1b]8;;\x1b\\\x1b[3;4H";

#[tokio::test]
async fn l12_graphics_effects_restore_link_and_cursor_state() {
    // Reuse the effects vector from the graphics restoration test. Every
    // two-chunk split and bytewise input must preserve the same presentation.
    for split in 0..=EFFECTS.len() {
        let mut terminal = rmux_core::TerminalScreen::new(PANE_SIZE, 16);
        terminal.feed(&EFFECTS[..split]);
        terminal.feed(&EFFECTS[split..]);
        assert_effects(&terminal);
    }
    let name = "graphics-effects";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    fixture.send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
        b"\x1b[2 qGRAPHICS-MENU".to_vec(),
        0,
        1,
        7,
    )));
    fixture.reader.read_until(b"GRAPHICS-MENU").await;
    for byte in EFFECTS {
        fixture.publish(&[*byte]);
    }
    fixture.send(AttachControl::Write(b"\x1b]2;OVERLAY-BARRIER\x07".to_vec()));
    fixture.reader.read_until(b"OVERLAY-BARRIER").await;
    fixture.send(AttachControl::AdvancePersistentOverlayState(8));
    fixture.send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
        Vec::new(),
        0,
        2,
        8,
    )));
    fixture.send(AttachControl::switch(effects_target(&fixture, name)));
    fixture.send(AttachControl::Write(b"\x1b]2;RESTORE-BARRIER\x07".to_vec()));
    fixture.reader.read_until(b"RESTORE-BARRIER").await;
    let mut outer = rmux_core::TerminalScreen::new(PANE_SIZE, 16);
    outer.feed(&fixture.reader.collected);
    assert_effects(&outer);
    let stop = effects_target(&fixture, name)
        .outer_terminal
        .attach_stop_sequence();
    fixture.send(AttachControl::Detach);
    fixture.reader.read_until(&stop).await;
    assert!((&mut fixture.attach_task).await.unwrap().is_ok());

    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(forward_attach(
        stream,
        effects_target(&fixture, name),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::unregistered_for_test(
            Arc::new(RequestHandler::new()),
            std::process::id(),
        ),
        false,
    ));
    let mut reader = AttachReader::new(peer);
    control_tx
        .send(AttachControl::Write(
            b"\x1b]2;REATTACH-BARRIER\x07".to_vec(),
        ))
        .unwrap();
    reader.read_until(b"REATTACH-BARRIER").await;
    let mut outer = rmux_core::TerminalScreen::new(PANE_SIZE, 16);
    outer.feed(&reader.collected);
    assert_effects(&outer);
    shutdown_tx.send(()).unwrap();
    assert!(task.await.unwrap().is_ok());
}

fn effects_target(fixture: &AttachFixture, name: &str) -> AttachTarget {
    let mut target = fixture.switch_target(name, "xterm-kitty");
    target.cursor_style = fixture
        .transcript
        .lock()
        .screen()
        .resolved_cursor_style()
        .into();
    target
}

fn assert_effects(terminal: &rmux_core::TerminalScreen) {
    assert_eq!(terminal.screen().cursor_position(), (3, 2));
    assert_eq!(
        terminal.screen().resolved_cursor_style(),
        6,
        "restored cursor style"
    );
    let mut text = String::new();
    assert!(terminal.screen().visit_visible_line_cells(0, 9, |cell| {
        if text.len() < 8 {
            assert_eq!(
                terminal.screen().hyperlink_uri(cell.link()),
                Some("https://example.com/image"),
                "restored hyperlink cell"
            );
            text.push_str(cell.text());
        } else {
            assert_eq!(cell.link(), 0, "hyperlink ends after restored text");
        }
    }));
    assert_eq!(text, "RESTORED");
}
