use super::*;

/// Graphics are live side effects, while the text/cursor presentation survives
/// overlays and a new subscriber. Exercise the actual parser, ring and attach
/// dispatcher with every protocol byte arriving in a separate pane read.
#[tokio::test]
async fn l12_graphics_restore_fragmented_passthrough() {
    assert_bytewise_terminal_vectors();
    let name = "graphics-restore";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    fixture.send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
        b"GRAPHICS-MENU".to_vec(),
        0,
        1,
        7,
    )));
    fixture.reader.read_until(b"GRAPHICS-MENU").await;
    let start = fixture.reader.collected.len();
    let sequences: &[&[u8]] = &[
        b"\x1b[6 q\x1b[H\x1b]8;;https://example.com/image\x1b\\RESTORED\x1b]8;;\x1b\\\x1b[3;4H",
        b"\x1bP001;2q#0!10~\x1b\\",
        b"\x1b_Ga=T,f=24,s=1,v=1;AQID\x1b\\",
        b"\x1b]52;c;QQ==\x07",
    ];
    for sequence in sequences {
        for byte in *sequence {
            fixture.publish(&[*byte]);
        }
    }
    // This control follows publication and proves the overlay is still live.
    fixture.send(AttachControl::Write(b"OVERLAY-BARRIER".to_vec()));
    fixture.reader.read_until(b"OVERLAY-BARRIER").await;
    for image in &sequences[1..3] {
        assert!(!fixture.reader.collected[start..]
            .windows(image.len())
            .any(|b| b == *image));
    }
    fixture.send(AttachControl::AdvancePersistentOverlayState(8));
    fixture.send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
        Vec::new(),
        0,
        2,
        8,
    )));
    fixture.send(AttachControl::switch(restoration_target(&fixture, name)));
    fixture.reader.read_until(sequences[2]).await;
    fixture.reader.read_until(sequences[3]).await;
    let restored = &fixture.reader.collected[start..];
    let text_at = restored
        .windows(8)
        .position(|b| b == b"RESTORED")
        .expect("restored text");
    for image in &sequences[1..3] {
        let positions: Vec<_> = restored
            .windows(image.len())
            .enumerate()
            .filter_map(|(i, b)| (b == *image).then_some(i))
            .collect();
        assert_eq!(positions.len(), 1, "exact image once: {image:?}");
        assert!(text_at < positions[0], "restore base before drawing image");
    }
    let expected_stop = fixture
        .switch_target(name, "xterm-kitty")
        .outer_terminal
        .attach_stop_sequence();
    fixture.send(AttachControl::Detach);
    fixture.reader.read_until(&expected_stop).await;
    assert!((&mut fixture.attach_task)
        .await
        .expect("detach join")
        .is_ok());

    // A later attach restores the current presentation but must not replay
    // consumed graphics or clipboard side effects from the old subscription.
    let target = restoration_target(&fixture, name);
    let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(forward_attach(
        stream,
        target,
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
    reader.read_until(b"RESTORED").await;
    let barrier = b"\x1b]2;REATTACH-BARRIER\x07";
    control_tx
        .send(AttachControl::Write(barrier.to_vec()))
        .unwrap();
    reader.read_until(barrier).await;
    for sequence in &sequences[1..] {
        assert!(
            !reader.contains(sequence),
            "a fresh attach must not replay side effects"
        );
    }
    let mut outer = rmux_core::TerminalScreen::new(PANE_SIZE, 16);
    outer.feed(&reader.collected);
    assert_eq!(outer.screen().cursor_position(), (3, 2));
    assert_eq!(outer.screen().resolved_cursor_style(), 6);
    assert!(reader.contains(b"https://example.com/image"));
    shutdown_tx.send(()).unwrap();
    assert!(task.await.unwrap().is_ok());
}

fn restoration_target(fixture: &AttachFixture, name: &str) -> AttachTarget {
    let mut target = fixture.switch_target(name, "xterm-kitty");
    target.cursor_style = fixture
        .transcript
        .lock()
        .screen()
        .resolved_cursor_style()
        .into();
    target
}

fn assert_bytewise_terminal_vectors() {
    use rmux_core::{input::OscColourSlot, TerminalScreen};
    let mut terminal = TerminalScreen::new(PANE_SIZE, 16);
    let identity = format!("\x1bP>|rmux {}\x1b\\", env!("CARGO_PKG_VERSION"));
    // DA matches the measured non-SIXEL tmux 3.7b build; identity names rmux.
    // Exact DECRQSS selectors deliberately exclude tmux's duplicated selector.
    let vectors: &[(&[u8], &[u8])] = &[
        (b"\x1b[c", b"\x1b[?1;2c"),
        (b"\x1b[>c", b"\x1b[>84;0;0c"),
        (b"\x1b[>q", identity.as_bytes()),
        (b"\x1b[6 q\x1bP$q q\x1b\\", b"\x1bP1$r6 q\x1b\\"),
        (b"\x1bP$qm\x1b\\", b"\x1bP0$r\x1b\\"),
        (
            b"\x1b]10;rgb:1111/2222/3333\x07\x1b]10;?\x07",
            b"\x1b]10;rgb:1111/2222/3333\x07",
        ),
        (
            b"\x1b]11;rgb:4444/5555/6666\x1b\\\x1b]11;?\x1b\\",
            b"\x1b]11;rgb:4444/5555/6666\x1b\\",
        ),
        (
            b"\x1b]12;rgb:7777/8888/9999\x07\x1b]12;?\x1b\\",
            b"\x1b]12;rgb:7777/8888/9999\x1b\\",
        ),
    ];
    for (vector_index, (input, expected)) in vectors.iter().enumerate() {
        // Measured tmux 3.7b replies to OSC11-ST and OSC12-ST at the ESC
        // before the final backslash; the other six reply at the last byte.
        let reply_index = match vector_index {
            6 | 7 => input.len() - 2,
            _ => input.len() - 1,
        };
        for (index, byte) in input.iter().enumerate() {
            terminal.feed(&[*byte]);
            let replies = terminal.take_replies();
            if index == reply_index {
                assert_eq!(replies, *expected, "query reply at boundary: {input:?}");
            } else {
                assert!(replies.is_empty(), "no reply outside boundary: {input:?}");
            }
        }
        assert!(terminal.take_replies().is_empty());
        assert!(terminal.take_terminal_passthrough().is_empty());
    }
    assert_eq!(terminal.screen().resolved_cursor_style(), 6);
    for (slot, colour) in [
        (OscColourSlot::Foreground, "rgb:1111/2222/3333"),
        (OscColourSlot::Background, "rgb:4444/5555/6666"),
        (OscColourSlot::Cursor, "rgb:7777/8888/9999"),
    ] {
        assert_eq!(terminal.dynamic_colour(slot), Some(colour));
    }
    for byte in b"\x1b]8;id=graphic;https://example.com/image\x1b\\X\x1b]8;;\x1b\\" {
        terminal.feed(&[*byte]);
    }
    let rendered = terminal.screen().capture_transcript(
        rmux_core::ScreenCaptureRange::default(),
        rmux_core::GridRenderOptions {
            with_sequences: true,
            ..Default::default()
        },
    );
    assert!(rendered
        .windows(b"https://example.com/image".len())
        .any(|b| b == b"https://example.com/image"));
}
