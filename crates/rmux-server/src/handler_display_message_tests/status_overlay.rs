use super::*;
use rmux_core::{input::InputParser, Screen};

fn screen_with_frame(size: TerminalSize, frame: &[u8]) -> (Screen, InputParser) {
    let mut screen = Screen::new(size, 100);
    let mut parser = InputParser::new();
    parser.parse(frame, &mut screen);
    (screen, parser)
}

fn apply_frame(parser: &mut InputParser, screen: &mut Screen, frame: &[u8]) {
    parser.parse(frame, screen);
}

fn visible_line_text(screen: &Screen, row: usize, cols: usize) -> String {
    let mut text = String::new();
    assert!(screen.visit_visible_line_cells(row, cols, |cell| text.push_str(cell.text())));
    text
}

fn assert_message_only_on_last_row(screen: &Screen, size: TerminalSize, message: &str) {
    for row in 0..usize::from(size.rows.saturating_sub(1)) {
        assert!(
            !visible_line_text(screen, row, usize::from(size.cols)).contains(message),
            "message must not remain on content row {row}"
        );
    }
    assert!(
        visible_line_text(
            screen,
            usize::from(size.rows.saturating_sub(1)),
            usize::from(size.cols),
        )
        .starts_with(message),
        "message must occupy the last terminal row"
    );
}

async fn create_status_off_attach(
    name: &str,
    attach_pid: u32,
    size: TerminalSize,
) -> (
    RequestHandler,
    SessionName,
    mpsc::UnboundedReceiver<AttachControl>,
) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(size),
                environment: None,
            }))
            .await,
    );
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Session(session.clone()),
                OptionName::Status,
                "off".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("status off");
    }
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    (handler, session, control_rx)
}

async fn display_status_off_message(
    handler: &RequestHandler,
    session: &SessionName,
    attach_pid: u32,
    message: &str,
    duration_ms: u32,
    ignore_input: bool,
) {
    assert!(matches!(
        handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(session.clone())),
                    print: false,
                    message: Some(message.to_owned()),
                    target_client: Some(attach_pid.to_string()),
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(duration_ms,)),
                    ignore_input,
                },
            )))
            .await,
        Response::DisplayMessage(_)
    ));
}

async fn expire_current_message(handler: &RequestHandler, attach_pid: u32) {
    let (identity, overlay_generation) = {
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("attached client remains active");
        (
            active.identity(attach_pid),
            active
                .transient_message
                .as_ref()
                .expect("transient message remains active")
                .overlay_generation(),
        )
    };
    handler
        .expire_transient_message_for_identity(identity, overlay_generation)
        .await;
}

async fn recv_switch_frame(control_rx: &mut mpsc::UnboundedReceiver<AttachControl>) -> Vec<u8> {
    timeout(Duration::from_secs(2), async {
        loop {
            match control_rx.recv().await.expect("attach control") {
                AttachControl::Switch(target) => break target.into_target().render_frame,
                AttachControl::Overlay(_) => {}
                other => panic!("unexpected attach control while awaiting restore: {other:?}"),
            }
        }
    })
    .await
    .expect("base restore is timely")
}

#[tokio::test]
async fn status_off_timed_ignore_message_is_visible_until_expiry_and_restores_content() {
    // Oracle tmux 3.7b: `display-message -N` remains visible on the last
    // terminal row while it owns input, then restores that content row.
    let size = TerminalSize { cols: 20, rows: 4 };
    let attach_pid = 4_701;
    let (handler, session, mut control_rx) =
        create_status_off_attach("status-off-ignore-expiry", attach_pid, size).await;
    let message = "IGNORE-VISIBLE";

    display_status_off_message(&handler, &session, attach_pid, message, 10_000, true).await;
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut control_rx).await else {
        panic!("display-message emits an overlay");
    };
    let (mut screen, mut parser) = screen_with_frame(size, &overlay.frame);
    assert_message_only_on_last_row(&screen, size, message);

    let identity = handler
        .active_attach_identity(attach_pid)
        .await
        .expect("attached identity");
    let mut pending = Vec::new();
    assert!(matches!(
        handler
            .handle_transient_message_input_for_identity(identity, &mut pending, b"x")
            .await,
        TransientMessageInput::Consumed
    ));
    assert!(pending.is_empty());

    expire_current_message(&handler, attach_pid).await;
    let AttachControl::Overlay(clear) = recv_overlay_control(&mut control_rx).await else {
        panic!("expiry first clears the overlay");
    };
    apply_frame(&mut parser, &mut screen, &clear.frame);
    let restore = recv_switch_frame(&mut control_rx).await;
    apply_frame(&mut parser, &mut screen, &restore);
    for row in 0..usize::from(size.rows) {
        assert!(
            !visible_line_text(&screen, row, usize::from(size.cols)).contains(message),
            "expired message must not leave a ghost on row {row}"
        );
    }
}

#[tokio::test]
async fn status_off_dismissible_duration_policies_forward_the_first_key() {
    // Oracle tmux 3.7b: ordinary `-d` and `-N -d 0` both dismiss and forward
    // the first key; only a positive-duration `-N` owns input until expiry.
    for (name, attach_pid, duration_ms, ignore_input) in [
        ("status-off-positive-d", 4_702, 10_000, false),
        ("status-off-N-d-zero", 4_703, 0, true),
    ] {
        let size = TerminalSize { cols: 20, rows: 4 };
        let (handler, session, mut control_rx) =
            create_status_off_attach(name, attach_pid, size).await;
        let message = "FORWARD-KEY";

        display_status_off_message(
            &handler,
            &session,
            attach_pid,
            message,
            duration_ms,
            ignore_input,
        )
        .await;
        let AttachControl::Overlay(overlay) = recv_overlay_control(&mut control_rx).await else {
            panic!("display-message emits an overlay");
        };
        let (screen, _) = screen_with_frame(size, &overlay.frame);
        assert_message_only_on_last_row(&screen, size, message);

        let identity = handler
            .active_attach_identity(attach_pid)
            .await
            .expect("attached identity");
        let mut pending = Vec::new();
        assert!(matches!(
            handler
                .handle_transient_message_input_for_identity(identity, &mut pending, b"k")
                .await,
            TransientMessageInput::Dismissed(bytes) if bytes == b"k"
        ));
        assert!(pending.is_empty());
    }
}

#[tokio::test]
async fn status_off_resize_moves_the_message_and_expiry_leaves_no_old_row_ghost() {
    // Oracle tmux 3.7b: the active message follows a grown terminal to its new
    // last row, and neither that row nor the old one survives expiry.
    let initial_size = TerminalSize { cols: 20, rows: 4 };
    let resized = TerminalSize { cols: 25, rows: 6 };
    let attach_pid = 4_704;
    let (handler, session, mut control_rx) =
        create_status_off_attach("status-off-message-resize", attach_pid, initial_size).await;
    let message = "RESIZE-VISIBLE";

    display_status_off_message(&handler, &session, attach_pid, message, 10_000, true).await;
    let AttachControl::Overlay(overlay) = recv_overlay_control(&mut control_rx).await else {
        panic!("display-message emits an overlay");
    };
    let (mut screen, mut parser) = screen_with_frame(initial_size, &overlay.frame);
    assert_message_only_on_last_row(&screen, initial_size, message);

    screen.resize(resized);
    handler
        .handle_attached_resize(attach_pid, resized)
        .await
        .expect("attached resize succeeds");
    let resized_frame = recv_switch_frame(&mut control_rx).await;
    apply_frame(&mut parser, &mut screen, &resized_frame);
    assert_message_only_on_last_row(&screen, resized, message);

    expire_current_message(&handler, attach_pid).await;
    let AttachControl::Overlay(clear) = recv_overlay_control(&mut control_rx).await else {
        panic!("expiry first clears the resized overlay");
    };
    apply_frame(&mut parser, &mut screen, &clear.frame);
    let restore = recv_switch_frame(&mut control_rx).await;
    apply_frame(&mut parser, &mut screen, &restore);
    for row in 0..usize::from(resized.rows) {
        assert!(
            !visible_line_text(&screen, row, usize::from(resized.cols)).contains(message),
            "expired resized message must not leave a ghost on row {row}"
        );
    }
}
