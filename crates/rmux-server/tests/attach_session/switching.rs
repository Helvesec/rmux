use std::error::Error;

use rmux_proto::{
    encode_attach_message, AttachMessage, AttachSessionRequest, KillSessionRequest,
    NewSessionRequest, OptionName, PaneTarget, Request, Response, ScopeSelector, SelectPaneRequest,
    SendKeysRequest, SetOptionMode, SetOptionRequest, SplitWindowRequest, SplitWindowTarget,
    SwitchClientRequest, TerminalSize, WindowTarget,
};
use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

use crate::common::{session_name, start_server, ClientConnection, TestHarness, PTY_TEST_LOCK};
use crate::support::{read_attach_until_contains, STEP_TIMEOUT};

async fn send_attach_command(
    stream: &mut tokio::net::UnixStream,
    command: &str,
) -> Result<(), Box<dyn Error>> {
    let mut bytes = command.as_bytes().to_vec();
    bytes.push(b'\r');
    let frame = encode_attach_message(&AttachMessage::Data(bytes))?;
    stream.write_all(&frame).await?;
    Ok(())
}

#[tokio::test]
async fn switch_client_reroutes_attach_input_and_output() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("switch-client");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created_alpha = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created_alpha, Response::NewSession(_)));

    let created_beta = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("beta"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created_beta, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;

    let alpha_output = crate::common::send_request(
        &socket_path,
        &Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name("alpha"), 0),
            keys: vec!["printf alpha-output".to_owned(), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(alpha_output, Response::SendKeys(_)));
    let alpha_output =
        read_attach_until_contains(&mut attach_stream, "alpha-output", STEP_TIMEOUT).await?;
    assert!(alpha_output.contains("alpha-output"));

    let switched = crate::common::send_request(
        &socket_path,
        &Request::SwitchClient(SwitchClientRequest {
            target: session_name("beta"),
        }),
    )
    .await?;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: session_name("beta"),
        })
    );

    send_attach_command(&mut attach_stream, "printf beta-input").await?;
    let beta_input =
        read_attach_until_contains(&mut attach_stream, "beta-input", STEP_TIMEOUT).await?;
    assert!(beta_input.contains("beta-input"));

    let beta_output = crate::common::send_request(
        &socket_path,
        &Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name("beta"), 0),
            keys: vec!["printf beta-output".to_owned(), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(beta_output, Response::SendKeys(_)));
    let beta_output =
        read_attach_until_contains(&mut attach_stream, "beta-output", STEP_TIMEOUT).await?;
    assert!(beta_output.contains("beta-output"));

    drop(attach_stream);
    for target in [session_name("alpha"), session_name("beta")] {
        let removed = crate::common::send_request(
            &socket_path,
            &Request::KillSession(KillSessionRequest {
                target,
                kill_all_except_target: false,
                clear_alerts: false,
            }),
        )
        .await?;
        assert_eq!(
            removed,
            Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
        );
    }
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn switch_client_to_multi_pane_session_emits_border_frame_before_forwarding_io(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("switch-client-borders");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    let created_alpha = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created_alpha, Response::NewSession(_)));

    let created_beta = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: beta.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created_beta, Response::NewSession(_)));

    let first_split = crate::common::send_request(
        &socket_path,
        &Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(beta.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(first_split, Response::SplitWindow(_)));

    let second_split = crate::common::send_request(
        &socket_path,
        &Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(beta.clone(), 1)),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(second_split, Response::SplitWindow(_)));

    let selected = crate::common::send_request(
        &socket_path,
        &Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(beta.clone(), 2),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })),
    )
    .await?;
    assert!(matches!(selected, Response::SelectPane(_)));

    for (scope, option, value) in [
        (
            ScopeSelector::Window(WindowTarget::new(beta.clone())),
            OptionName::PaneBorderStyle,
            "blue",
        ),
        (
            ScopeSelector::Window(WindowTarget::new(beta.clone())),
            OptionName::PaneActiveBorderStyle,
            "colour196",
        ),
        (
            ScopeSelector::Session(beta.clone()),
            OptionName::Status,
            "off",
        ),
    ] {
        let response = crate::common::send_request(
            &socket_path,
            &Request::SetOption(SetOptionRequest {
                scope,
                option,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }),
        )
        .await?;
        assert!(matches!(response, Response::SetOption(_)));
    }

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest { target: alpha })
        .await?;
    read_attach_until_contains(&mut attach_stream, "[alpha]", STEP_TIMEOUT).await?;

    let switched = crate::common::send_request(
        &socket_path,
        &Request::SwitchClient(SwitchClientRequest {
            target: beta.clone(),
        }),
    )
    .await?;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );

    let border_text =
        read_attach_until_contains(&mut attach_stream, "\u{1b}[34m", STEP_TIMEOUT).await?;
    assert!(border_text.contains("\u{1b}[34m"));
    assert!(border_text.contains("\u{1b}[38;5;196m"));
    assert!(border_text.contains('│'));

    send_attach_command(&mut attach_stream, "printf beta-input").await?;
    let beta_input =
        read_attach_until_contains(&mut attach_stream, "beta-input", STEP_TIMEOUT).await?;
    assert!(beta_input.contains("beta-input"));

    let beta_output = crate::common::send_request(
        &socket_path,
        &Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(beta.clone(), 2),
            keys: vec!["printf beta-output".to_owned(), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(beta_output, Response::SendKeys(_)));
    let beta_output =
        read_attach_until_contains(&mut attach_stream, "beta-output", STEP_TIMEOUT).await?;
    assert!(beta_output.contains("beta-output"));

    drop(attach_stream);
    for target in [session_name("alpha"), session_name("beta")] {
        let removed = crate::common::send_request(
            &socket_path,
            &Request::KillSession(KillSessionRequest {
                target,
                kill_all_except_target: false,
                clear_alerts: false,
            }),
        )
        .await?;
        assert_eq!(
            removed,
            Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
        );
    }
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

#[tokio::test]
async fn switch_client_to_missing_session_keeps_the_current_attach_stream(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("switch-missing");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = crate::common::send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let (_, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name("alpha"),
        })
        .await?;

    let switched = crate::common::send_request(
        &socket_path,
        &Request::SwitchClient(SwitchClientRequest {
            target: session_name("missing"),
        }),
    )
    .await?;
    assert_eq!(
        switched,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::SessionNotFound("missing".to_owned()),
        })
    );

    send_attach_command(&mut attach_stream, "printf still-alpha").await?;
    let still_alpha =
        read_attach_until_contains(&mut attach_stream, "still-alpha", STEP_TIMEOUT).await?;
    assert!(still_alpha.contains("still-alpha"));

    let still_output = crate::common::send_request(
        &socket_path,
        &Request::SendKeys(SendKeysRequest {
            target: PaneTarget::new(session_name("alpha"), 0),
            keys: vec!["printf still-output".to_owned(), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(still_output, Response::SendKeys(_)));
    let still_output =
        read_attach_until_contains(&mut attach_stream, "still-output", STEP_TIMEOUT).await?;
    assert!(still_output.contains("still-output"));

    drop(attach_stream);
    let removed = crate::common::send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}
