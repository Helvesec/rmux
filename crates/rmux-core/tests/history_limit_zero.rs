//! `history-limit 0` collection contract, driven through the public
//! [`TerminalScreen`] facade with real terminal bytes.
//!
//! The retained rows follow tmux 3.7b: a pane built with a positive limit and
//! then moved to zero keeps the last row a clear or a scroll transferred, while
//! a pane built at zero never collects, even after a later positive limit.
//! Zero is not unlimited.

use rmux_core::{GridRenderOptions, ScreenCaptureRange, TerminalScreen};
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

const HOME: &[u8] = b"\x1b[H";
const ED2: &[u8] = b"\x1b[2J";

fn screen_with_limit(limit: usize) -> TerminalScreen {
    let mut terminal = TerminalScreen::new(SIZE, limit);
    terminal.set_scroll_on_clear(true);
    terminal
}

fn render_options() -> GridRenderOptions {
    GridRenderOptions {
        trim_spaces: true,
        ..GridRenderOptions::default()
    }
}

/// Returns the history rows as text, oldest first.
fn history_rows(terminal: &TerminalScreen) -> Vec<String> {
    let size = terminal.screen().history_size();
    if size == 0 {
        return Vec::new();
    }
    let start = -(i64::try_from(size).expect("history size fits i64"));
    terminal
        .screen()
        .capture_transcript_lines_independent(
            ScreenCaptureRange::new(Some(start), Some(-1)),
            render_options(),
        )
        .into_iter()
        .map(|row| String::from_utf8_lossy(&row).into_owned())
        .collect()
}

/// Returns the visible rows as text, top first.
fn visible_rows(terminal: &TerminalScreen) -> Vec<String> {
    terminal
        .screen()
        .capture_transcript_lines_independent(
            ScreenCaptureRange::new(Some(0), None),
            render_options(),
        )
        .into_iter()
        .map(|row| String::from_utf8_lossy(&row).into_owned())
        .collect()
}

fn feed_three_rows(terminal: &mut TerminalScreen, prefix: &str) {
    terminal.feed(format!("{prefix}1\r\n{prefix}2\r\n{prefix}3\r\n").as_bytes());
}

/// The pane is born with a positive limit, really moved to zero, and then
/// clears: tmux keeps the last transferred row, so `A3` survives and the
/// viewport is empty.
#[test]
fn hot_zero_clear_keeps_only_the_last_transferred_row() {
    let mut terminal = screen_with_limit(3);
    feed_three_rows(&mut terminal, "A");
    terminal.screen_mut().set_history_limit(0);
    assert_eq!(terminal.screen().history_limit(), 0);
    assert_eq!(terminal.screen().history_size(), 0);

    terminal.feed(HOME);
    terminal.feed(ED2);

    assert_eq!(history_rows(&terminal), vec!["A3".to_owned()]);
    assert!(
        visible_rows(&terminal).iter().all(|row| row.is_empty()),
        "the viewport is empty after the clear: {:?}",
        visible_rows(&terminal)
    );
}

/// A second clear at zero replaces the retained row instead of accumulating.
#[test]
fn a_second_hot_zero_clear_replaces_the_retained_row() {
    let mut terminal = screen_with_limit(3);
    feed_three_rows(&mut terminal, "A");
    terminal.screen_mut().set_history_limit(0);
    terminal.feed(HOME);
    terminal.feed(ED2);
    assert_eq!(history_rows(&terminal), vec!["A3".to_owned()]);

    feed_three_rows(&mut terminal, "B");
    terminal.feed(HOME);
    terminal.feed(ED2);

    assert_eq!(history_rows(&terminal), vec!["B3".to_owned()]);
    assert_eq!(terminal.screen().history_size(), 1, "zero is not unlimited");
}

/// A pane born at zero never collects, and a later positive limit does not
/// switch collection back on.
#[test]
fn a_pane_born_at_zero_stays_uncollected_through_a_positive_limit() {
    let mut terminal = screen_with_limit(0);
    feed_three_rows(&mut terminal, "A");
    terminal.feed(HOME);
    terminal.feed(ED2);
    assert_eq!(terminal.screen().history_size(), 0);

    terminal.screen_mut().set_history_limit(3);
    feed_three_rows(&mut terminal, "B");
    terminal.feed(HOME);
    terminal.feed(ED2);

    assert_eq!(terminal.screen().history_limit(), 3);
    assert_eq!(
        terminal.screen().history_size(),
        0,
        "a positive limit does not re-enable a pane born at zero"
    );
}

/// A pane born positive keeps collecting across positive to zero to positive,
/// and the positive bound applies again.
#[test]
fn a_pane_born_positive_keeps_collecting_through_zero_and_back() {
    let mut terminal = screen_with_limit(3);
    feed_three_rows(&mut terminal, "A");
    terminal.screen_mut().set_history_limit(0);
    terminal.feed(HOME);
    terminal.feed(ED2);
    assert_eq!(history_rows(&terminal), vec!["A3".to_owned()]);

    terminal.screen_mut().set_history_limit(3);
    feed_three_rows(&mut terminal, "B");
    terminal.feed(HOME);
    terminal.feed(ED2);

    assert_eq!(
        history_rows(&terminal),
        vec!["B1".to_owned(), "B2".to_owned(), "B3".to_owned()],
        "the positive bound applies again"
    );
}

/// The ordinary scroll shares the rule, and `scroll-on-clear off` does not stop
/// it: only the clear transfer is disabled.
#[test]
fn hot_zero_ordinary_scroll_retains_a_row_even_with_scroll_on_clear_off() {
    let mut terminal = screen_with_limit(3);
    terminal.set_scroll_on_clear(false);
    terminal.screen_mut().set_history_limit(0);

    // Rows `S0`..`S23` fill the 24-row viewport; the newline after the last one
    // scrolls the region once and hands `S0` over.
    for row in 0..24 {
        terminal.feed(format!("S{row}\r\n").as_bytes());
    }
    assert_eq!(history_rows(&terminal), vec!["S0".to_owned()]);

    // The next scroll replaces it rather than accumulating.
    terminal.feed(b"S24\r\n");
    assert_eq!(history_rows(&terminal), vec!["S1".to_owned()]);

    // With `scroll-on-clear off` the clear itself transfers nothing.
    terminal.feed(HOME);
    terminal.feed(ED2);
    assert_eq!(history_rows(&terminal), vec!["S1".to_owned()]);
}

/// Positive limits keep their current bounds; these are guards, not a new
/// conformance claim.
#[test]
fn positive_limits_of_one_and_three_keep_their_bounds() {
    let mut one = screen_with_limit(1);
    feed_three_rows(&mut one, "A");
    one.feed(HOME);
    one.feed(ED2);
    assert_eq!(history_rows(&one), vec!["A3".to_owned()]);

    let mut three = screen_with_limit(3);
    feed_three_rows(&mut three, "A");
    three.feed(HOME);
    three.feed(ED2);
    assert_eq!(
        history_rows(&three),
        vec!["A1".to_owned(), "A2".to_owned(), "A3".to_owned()]
    );
}

/// The alternate screen still saves and restores the collection flag on top of
/// the zero limit, and collection resumes at one row afterwards.
#[test]
fn alternate_screen_saves_and_restores_the_collection_flag_at_zero() {
    let mut terminal = screen_with_limit(3);
    terminal.set_alternate_screen_enabled(true);
    feed_three_rows(&mut terminal, "A");
    terminal.screen_mut().set_history_limit(0);
    terminal.feed(HOME);
    terminal.feed(ED2);
    assert_eq!(history_rows(&terminal), vec!["A3".to_owned()]);

    terminal.feed(b"\x1b[?1049h");
    feed_three_rows(&mut terminal, "B");
    terminal.feed(HOME);
    terminal.feed(ED2);
    assert_eq!(
        history_rows(&terminal),
        vec!["A3".to_owned()],
        "the alternate screen transfers nothing to history"
    );

    terminal.feed(b"\x1b[?1049l");
    feed_three_rows(&mut terminal, "C");
    terminal.feed(HOME);
    terminal.feed(ED2);

    assert_eq!(
        history_rows(&terminal),
        vec!["C3".to_owned()],
        "the restored flag collects again, still bounded to one row"
    );
}
