//! `search_match` reads the search mark under the copy cursor.
//!
//! tmux 3.7b marks every cell of each match with a generation of its own and
//! answers `search_match` from the mark AT the cursor cell; only when that cell
//! is unmarked does it read the cell just before it (window-copy.c:4397-4423,
//! 4629-4674). The retained current result decides nothing. Every expected
//! string below is derived from that source and from the vi cursor motions of
//! grid-reader.c, never from rmux.

use super::*;

const SIZE: TerminalSize = TerminalSize { cols: 40, rows: 4 };
const MARKS: &str = "#{search_present}|#{search_match}";

async fn copy_mode_over(
    name: &str,
    mode_keys: &str,
    content: &[u8],
) -> (RequestHandler, PaneTarget) {
    let handler = RequestHandler::new();
    let target = create_session(&handler, name, SIZE).await;
    replace_transcript_contents(&handler, &target, SIZE, content).await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(
                    target.session_name().clone(),
                    0,
                )),
                option: OptionName::ModeKeys,
                value: mode_keys.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    (handler, target)
}

/// Runs `command` `times` times, then reports both search fields.
async fn marks_after(
    handler: &RequestHandler,
    target: &PaneTarget,
    command: &[&str],
    times: usize,
) -> String {
    for _ in 0..times {
        assert!(matches!(
            send_copy_mode_command(handler, target, command).await,
            Response::SendKeys(_)
        ));
    }
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some(MARKS.to_owned()),
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

const SMARK: &[&str] = &["search-forward-text", "SMARKa"];
const RIGHT: &[&str] = &["cursor-right"];
const LEFT: &[&str] = &["cursor-left"];

#[tokio::test]
async fn search_match_is_empty_beyond_the_one_after_position() {
    let (handler, target) = copy_mode_over("hit-beyond", "vi", b"SMARKa0123 tail\r\n").await;

    assert_eq!(marks_after(&handler, &target, SMARK, 1).await, "1|SMARKa\n");
    assert_eq!(marks_after(&handler, &target, RIGHT, 7).await, "1|\n");
}

#[tokio::test]
async fn search_match_reads_the_match_under_cursor_not_retained_index() {
    let (handler, target) = copy_mode_over("hit-second", "vi", b"HIT1HIT2 tail\r\n").await;
    let regex = &["search-forward", "HIT[12]"];

    assert_eq!(marks_after(&handler, &target, regex, 1).await, "1|HIT1\n");
    assert_eq!(marks_after(&handler, &target, RIGHT, 4).await, "1|HIT2\n");
}

#[tokio::test]
async fn search_match_allows_inside_and_one_after() {
    let (handler, target) = copy_mode_over("hit-inside", "vi", b"SMARKa0123 tail\r\n").await;

    assert_eq!(marks_after(&handler, &target, SMARK, 1).await, "1|SMARKa\n");
    // x3 inside, x6 one after, x7 beyond, then x5 inside again.
    assert_eq!(marks_after(&handler, &target, RIGHT, 3).await, "1|SMARKa\n");
    assert_eq!(marks_after(&handler, &target, RIGHT, 3).await, "1|SMARKa\n");
    assert_eq!(marks_after(&handler, &target, RIGHT, 1).await, "1|\n");
    assert_eq!(marks_after(&handler, &target, LEFT, 2).await, "1|SMARKa\n");
}

#[tokio::test]
async fn search_match_respects_row_and_cleared_marks() {
    let content = b"SMARKa0123\r\nother row\r\n";
    let (handler, target) = copy_mode_over("hit-row", "vi", content).await;
    assert_eq!(marks_after(&handler, &target, SMARK, 1).await, "1|SMARKa\n");
    let down = &["cursor-down"];
    assert_eq!(marks_after(&handler, &target, down, 1).await, "1|\n");

    let (handler, target) = copy_mode_over("hit-emacs", "emacs", content).await;
    assert_eq!(marks_after(&handler, &target, SMARK, 1).await, "1|SMARKa\n");
    assert_eq!(marks_after(&handler, &target, RIGHT, 1).await, "0|\n");
}

#[tokio::test]
async fn search_match_preserves_wide_owner_extent() {
    // b at 1, the wide owner at 2 with its padding at 3, c at 4, d at 5.
    let content = "ab\u{4e2d}cd tail\r\n".as_bytes();
    let (handler, target) = copy_mode_over("hit-wide", "vi", content).await;
    let wide = &["search-forward-text", "b\u{4e2d}"];

    assert_eq!(
        marks_after(&handler, &target, wide, 1).await,
        "1|b\u{4e2d}\n"
    );
    // 2 the wide owner, 4 one after its padding, 5 beyond.
    assert_eq!(
        marks_after(&handler, &target, RIGHT, 1).await,
        "1|b\u{4e2d}\n"
    );
    assert_eq!(
        marks_after(&handler, &target, RIGHT, 1).await,
        "1|b\u{4e2d}\n"
    );
    assert_eq!(marks_after(&handler, &target, RIGHT, 1).await, "1|\n");
}
