//! `search_present` and `search_match` follow the live search marks.
//!
//! tmux 3.7b reports `search_present` as `searchmark != NULL` and reads
//! `search_match` from the mark under the copy cursor (window-copy.c:1025,
//! 4629). A non-search command clears the marks when its clear policy applies
//! (window-copy.c:3505-3514): cursor-right clears them in Emacs and keeps them
//! in vi, and a search that finds nothing never creates them (window-copy.c:
//! 4292-4295). An incremental search takes the first character of its argument
//! as the prefix and searches the rest literally (window-copy.c:2680-2785).
//! Every expected string below is derived from that source, never from rmux.

use super::*;

const SIZE: TerminalSize = TerminalSize { cols: 40, rows: 4 };
const CONTENT: &[u8] = b"alpha SMARKa0123 omega\r\n";
const BOTH_FIELDS: &str = "#{search_present}|#{search_match}";
const ROW_AND_MARKS: &str = "#{copy_cursor_y}|#{search_present}|#{search_match}";
const MARKS_AND_TEXT: &str = "#{search_present}|#{search_match}|#{pane_search_string}";

async fn copy_mode_with_keys(name: &str, mode_keys: &str) -> (RequestHandler, PaneTarget) {
    copy_mode_over(name, mode_keys, CONTENT).await
}

async fn copy_mode_over(
    name: &str,
    mode_keys: &str,
    content: &[u8],
) -> (RequestHandler, PaneTarget) {
    let handler = RequestHandler::new();
    let target = create_session(&handler, name, SIZE).await;
    replace_transcript_contents(&handler, &target, SIZE, content).await;
    set_mode_keys(&handler, &target, mode_keys).await;
    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    (handler, target)
}

async fn set_mode_keys(handler: &RequestHandler, target: &PaneTarget, value: &str) {
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(
                    target.session_name().clone(),
                    0,
                )),
                option: OptionName::ModeKeys,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
}

async fn run(handler: &RequestHandler, target: &PaneTarget, command: &[&str]) {
    assert!(matches!(
        send_copy_mode_command(handler, target, command).await,
        Response::SendKeys(_)
    ));
}

async fn report(handler: &RequestHandler, target: &PaneTarget, format: &str) -> String {
    let listed = handler
        .handle(Request::ListPanes(Box::new(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some(format.to_owned()),
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

#[tokio::test]
async fn search_present_clears_after_emacs_cursor_motion() {
    let (handler, target) = copy_mode_with_keys("smark-present", "emacs").await;

    run(&handler, &target, &["search-forward-text", "SMARKa"]).await;
    assert_eq!(report(&handler, &target, "#{search_present}").await, "1\n");

    run(&handler, &target, &["cursor-right"]).await;
    assert_eq!(report(&handler, &target, "#{search_present}").await, "0\n");
}

#[tokio::test]
async fn search_match_clears_after_emacs_cursor_motion() {
    let (handler, target) = copy_mode_with_keys("smark-match", "emacs").await;

    run(&handler, &target, &["search-forward-text", "SMARKa"]).await;
    assert_eq!(
        report(&handler, &target, "[#{search_match}]").await,
        "[SMARKa]\n"
    );

    run(&handler, &target, &["cursor-right"]).await;
    assert_eq!(report(&handler, &target, "[#{search_match}]").await, "[]\n");
}

#[tokio::test]
async fn search_marks_remain_after_vi_cursor_motion() {
    let (handler, target) = copy_mode_with_keys("smark-vi", "vi").await;

    run(&handler, &target, &["search-forward-text", "SMARKa"]).await;
    run(&handler, &target, &["cursor-right"]).await;

    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "1|SMARKa\n");
}

#[tokio::test]
async fn search_formats_reappear_after_a_new_explicit_search() {
    let (handler, target) = copy_mode_with_keys("smark-again", "emacs").await;

    run(&handler, &target, &["search-forward-text", "SMARKa"]).await;
    run(&handler, &target, &["cursor-right"]).await;
    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "0|\n");

    run(&handler, &target, &["search-forward-text", "SMARKa"]).await;
    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "1|SMARKa\n");
}

#[tokio::test]
async fn search_present_is_false_after_a_fresh_search_without_matches() {
    let (handler, target) = copy_mode_with_keys("smark-miss", "emacs").await;

    run(&handler, &target, &["search-forward-text", "NOSUCHMARK"]).await;

    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "0|\n");
}

#[tokio::test]
async fn search_formats_are_absent_outside_copy_mode() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "smark-outside", SIZE).await;
    replace_transcript_contents(&handler, &target, SIZE, CONTENT).await;

    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "|\n");
}

const FORWARD: &str = "search-forward-incremental";
const BACKWARD: &str = "search-backward-incremental";
const EXPLICIT_UP: &[&str] = &["search-backward-text", "KEY"];
const EXPLICIT_DOWN: &[&str] = &["search-forward-text", "KEY"];

async fn incremental(handler: &RequestHandler, target: &PaneTarget, command: &str, arg: &str) {
    run(handler, target, &[command, "--", arg]).await;
}

/// history-top then cursor-down: the copy cursor at (0,1), between two KEYs.
async fn between_the_two_keys(handler: &RequestHandler, target: &PaneTarget) {
    run(handler, target, &["history-top"]).await;
    run(handler, target, &["cursor-down"]).await;
}

#[tokio::test]
async fn incremental_prefix_chooses_the_direction_of_its_command() {
    let content = b"KEY above\r\nmiddle line\r\nKEY below\r\n";
    let (handler, target) = copy_mode_over("smark-direction", "emacs", content).await;
    // (an explicit search run first, the command, its argument, the report)
    let cases: &[(&[&str], &str, &str, &str)] = &[
        (&[], FORWARD, "+KEY", "2|1|KEY\n"),
        (&[], FORWARD, "-KEY", "0|1|KEY\n"),
        (&[], BACKWARD, "+KEY", "2|1|KEY\n"),
        (&[], BACKWARD, "-KEY", "0|1|KEY\n"),
        (EXPLICIT_UP, FORWARD, "=KEY", "2|1|KEY\n"),
        (EXPLICIT_DOWN, BACKWARD, "=KEY", "0|1|KEY\n"),
    ];
    for (previous, command, arg, expected) in cases {
        if !previous.is_empty() {
            between_the_two_keys(&handler, &target).await;
            run(&handler, &target, previous).await;
        }
        between_the_two_keys(&handler, &target).await;
        incremental(&handler, &target, command, arg).await;
        assert_eq!(
            report(&handler, &target, ROW_AND_MARKS).await,
            *expected,
            "{command} {arg} after {previous:?}"
        );
    }
}

#[tokio::test]
async fn incremental_remainder_is_searched_literally() {
    let content = b"abc then a.c then :alpha\r\n";
    let (handler, target) = copy_mode_over("smark-literal", "emacs", content).await;

    run(&handler, &target, &["history-top"]).await;
    incremental(&handler, &target, FORWARD, "+:alpha").await;
    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "1|:alpha\n");

    run(&handler, &target, &["history-top"]).await;
    incremental(&handler, &target, FORWARD, "+a.c").await;
    assert_eq!(report(&handler, &target, BOTH_FIELDS).await, "1|a.c\n");

    // An unknown prefix, ASCII or not, runs no search and keeps the marks.
    for unknown in ["?zzz", "\u{e9}KEY", "\u{e9}"] {
        incremental(&handler, &target, FORWARD, unknown).await;
        let present = report(&handler, &target, "#{search_present}").await;
        assert_eq!(present, "1\n", "{unknown:?}");
    }
}

#[tokio::test]
async fn incremental_empty_or_missed_remainder_clears_the_marks() {
    let (handler, target) = copy_mode_over("smark-cleared", "emacs", b"KEY here\r\n").await;
    run(&handler, &target, &["history-top"]).await;

    let steps = [
        ("+KEY", "1|KEY|KEY\n"),
        ("+", "0||KEY\n"),
        ("+KEY", "1|KEY|KEY\n"),
        ("+NOSUCH", "0||NOSUCH\n"),
    ];
    for (arg, expected) in steps {
        incremental(&handler, &target, FORWARD, arg).await;
        let marks = report(&handler, &target, MARKS_AND_TEXT).await;
        assert_eq!(marks, expected, "{arg:?}");
    }
}
