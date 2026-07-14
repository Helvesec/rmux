use super::*;

#[derive(Debug, Clone, Copy)]
enum PaletteModalSurface {
    Prompt,
    ModeTree,
    Overlay,
    DisplayPanes,
}

impl PaletteModalSurface {
    const fn name(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::ModeTree => "mode-tree",
            Self::Overlay => "overlay",
            Self::DisplayPanes => "display-panes",
        }
    }
}

async fn activate_surface(
    handler: &RequestHandler,
    requester_pid: u32,
    surface: PaletteModalSurface,
) {
    let command = match surface {
        PaletteModalSurface::Prompt => "command-prompt -b -p Palette",
        PaletteModalSurface::ModeTree => "choose-tree -Zw",
        PaletteModalSurface::Overlay => {
            r#"display-menu -T Palette "Keep" "k" "display-message keep""#
        }
        PaletteModalSurface::DisplayPanes => "display-panes -b -d 60000",
    };
    let commands = handler
        .parse_control_commands(command)
        .await
        .unwrap_or_else(|error| panic!("{} command parses: {error}", surface.name()));
    handler
        .execute_parsed_commands_for_test(requester_pid, commands)
        .await
        .unwrap_or_else(|error| panic!("{} activates: {error}", surface.name()));
    assert_surface_active(handler, requester_pid, surface).await;
}

async fn assert_surface_active(
    handler: &RequestHandler,
    requester_pid: u32,
    surface: PaletteModalSurface,
) {
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&requester_pid)
        .expect("attached client remains registered");
    let is_active = match surface {
        PaletteModalSurface::Prompt => active.prompt.is_some(),
        PaletteModalSurface::ModeTree => active.mode_tree.is_some(),
        PaletteModalSurface::Overlay => active.overlay.is_some(),
        PaletteModalSurface::DisplayPanes => active.display_panes.is_some(),
    };
    assert!(is_active, "{} must remain active", surface.name());
}

async fn register_palette_query(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    query: &[u8],
) {
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(session, 0, 0, query)
        .expect("pane emits palette query");
}

#[tokio::test]
async fn fragmented_correlated_palette_response_bypasses_every_attached_modal_surface() {
    for (surface, query, response) in [
        (
            PaletteModalSurface::Prompt,
            b"\x1b]4;0;?\x07".as_slice(),
            b"\x1b]4;0;rgb:0000/1111/ffff\x07".as_slice(),
        ),
        (
            PaletteModalSurface::ModeTree,
            b"\x1b]4;1;?\x1b\\".as_slice(),
            b"\x1b]4;1;rgb:1111/2222/3333\x1b\\".as_slice(),
        ),
        (
            PaletteModalSurface::Overlay,
            b"\x1b]4;2;?\x07".as_slice(),
            b"\x1b]4;2;rgb:2222/3333/4444\x07".as_slice(),
        ),
        (
            PaletteModalSurface::DisplayPanes,
            b"\x1b]4;3;?\x1b\\".as_slice(),
            b"\x1b]4;3;rgb:3333/4444/5555\x1b\\".as_slice(),
        ),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name(&format!("palette-modal-{}", surface.name()));
        let requester_pid = std::process::id();
        create_send_keys_test_session(&handler, &alpha).await;
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        let _attach_id = handler
            .register_attach(requester_pid, alpha.clone(), control_tx)
            .await;
        register_palette_query(&handler, &alpha, query).await;
        activate_surface(&handler, requester_pid, surface).await;
        let capture = RawPaneInputProbe::start(
            &handler,
            &alpha,
            &format!("palette-modal-{}", surface.name()),
            response.len(),
        )
        .await;

        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &response[..2])
            .await
            .expect("fragmented palette opener");
        assert_eq!(pending_input, response[..2]);
        assert_surface_active(&handler, requester_pid, surface).await;

        handler
            .handle_attached_live_input(
                requester_pid,
                &mut pending_input,
                &response[2..response.len() - 1],
            )
            .await
            .expect("fragmented palette body");
        assert_eq!(pending_input, response[..response.len() - 1]);
        assert_surface_active(&handler, requester_pid, surface).await;

        handler
            .handle_attached_live_input(
                requester_pid,
                &mut pending_input,
                &response[response.len() - 1..],
            )
            .await
            .expect("palette terminator");
        assert!(pending_input.is_empty());
        assert_surface_active(&handler, requester_pid, surface).await;

        capture.finish(&handler, &alpha).await;
        capture.assert_contents(&handler, response).await;
    }
}

#[tokio::test]
async fn command_prompt_palette_response_survives_every_fragment_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-all-splits");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;
    let query = b"\x1b]4;9;?\x1b\\";
    let response = b"\x1b]4;9;rgb:1111/2222/3333\x1b\\";
    let response_count = response.len() - 1;
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "palette-modal-all-splits",
        response.len() * response_count,
    )
    .await;

    for split in 1..response.len() {
        register_palette_query(&handler, &alpha, query).await;
        let mut pending_input = Vec::new();
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &response[..split])
            .await
            .unwrap_or_else(|error| panic!("first response fragment at {split}: {error}"));
        assert_eq!(pending_input, response[..split], "split at {split}");
        assert_surface_active(&handler, requester_pid, PaletteModalSurface::Prompt).await;
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &response[split..])
            .await
            .unwrap_or_else(|error| panic!("second response fragment at {split}: {error}"));
        assert!(pending_input.is_empty(), "split at {split}");
        assert_surface_active(&handler, requester_pid, PaletteModalSurface::Prompt).await;
    }

    let expected = response.repeat(response_count);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn modal_close_palette_response_and_tail_keep_wire_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-close-order");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    register_palette_query(&handler, &alpha, b"\x1b]4;7;?\x07").await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;

    let response = b"\x1b]4;7;rgb:1111/2222/3333\x07";
    let mut input = b"\r".to_vec();
    input.extend_from_slice(response);
    input.extend_from_slice(b"TAIL\n");
    let mut expected = response.to_vec();
    expected.extend_from_slice(b"TAIL\n");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "palette-modal-close-order",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, &input)
        .await
        .expect("prompt close, response, and pane tail");

    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach.by_pid[&requester_pid].prompt.is_none());
    drop(active_attach);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

#[tokio::test]
async fn modal_close_reveals_underlying_surface_before_response_and_tail() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-transition-order");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::ModeTree).await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;
    register_palette_query(&handler, &alpha, b"\x1b]4;11;?\x07").await;

    let response = b"\x1b]4;11;rgb:1111/2222/3333\x07";
    let mut input = b"\x03".to_vec();
    input.extend_from_slice(response);
    input.extend_from_slice(b"q");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "palette-modal-transition-order",
        response.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, &input)
        .await
        .expect("prompt close, response, and underlying-surface tail");

    let active_attach = handler.active_attach.lock().await;
    let active = &active_attach.by_pid[&requester_pid];
    assert!(active.prompt.is_none());
    assert!(
        active.mode_tree.is_none(),
        "the q tail must reach and close the revealed mode-tree"
    );
    drop(active_attach);
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, response).await;
}

#[tokio::test]
async fn correlated_response_does_not_join_or_discard_partial_modal_input() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-partial-input");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    register_palette_query(&handler, &alpha, b"\x1b]4;12;?\x07").await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;

    let response = b"\x1b]4;12;rgb:1111/2222/3333\x07";
    let mut input = b"\xe6\x97".to_vec();
    input.extend_from_slice(response);
    input.push(0xa5);
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "palette-modal-partial-input",
        response.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, &input)
        .await
        .expect("partial UTF-8, response, and completing byte");

    let prompt = handler
        .attached_prompt_render(requester_pid)
        .await
        .expect("prompt remains active");
    assert_eq!(prompt.input, "日");
    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, response).await;
}

#[tokio::test]
async fn retained_palette_response_survives_modal_replacement() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-replacement");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    register_palette_query(&handler, &alpha, b"\x1b]4;8;?\x1b\\").await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;
    let response = b"\x1b]4;8;rgb:1111/2222/3333\x1b\\";
    let split = response.len() - 1;
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "palette-modal-replacement",
        response.len(),
    )
    .await;
    let mut pending_input = Vec::new();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &response[..split])
        .await
        .expect("partial response under prompt");
    assert_eq!(pending_input, response[..split]);

    handler.clear_prompt_for_attach(requester_pid).await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Overlay).await;
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &response[split..])
        .await
        .expect("response completes under replacement overlay");
    assert!(pending_input.is_empty());
    assert_surface_active(&handler, requester_pid, PaletteModalSurface::Overlay).await;

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, response).await;
}

#[tokio::test]
async fn incomplete_palette_like_user_input_resolves_on_escape_timeout() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-user-prefix");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "palette-modal-user-prefix", 3).await;
    let mut pending_input = Vec::new();

    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, b"\x1b]4;")
        .await
        .expect("palette-like user prefix is retained");
    assert_eq!(pending_input, b"\x1b]4;");
    handler
        .flush_attached_pending_escape_input(requester_pid, &mut pending_input)
        .await
        .expect("palette-like user prefix resolves at timeout");
    assert!(pending_input.is_empty());
    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach.by_pid[&requester_pid].prompt.is_none());
    drop(active_attach);

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, b"]4;").await;
}

#[tokio::test]
async fn uncorrelated_palette_response_preserves_modal_decoder_bytes() {
    let handler = RequestHandler::new();
    let alpha = session_name("palette-modal-uncorrelated");
    let requester_pid = std::process::id();
    create_send_keys_test_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    activate_surface(&handler, requester_pid, PaletteModalSurface::Prompt).await;

    let response = b"\x1b]4;10;rgb:1111/2222/3333\x07";
    let mut input = response.to_vec();
    input.extend_from_slice(b"TAIL");
    // The established prompt behavior consumes the leading ESC as prompt
    // cancellation and reroutes every remaining byte. A missing query slot
    // must not make the OSC splitter consume, rewrite, or reorder that input.
    let expected = &input[1..];
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "palette-modal-uncorrelated",
        expected.len(),
    )
    .await;

    handler
        .handle_attached_live_input_for_test(requester_pid, &input)
        .await
        .expect("uncorrelated response follows the established prompt path");
    let active_attach = handler.active_attach.lock().await;
    assert!(active_attach.by_pid[&requester_pid].prompt.is_none());
    drop(active_attach);

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}
