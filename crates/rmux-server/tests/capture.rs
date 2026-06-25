#![cfg(unix)]

mod common;

use std::error::Error;
use std::time::Duration;

use common::{send_request, session_name, start_server, TestHarness, PTY_TEST_LOCK};
use rmux_proto::{
    CapturePaneRequest, NewSessionRequest, PaneTarget, Request, Response, SendKeysRequest,
    ShowBufferRequest, TerminalSize,
};
use tokio::time::sleep;

#[tokio::test]
async fn capture_pane_reads_unattached_transcript() -> Result<(), Box<dyn Error>> {
    let _pty_guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("capture-unattached");
    let server = start_server(&harness).await?;
    let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
    let marker = "server_capture_unattached_marker";

    let created = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let sent = send_request(
        harness.socket_path(),
        &Request::SendKeys(SendKeysRequest {
            target: target.clone(),
            keys: vec![format!("printf '{marker}\\n'"), "Enter".to_owned()],
        }),
    )
    .await?;
    assert!(matches!(sent, Response::SendKeys(_)));

    let output = wait_for_capture(harness.socket_path(), target.clone(), marker).await?;
    assert!(String::from_utf8_lossy(&output).contains(marker));

    let captured = send_request(
        harness.socket_path(),
        &Request::CapturePane(Box::new(CapturePaneRequest {
            target,
            start: None,
            end: None,
            print: false,
            buffer_name: Some("server-cap".to_owned()),
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        })),
    )
    .await?;
    match captured {
        Response::CapturePane(response) => {
            assert_eq!(response.buffer_name.as_deref(), Some("server-cap"));
            assert!(response.command_output().is_none());
        }
        other => panic!("expected capture-pane response, got {other:?}"),
    }

    let show = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest {
            name: Some("server-cap".to_owned()),
        }),
    )
    .await?;
    assert!(String::from_utf8_lossy(
        show.command_output()
            .expect("show-buffer returns output")
            .stdout()
    )
    .contains(marker));

    server.shutdown().await?;
    Ok(())
}

async fn wait_for_capture(
    socket_path: &std::path::Path,
    target: PaneTarget,
    marker: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    for _ in 0..100 {
        let response = send_request(
            socket_path,
            &Request::CapturePane(Box::new(CapturePaneRequest {
                target: target.clone(),
                start: None,
                end: None,
                print: true,
                buffer_name: None,
                alternate: false,
                escape_ansi: false,
                escape_sequences: false,
                join_wrapped: false,
                use_mode_screen: false,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: false,
                end_is_absolute: false,
            })),
        )
        .await?;
        let output = response
            .command_output()
            .expect("capture-pane -p returns output");
        if String::from_utf8_lossy(output.stdout()).contains(marker) {
            return Ok(output.stdout().to_vec());
        }

        sleep(Duration::from_millis(20)).await;
    }

    Err(format!("capture output never contained marker {marker}").into())
}
