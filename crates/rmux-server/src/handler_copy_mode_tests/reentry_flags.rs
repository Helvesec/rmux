//! The flags of `copy-mode` on a pane that is already in copy mode.
//!
//! tmux 3.7b returns from `window_pane_set_mode` before `window_copy_init`
//! when copy mode is already the pane's mode, so `-H` and `-e` keep what the
//! first entry set; `-u` and `-d` still page, and `-d` pages with this
//! command's own `-e` (`cmd-copy-mode.c`). Every expectation below was
//! measured on tmux 3.7b (S2231 `sonde-drapeaux.sh` and `sonde-client.sh`),
//! never derived from rmux.

use super::*;

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 3 };

/// `copy-mode` with the flags among `d`, `e`, `H` and `u` that `flags` names.
async fn copy_mode(handler: &RequestHandler, target: &PaneTarget, flags: &str) -> Response {
    handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: flags.contains('d'),
            exit_on_scroll: flags.contains('e'),
            hide_position: flags.contains('H'),
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: flags.contains('u'),
        }))
        .await
}

async fn send(handler: &RequestHandler, target: &PaneTarget, command: &str) {
    let response = send_copy_mode_command(handler, target, &[command]).await;
    assert!(matches!(response, Response::SendKeys(_)), "{response:?}");
}

/// `#{pane_in_mode}`, then whether the view is at the bottom while in mode.
async fn in_mode(handler: &RequestHandler, target: &PaneTarget) -> String {
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some(
                "#{pane_in_mode}#{?pane_in_mode, #{?#{==:#{scroll_position},0},bottom,up},}"
                    .to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })))
        .await;
    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    String::from_utf8_lossy(output.stdout()).into_owned()
}

async fn shows_position(handler: &RequestHandler, target: &PaneTarget) -> bool {
    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .expect("target pane exists")
        .id();
    state
        .pane_copy_mode_summary(target.session_name(), pane_id)
        .expect("copy mode summary")
        .show_position
}

/// A pane with ten numbered lines, so two pages fit above the bottom.
async fn ten_lines(name: &str) -> (RequestHandler, PaneTarget) {
    let handler = RequestHandler::new();
    let target = create_session(&handler, name, SIZE).await;
    let lines: String = (1..=10).map(|line| format!("line {line}\r\n")).collect();
    replace_transcript_contents(&handler, &target, SIZE, lines.as_bytes()).await;
    (handler, target)
}

fn entered(response: Response, active: bool) {
    match response {
        Response::CopyMode(response) => assert_eq!(response.active, active),
        other => panic!("copy-mode answered {other:?}"),
    }
}

#[tokio::test]
async fn reentry_with_exit_on_scroll_does_not_arm_it() {
    let (handler, target) = ten_lines("reentry-e").await;
    entered(copy_mode(&handler, &target, "").await, true);
    send(&handler, &target, "page-up").await;
    entered(copy_mode(&handler, &target, "e").await, true);
    send(&handler, &target, "page-down").await;

    // tmux 3.7b (e1): `in_mode=1 pos=0`.
    assert_eq!(in_mode(&handler, &target).await, "1 bottom\n");
}

#[tokio::test]
async fn reentry_keeps_the_position_indicator_of_the_first_entry() {
    let (handler, target) = ten_lines("reentry-h-shown").await;
    entered(copy_mode(&handler, &target, "").await, true);
    entered(copy_mode(&handler, &target, "H").await, true);
    // tmux 3.7b (h1): `[0/477]` still drawn after `copy-mode -H`.
    assert!(shows_position(&handler, &target).await);

    let (handler, target) = ten_lines("reentry-h-hidden").await;
    entered(copy_mode(&handler, &target, "H").await, true);
    entered(copy_mode(&handler, &target, "").await, true);
    // tmux 3.7b (h2): no indicator after a plain `copy-mode`.
    assert!(!shows_position(&handler, &target).await);
}

#[tokio::test]
async fn page_down_on_entry_leaves_at_the_bottom_with_its_own_exit_on_scroll() {
    // tmux 3.7b (d5): `copy-mode -e -d` on a pane at the bottom never stays.
    let (handler, target) = ten_lines("entry-ed").await;
    entered(copy_mode(&handler, &target, "ed").await, false);
    assert_eq!(in_mode(&handler, &target).await, "0\n");

    // tmux 3.7b (d3): one page up, then `copy-mode -e -d`, leaves the mode.
    let (handler, target) = ten_lines("reentry-ed").await;
    entered(copy_mode(&handler, &target, "").await, true);
    send(&handler, &target, "page-up").await;
    entered(copy_mode(&handler, &target, "ed").await, false);
    assert_eq!(in_mode(&handler, &target).await, "0\n");
}

#[tokio::test]
async fn page_down_on_reentry_reads_the_command_exit_on_scroll_not_the_mode() {
    // tmux 3.7b (d2): entered with `-e`, `copy-mode -d` reaches the bottom
    // and stays, because this `-d` has no `-e`.
    let (handler, target) = ten_lines("reentry-d-armed").await;
    entered(copy_mode(&handler, &target, "e").await, true);
    send(&handler, &target, "page-up").await;
    entered(copy_mode(&handler, &target, "d").await, true);
    assert_eq!(in_mode(&handler, &target).await, "1 bottom\n");

    // tmux 3.7b (d4): two pages up, `copy-mode -e -d` stops above the bottom
    // and its `-e` arms nothing, so the next page-down stays in the mode.
    let (handler, target) = ten_lines("reentry-ed-above").await;
    entered(copy_mode(&handler, &target, "").await, true);
    send(&handler, &target, "page-up").await;
    send(&handler, &target, "page-up").await;
    entered(copy_mode(&handler, &target, "ed").await, true);
    assert_eq!(in_mode(&handler, &target).await, "1 up\n");
    send(&handler, &target, "page-down").await;
    assert_eq!(in_mode(&handler, &target).await, "1 bottom\n");
}
