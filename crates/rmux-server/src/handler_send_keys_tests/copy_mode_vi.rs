use super::*;

async fn replace_transcript_contents(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    content: &[u8],
) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("session transcript must exist")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .history_limit();
    let mut screen = rmux_core::Screen::new(size, history_limit);
    let mut parser = rmux_core::input::InputParser::new();
    parser.parse(content, &mut screen);
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .set_screen_for_test(screen);
}

#[tokio::test]
async fn send_keys_uses_copy_mode_vi_default_bindings() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 0);

    create_send_keys_test_session(&handler, &alpha).await;
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 80, rows: 24 },
        b"alpha\r\nbeta\r\n",
    )
    .await;

    let configured = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(alpha.clone(), 0)),
            option: OptionName::ModeKeys,
            value: "vi".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(configured, Response::SetOption(_)));

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

    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target,
            keys: vec!["g".to_owned(), "V".to_owned(), "Enter".to_owned()],
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 3 })
    );

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected show-buffer response");
    };
    assert_eq!(response.command_output().stdout(), b"alpha\n");
}
