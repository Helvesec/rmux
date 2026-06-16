use super::*;
use crate::pane_terminals::PaneCaptureRequest;
use rmux_core::{GridRenderOptions, ScreenCaptureRange};

#[tokio::test]
async fn parsed_queue_resize_pane_trim_flag_trims_below_cursor() {
    let handler = RequestHandler::new();
    let session = session_name("resize-trim");
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 10, rows: 5 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(
                &session,
                0,
                0,
                b"01\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n10\x1b[3;1H",
            )
            .expect("transcript append succeeds");
    }

    execute(&handler, "resize-pane -T -t resize-trim:0.0").await;

    let captured = {
        let state = handler.state.lock().await;
        state
            .capture_transcript(
                &target,
                PaneCaptureRequest {
                    range: ScreenCaptureRange {
                        start_is_absolute: true,
                        end_is_absolute: true,
                        ..ScreenCaptureRange::default()
                    },
                    options: GridRenderOptions::default(),
                    alternate: false,
                    use_mode_screen: false,
                    pending_input: false,
                    quiet: false,
                    escape_pending: false,
                },
            )
            .expect("capture succeeds")
    };
    let captured = String::from_utf8(captured).expect("capture is utf-8");
    assert_eq!(
        captured.lines().collect::<Vec<_>>(),
        vec!["01", "02", "03", "04", "05", "06", "07", "08"]
    );
}

#[tokio::test]
async fn parsed_queue_resize_pane_trim_flag_takes_precedence_over_size_flags() {
    let handler = RequestHandler::new();
    let session = session_name("resize-trim-size");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    execute(&handler, "split-window -v -t resize-trim-size:0.0").await;

    let before = pane_height(&handler, &session, 0).await;
    execute(&handler, "resize-pane -T -y 5 -t resize-trim-size:0.0").await;
    let after = pane_height(&handler, &session, 0).await;

    assert_eq!(after, before);
}

#[tokio::test]
async fn parsed_queue_resize_pane_mouse_flag_is_noop_without_mouse_context() {
    let handler = RequestHandler::new();
    let session = session_name("resize-mouse-noop");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    execute(&handler, "split-window -h -t resize-mouse-noop:0.0").await;

    let before = pane_sizes(&handler, &session).await;
    execute(&handler, "resize-pane -M -t resize-mouse-noop:0.0").await;
    let after = pane_sizes(&handler, &session).await;

    assert_eq!(after, before);
}

async fn execute(handler: &RequestHandler, command: &str) {
    let parsed = CommandParser::new().parse(command).expect("command parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap_or_else(|error| panic!("{command} should execute: {error}"));
}

async fn pane_height(handler: &RequestHandler, session: &SessionName, pane_index: u32) -> u16 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .pane(pane_index)
        .expect("pane exists")
        .geometry()
        .rows()
}

async fn pane_sizes(handler: &RequestHandler, session: &SessionName) -> Vec<(u16, u16)> {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(session)
        .expect("session exists")
        .window_at(0)
        .expect("window exists")
        .panes()
        .iter()
        .map(|pane| {
            let geometry = pane.geometry();
            (geometry.cols(), geometry.rows())
        })
        .collect()
}
