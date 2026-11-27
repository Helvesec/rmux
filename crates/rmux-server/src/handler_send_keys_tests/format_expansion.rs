use super::*;

fn formatted(target: Option<PaneTarget>, keys: &[&str]) -> SendKeysExtRequest {
    SendKeysExtRequest {
        target,
        keys: keys.iter().map(|key| (*key).to_owned()).collect(),
        expand_formats: true,
        hex: false,
        literal: false,
        dispatch_key_table: false,
        copy_mode_command: false,
        forward_mouse_event: false,
        reset_terminal: false,
        repeat_count: None,
    }
}

#[tokio::test]
async fn format_flag_without_copy_mode_ext2_preserves_literals_and_count() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_quiet_input_session(&handler, &alpha).await;
    create_quiet_input_session(&handler, &beta).await;
    let (tx, _rx) = mpsc::unbounded_channel();
    handler.register_attach(77, beta.clone(), tx).await;
    let capture = RawPaneInputProbe::start(&handler, &beta, "format-ext2", 0).await;
    let response = handler
        .handle(Request::SendKeysExt2(Box::new(
            rmux_proto::SendKeysExt2Request {
                target: None,
                keys: vec![
                    "#{session_name}".into(),
                    "#{session_attached}".into(),
                    "#{missing_send_key}".into(),
                    "Enter".into(),
                ],
                expand_formats: true,
                hex: false,
                literal: true,
                dispatch_key_table: false,
                copy_mode_command: false,
                forward_mouse_event: false,
                reset_terminal: false,
                repeat_count: Some(2),
                target_client: Some("77".into()),
            },
        )))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 4 })
    );
    capture
        .assert_contents(
            &handler,
            b"#{session_name}#{session_attached}#{missing_send_key}Enter#{session_name}#{session_attached}#{missing_send_key}Enter",
        )
        .await;
}

#[tokio::test]
async fn format_flag_without_copy_mode_binding_keeps_payload_literal() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_quiet_input_session(&handler, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let (tx, _rx) = mpsc::unbounded_channel();
    handler
        .register_attach(std::process::id(), alpha.clone(), tx)
        .await;
    let capture = RawPaneInputProbe::start(&handler, &alpha, "format-binding", 0).await;
    let bound = handler
        .handle(Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".into(),
            key: "x".into(),
            note: None,
            repeat: false,
            command: Some(
                ["send-keys", "-Fl", "#{session_name}:#{@format_value}"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            ),
        })))
        .await;
    assert!(matches!(bound, Response::BindKey(_)));
    // Mutate after binding definition: without -X, -F must not expand either value.
    handler
        .state
        .lock()
        .await
        .options
        .set_by_name(
            rmux_proto::OptionScopeSelector::SessionGlobal,
            "@format_value",
            Some("now".into()),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .unwrap();
    let mut request = formatted(Some(target.clone()), &["x"]);
    request.dispatch_key_table = true;
    assert_eq!(
        handler.handle(Request::SendKeysExt(request)).await,
        Response::SendKeys(SendKeysResponse { key_count: 1 })
    );
    capture
        .assert_contents(&handler, b"#{session_name}:#{@format_value}")
        .await;
    // tmux injects an unknown key name character by character, without -F expansion.
    for (keys, expected) in [
        (
            vec!["#{?session_name,x,y}"],
            "#{?session_name,#{session_name}:#{@format_value},y}",
        ),
        (
            vec!["bad", "x", "#{missing_send_key_format}"],
            "bad#{session_name}:#{@format_value}#{missing_send_key_format}",
        ),
    ] {
        let capture = RawPaneInputProbe::start(&handler, &alpha, "format-invalid-binding", 0).await;
        let mut request = formatted(Some(target.clone()), &keys);
        request.dispatch_key_table = true;
        assert_eq!(
            handler.handle(Request::SendKeysExt(request)).await,
            Response::SendKeys(SendKeysResponse {
                key_count: keys.len()
            })
        );
        capture.assert_contents(&handler, expected.as_bytes()).await;
    }
    let capture = RawPaneInputProbe::start(&handler, &alpha, "format-hex-binding", 0).await;
    let mut request = formatted(Some(target), &["#{session_name}", "78", "invalid"]);
    request.dispatch_key_table = true;
    request.hex = true;
    assert_eq!(
        handler.handle(Request::SendKeysExt(request)).await,
        Response::SendKeys(SendKeysResponse { key_count: 3 })
    );
    capture
        .assert_contents(&handler, b"#{session_name}:#{@format_value}")
        .await;
}

#[tokio::test]
async fn format_expansion_copy_mode_command_uses_current_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_quiet_input_session(&handler, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let capture = RawPaneInputProbe::start(&handler, &alpha, "format-copy", 0).await;
    let response = handler
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
    assert!(matches!(response, Response::CopyMode(_)));
    // -F expands search arguments under -X; it does not name a copy-mode command.
    let mut request = formatted(Some(target.clone()), &["search-forward", "#{pane_width}"]);
    request.copy_mode_command = true;
    assert!(matches!(
        handler.handle(Request::SendKeysExt(request)).await,
        Response::SendKeys(_)
    ));
    let report = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha.clone(),
            format: Some("#{pane_search_string}".into()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let Response::ListPanes(report) = report else {
        panic!("expected list-panes");
    };
    assert_eq!(report.command_output().stdout(), b"80\n");
    assert!(handler.target_is_in_copy_mode(&target).await.unwrap());
    // tmux 3.7b: this ONE argv stays literal without -X. Its q cancels copy
    // mode; the suffix must not become application input (with or without -F).
    let response = handler
        .handle(Request::SendKeysExt(formatted(
            Some(target.clone()),
            &["#{?session_name,q,invalid}"],
        )))
        .await;
    assert!(!handler.target_is_in_copy_mode(&target).await.unwrap());
    capture.assert_contents(&handler, b"").await;
    let Response::Error(error) = response else {
        panic!("expected not in a mode, got {response:?}");
    };
    assert!(error.error.to_string().contains("not in a mode"));
}

#[tokio::test]
async fn format_expansion_plain_copy_mode_preserves_argument_boundary() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_quiet_input_session(&handler, &alpha).await;
    let target = PaneTarget::new(alpha.clone(), 0);
    let capture = RawPaneInputProbe::start(&handler, &alpha, "plain-copy-boundary", 0).await;
    assert!(matches!(
        handler
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
            .await,
        Response::CopyMode(_)
    ));
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec!["#{?session_name,q,invalid}".into()],
        }))
        .await;
    assert!(!handler.target_is_in_copy_mode(&target).await.unwrap());
    capture.assert_contents(&handler, b"").await;
    let Response::Error(error) = response else {
        panic!("expected not in a mode, got {response:?}");
    };
    assert!(error.error.to_string().contains("not in a mode"));
}
