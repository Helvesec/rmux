use tokio::sync::mpsc;

use super::RequestHandler;
use crate::pane_io::AttachControl;
use rmux_proto::{
    DisplayPanesRequest, DisplayPanesResponse, NewSessionRequest, PaneTarget, Request,
    ResizePaneAdjustment, ResizePaneRequest, Response, SessionName, SplitDirection,
    SplitWindowRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

#[tokio::test]
async fn resize_pane_zoom_toggles_the_target_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let response = handler
        .handle(Request::ResizePane(ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 1),
            adjustment: ResizePaneAdjustment::Zoom,
        }))
        .await;

    assert_eq!(
        response,
        Response::ResizePane(rmux_proto::ResizePaneResponse {
            target: PaneTarget::new(alpha.clone(), 1),
            adjustment: ResizePaneAdjustment::Zoom,
        })
    );

    let state = handler.state.lock().await;
    assert!(state
        .sessions
        .session(&alpha)
        .expect("session exists")
        .window()
        .is_zoomed());
}

#[tokio::test]
async fn display_panes_sends_overlay_to_attached_session_without_waiting_for_clear() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 8, rows: 4 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    handler.register_attach(42, alpha.clone(), control_tx).await;

    let response = handler
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: None,
            non_blocking: false,
            no_command: false,
            template: None,
            target_client: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::DisplayPanes(DisplayPanesResponse {
            target: WindowTarget::new(alpha),
            pane_count: 2,
        })
    );
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut saw_display_panes_overlay = false;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next = tokio::time::timeout(remaining, control_rx.recv())
            .await
            .expect("display-panes control should arrive");
        let Some(next) = next else {
            break;
        };
        if let AttachControl::Overlay(overlay) = next {
            let frame = String::from_utf8(overlay.frame).expect("overlay is utf-8");
            if frame.contains("\u{1b}[41m") || frame.contains("\u{1b}[44m") {
                saw_display_panes_overlay = true;
                break;
            }
        }
    }
    assert!(
        saw_display_panes_overlay,
        "display-panes should emit an overlay frame with pane colours"
    );
}

#[tokio::test]
async fn display_panes_counts_only_labels_that_were_rendered() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();

    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(alpha.clone(), TerminalSize { cols: 3, rows: 4 })
            .expect("session create succeeds");
        state
            .sessions
            .session_mut(&alpha)
            .expect("session exists")
            .split_active_pane()
            .expect("split succeeds");
        state
            .sessions
            .session_mut(&alpha)
            .expect("session exists")
            .resize_terminal(TerminalSize { cols: 3, rows: 1 });
    }
    handler.register_attach(43, alpha.clone(), control_tx).await;

    let response = handler
        .handle(Request::DisplayPanes(Box::new(DisplayPanesRequest {
            target: alpha.clone(),
            duration_ms: None,
            non_blocking: false,
            no_command: false,
            template: None,
            target_client: None,
        })))
        .await;

    assert_eq!(
        response,
        Response::DisplayPanes(DisplayPanesResponse {
            target: WindowTarget::new(alpha),
            pane_count: 0,
        })
    );
    let overlay = control_rx.recv().await.expect("overlay control");
    let AttachControl::Overlay(overlay) = overlay else {
        panic!("expected display-panes overlay control");
    };
    assert!(overlay.frame.is_empty());
}
