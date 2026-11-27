//! R3634 M1: tmux 3.7b bounds every window axis at `WINDOW_MAXIMUM` (10000).
//! Without the bound, one `ESC # 8` fills a 65535x65535 pane grid and the
//! daemon stops answering while its memory grows without limit.

use super::*;
use rmux_proto::{ErrorResponse, RmuxError};

/// tmux 3.7b `WINDOW_MAXIMUM`, written out as the measured oracle value.
const WINDOW_MAXIMUM: u16 = 10_000;

const MAXIMUM: TerminalSize = TerminalSize {
    cols: WINDOW_MAXIMUM,
    rows: WINDOW_MAXIMUM,
};

async fn create_sized_session(handler: &RequestHandler, name: &str, size: Option<TerminalSize>) {
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name(name)),
            working_directory: None,
            detached: true,
            size,
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);
}

async fn resize(
    handler: &RequestHandler,
    target: &WindowTarget,
    width: Option<u16>,
    height: Option<u16>,
    adjustment: Option<ResizeWindowAdjustment>,
) -> Response {
    handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: target.clone(),
            width,
            height,
            adjustment,
        }))
        .await
}

/// Window geometry plus the active pane's real screen, the grid DECALN fills.
async fn window_and_screen_size(
    handler: &RequestHandler,
    name: &str,
) -> (TerminalSize, TerminalSize) {
    let name = session_name(name);
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&name)
        .and_then(|session| session.window_at(0))
        .expect("window 0 exists");
    let pane_id = window.active_pane().expect("active pane").id();
    let screen = state
        .pane_screen(&name, pane_id)
        .expect("active pane has a screen");
    (window.size(), screen.size())
}

#[tokio::test]
async fn r3634_resize_window_rejects_axes_beyond_window_maximum() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let target = WindowTarget::with_window(session_name("alpha"), 0);
    let initial = TerminalSize {
        cols: 120,
        rows: 40,
    };

    for (width, height, message) in [
        (Some(20_000), Some(20_000), "width too large"),
        (Some(WINDOW_MAXIMUM + 1), Some(24), "width too large"),
        (Some(80), Some(WINDOW_MAXIMUM + 1), "height too large"),
        (Some(0), Some(24), "width too small"),
        (None, Some(0), "height too small"),
    ] {
        let response = resize(&handler, &target, width, height, None).await;
        assert!(
            matches!(&response, Response::Error(ErrorResponse { error: RmuxError::Server(text) }) if text == message),
            "{width:?}x{height:?}: expected {message:?}, got {response:?}"
        );
        assert_eq!(window_and_screen_size(&handler, "alpha").await.0, initial);
    }

    let response = resize(
        &handler,
        &target,
        Some(WINDOW_MAXIMUM),
        Some(WINDOW_MAXIMUM),
        None,
    )
    .await;
    assert!(
        matches!(response, Response::ResizeWindow(_)),
        "{response:?}"
    );
    assert_eq!(
        window_and_screen_size(&handler, "alpha").await,
        (MAXIMUM, MAXIMUM)
    );
}

#[tokio::test]
async fn r3634_relative_resize_window_caps_at_window_maximum() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let target = WindowTarget::with_window(session_name("alpha"), 0);

    let response = resize(
        &handler,
        &target,
        Some(9_990),
        Some(9_990),
        Some(ResizeWindowAdjustment::Right(60_000)),
    )
    .await;
    assert!(
        matches!(response, Response::ResizeWindow(_)),
        "{response:?}"
    );
    let response = resize(
        &handler,
        &target,
        None,
        None,
        Some(ResizeWindowAdjustment::Down(u16::MAX)),
    )
    .await;
    assert!(
        matches!(response, Response::ResizeWindow(_)),
        "{response:?}"
    );
    assert_eq!(
        window_and_screen_size(&handler, "alpha").await,
        (MAXIMUM, MAXIMUM)
    );
}

#[tokio::test]
async fn r3634_new_session_size_caps_window_and_pane_screen() {
    let handler = RequestHandler::new();
    create_sized_session(
        &handler,
        "huge",
        Some(TerminalSize {
            cols: u16::MAX,
            rows: u16::MAX,
        }),
    )
    .await;
    assert_eq!(
        window_and_screen_size(&handler, "huge").await,
        (MAXIMUM, MAXIMUM)
    );

    create_sized_session(
        &handler,
        "wide",
        Some(TerminalSize {
            cols: WINDOW_MAXIMUM + 1,
            rows: 24,
        }),
    )
    .await;
    let wide = TerminalSize {
        cols: WINDOW_MAXIMUM,
        rows: 24,
    };
    assert_eq!(window_and_screen_size(&handler, "wide").await, (wide, wide));
}

#[tokio::test]
async fn r3634_default_size_caps_window_but_keeps_the_option_value() {
    let handler = RequestHandler::new();
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultSize,
            value: "30000x30000".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");

    create_sized_session(&handler, "ds", None).await;
    assert_eq!(
        window_and_screen_size(&handler, "ds").await,
        (MAXIMUM, MAXIMUM)
    );
    let state = handler.state.lock().await;
    assert_eq!(
        state.options.global_value(OptionName::DefaultSize),
        Some("30000x30000")
    );
}
