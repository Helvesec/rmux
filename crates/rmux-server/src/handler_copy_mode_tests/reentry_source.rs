//! `copy-mode` on a pane that is already in copy mode.
//!
//! tmux 3.7b returns from `window_pane_set_mode` before `window_copy_init`
//! when copy mode is already the pane's mode, so the re-entry keeps both the
//! backing screen and the pane `refresh-from-pane` reads (`wme->swp`). The
//! expected lines below were measured on tmux 3.7b (S2218 `sonde-s.sh`, cases
//! 1 to 3), never derived from rmux.

use super::*;

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 3 };

async fn copy_mode_with_source(
    handler: &RequestHandler,
    target: &PaneTarget,
    source: Option<&PaneTarget>,
) {
    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: source.cloned(),
            page_up: false,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)), "{response:?}");
}

/// The copy cursor line after `motions`, read through `#{copy_cursor_line}`.
async fn line_after(handler: &RequestHandler, target: &PaneTarget, motions: &[&str]) -> String {
    for motion in motions {
        assert!(
            matches!(
                send_copy_mode_command(handler, target, &[motion]).await,
                Response::SendKeys(_)
            ),
            "{motion}"
        );
    }
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some("#{pane_mode} #{copy_cursor_line}".to_owned()),
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

/// A target with ten numbered lines and a one-line source in another session.
async fn target_and_source(name: &str) -> (RequestHandler, PaneTarget, PaneTarget) {
    let handler = RequestHandler::new();
    let target = create_session(&handler, &format!("{name}-target"), SIZE).await;
    let source = create_session(&handler, &format!("{name}-source"), SIZE).await;
    let lines: String = (1..=10).map(|line| format!("line {line}\r\n")).collect();
    replace_transcript_contents(&handler, &target, SIZE, lines.as_bytes()).await;
    replace_transcript_contents(&handler, &source, SIZE, b"SOURCE\r\n").await;
    (handler, target, source)
}

#[tokio::test]
async fn copy_mode_source_on_a_pane_already_in_copy_mode_keeps_its_screen() {
    let (handler, target, source) = target_and_source("reentry-screen").await;
    copy_mode_with_source(&handler, &target, None).await;
    copy_mode_with_source(&handler, &target, Some(&source)).await;

    // tmux 3.7b: `l=3001`, the first line the target kept, not `SOURCE`.
    assert_eq!(
        line_after(&handler, &target, &["history-top"]).await,
        "copy-mode line 1\n"
    );
    // And `refresh-from-pane` still reads the target, not the ignored source.
    assert_eq!(
        line_after(&handler, &target, &["refresh-from-pane", "history-top"]).await,
        "copy-mode line 1\n"
    );
}

#[tokio::test]
async fn copy_mode_reentry_without_source_keeps_the_entry_source() {
    let (handler, target, source) = target_and_source("reentry-retarget").await;
    copy_mode_with_source(&handler, &target, Some(&source)).await;
    assert_eq!(
        line_after(&handler, &target, &["history-top"]).await,
        "copy-mode SOURCE\n"
    );
    copy_mode_with_source(&handler, &target, None).await;

    // tmux 3.7b: `refresh-from-pane` still reads the `-s` pane (`l=SOURCE`).
    assert_eq!(
        line_after(&handler, &target, &["refresh-from-pane", "history-top"]).await,
        "copy-mode SOURCE\n"
    );
}
