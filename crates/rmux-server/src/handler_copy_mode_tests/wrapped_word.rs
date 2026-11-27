//! `copy_cursor_word` for a word that a soft wrap splits across two rows.
//!
//! tmux 3.7b reads the word under the copy cursor with `format_grid_word`
//! (format.c:6001-6070): it walks back to a separator and forward to the next
//! one, crossing a row boundary only where the upper row is soft-wrapped, and
//! skipping padding cells. Every expected string below is derived from that
//! source and from the copy-mode motions of window-copy.c, never from rmux.

use super::*;

const SIZE: TerminalSize = TerminalSize { cols: 12, rows: 4 };
const CURSOR_REPORT: &str = "#{copy_cursor_x}|#{copy_cursor_y}|#{copy_cursor_word}";

/// `go abcdefghi` soft-wraps into `jKLM end`: the word is `abcdefghijKLM`.
const SOFT_WRAPPED: &[u8] = b"go abcdefghijKLM end\r\n";
/// The same cells, with a hard newline where the soft wrap was.
const HARD_NEWLINE: &[u8] = b"go abcdefghi\r\njKLM end\r\n";
/// A wide owner fills the last two cells of the soft-wrapped first row.
const WIDE_AT_WRAP: &str = "go abcdefg\u{4e2d}hi end\r\n";

async fn copy_mode_over(name: &str, content: &[u8]) -> (RequestHandler, PaneTarget) {
    let handler = RequestHandler::new();
    let target = create_session(&handler, name, SIZE).await;
    replace_transcript_contents(&handler, &target, SIZE, content).await;
    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    (handler, target)
}

async fn report_after(handler: &RequestHandler, target: &PaneTarget, motions: &[&str]) -> String {
    for motion in motions {
        assert!(matches!(
            send_copy_mode_command(handler, target, &[motion]).await,
            Response::SendKeys(_)
        ));
    }
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some(CURSOR_REPORT.to_owned()),
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

const FIRST_ROW_X5: &[&str] = &[
    "history-top",
    "cursor-right",
    "cursor-right",
    "cursor-right",
    "cursor-right",
    "cursor-right",
];
const CONTINUATION_X2: &[&str] = &["history-top", "cursor-down", "cursor-right", "cursor-right"];
const CONTINUATION_X1: &[&str] = &["history-top", "cursor-down", "cursor-right"];

#[tokio::test]
async fn copy_cursor_word_from_the_first_row_reads_the_word_across_its_soft_wrap() {
    let (handler, target) = copy_mode_over("cword-first", SOFT_WRAPPED).await;

    assert_eq!(
        report_after(&handler, &target, FIRST_ROW_X5).await,
        "5|0|abcdefghijKLM\n"
    );
}

#[tokio::test]
async fn copy_cursor_word_from_the_continuation_row_reads_the_word_across_its_soft_wrap() {
    let (handler, target) = copy_mode_over("cword-continuation", SOFT_WRAPPED).await;

    assert_eq!(
        report_after(&handler, &target, CONTINUATION_X2).await,
        "2|1|abcdefghijKLM\n"
    );
}

#[tokio::test]
async fn copy_cursor_word_stops_at_a_hard_newline_between_the_same_cells() {
    let (handler, target) = copy_mode_over("cword-hard", HARD_NEWLINE).await;

    assert_eq!(
        report_after(&handler, &target, FIRST_ROW_X5).await,
        "5|0|abcdefghi\n"
    );
    assert_eq!(
        report_after(&handler, &target, CONTINUATION_X2).await,
        "2|1|jKLM\n"
    );
}

#[tokio::test]
async fn copy_cursor_word_emits_a_wide_owner_ending_the_wrapped_row_once() {
    let (handler, target) = copy_mode_over("cword-wide", WIDE_AT_WRAP.as_bytes()).await;
    let onto_the_wide_owner = [&["history-top"][..], &["cursor-right"; 10][..]].concat();

    assert_eq!(
        report_after(&handler, &target, &onto_the_wide_owner).await,
        "10|0|abcdefg\u{4e2d}hi\n"
    );
    assert_eq!(
        report_after(&handler, &target, CONTINUATION_X1).await,
        "1|1|abcdefg\u{4e2d}hi\n"
    );
}
