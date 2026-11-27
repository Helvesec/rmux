//! Behaviour of the `allow-rename` policy on `ESC k` window-rename requests.
//!
//! Expectations are derived from the pinned tmux 3.7b sources and measured
//! against the pinned binary in `arn1/oracle`: the option gates the
//! request, titles keep their own policy, invalid names are dropped whole, and
//! every accepted request survives as a request — including an empty one and a
//! repeat, which change no stored name.

use super::Screen;
use crate::input::InputParser;
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 4 };

fn screen_with(allow_rename: bool) -> Screen {
    let mut screen = Screen::new(SIZE, 10);
    assert!(
        !screen.allow_rename(),
        "the constructed default is tmux's off"
    );
    screen.set_allow_rename(allow_rename);
    screen
}

fn feed(screen: &mut Screen, bytes: &[u8]) {
    let mut parser = InputParser::new();
    parser.parse(bytes, screen);
}

#[test]
fn a_rename_request_is_refused_while_allow_rename_is_off() {
    let mut screen = screen_with(false);
    let revision_before = screen.metadata_revision();

    feed(&mut screen, b"\x1bkdenied\x1b\\");

    assert!(screen.take_window_rename_requests().is_empty());
    assert_eq!(screen.metadata_revision(), revision_before);
}

#[test]
fn a_rename_request_is_accepted_and_ordered_while_allow_rename_is_on() {
    let mut screen = screen_with(true);

    feed(&mut screen, b"\x1bkfirst\x1b\\\x1bksecond\x1b\\");

    assert_eq!(
        screen.take_window_rename_requests(),
        vec!["first".to_owned(), "second".to_owned()],
        "both requests of the same chunk survive, in order"
    );
    assert!(
        screen.take_window_rename_requests().is_empty(),
        "draining is exhaustive"
    );
}

#[test]
fn a_repeated_identical_name_is_a_request_of_its_own() {
    let mut screen = screen_with(true);

    feed(&mut screen, b"\x1bksame\x1b\\");
    feed(&mut screen, b"\x1bksame\x1b\\");

    assert_eq!(
        screen.take_window_rename_requests(),
        vec!["same".to_owned(), "same".to_owned()],
        "tmux notifies for each cleaned name, repetition included"
    );
}

#[test]
fn an_empty_request_is_kept_even_as_the_very_first_one() {
    let mut screen = screen_with(true);

    feed(&mut screen, b"\x1bk\x1b\\");

    assert_eq!(
        screen.take_window_rename_requests(),
        vec![String::new()],
        "an empty name is an option-reset request, not an absence of request"
    );
}

#[test]
fn an_invalid_utf8_name_is_dropped_whole() {
    let mut screen = screen_with(true);

    feed(&mut screen, b"\x1bkbad\xffname\x1b\\");

    assert!(
        screen.take_window_rename_requests().is_empty(),
        "tmux checks utf8_isvalid before touching the window, and never renames \
         to replacement characters"
    );
}

#[test]
fn a_delete_byte_is_not_a_printable_rename_character() {
    let mut screen = screen_with(true);

    feed(&mut screen, b"\x1bkwith\x7fdel\x1b\\");

    assert!(
        screen.take_window_rename_requests().is_empty(),
        "utf8_isvalid accepts 0x20..=0x7e for a single-byte character"
    );
}

#[test]
fn a_valid_multi_byte_name_is_accepted_verbatim() {
    let mut screen = screen_with(true);

    feed(&mut screen, "\x1bkécran-é\x1b\\".as_bytes());

    assert_eq!(
        screen.take_window_rename_requests(),
        vec!["écran-é".to_owned()]
    );
}

#[test]
fn allow_set_title_does_not_gate_a_rename_and_allow_rename_does_not_gate_a_title() {
    let mut screen = screen_with(true);
    screen.set_title_rename_enabled(false);

    feed(&mut screen, b"\x1bkrenamed\x1b\\\x1b]2;blocked\x1b\\");

    assert_eq!(
        screen.take_window_rename_requests(),
        vec!["renamed".to_owned()],
        "the rename obeys allow-rename alone"
    );
    assert_eq!(screen.title(), "", "the title obeys allow-set-title alone");

    let mut screen = screen_with(false);
    screen.set_title_rename_enabled(true);

    feed(&mut screen, b"\x1bkrefused\x1b\\\x1b]2;accepted\x1b\\");

    assert!(screen.take_window_rename_requests().is_empty());
    assert_eq!(screen.title(), "accepted");
}

#[test]
fn the_policy_is_re_readable_and_reversible_on_a_live_screen() {
    let mut screen = screen_with(false);

    feed(&mut screen, b"\x1bkwhile-off\x1b\\");
    assert!(screen.take_window_rename_requests().is_empty());

    screen.set_allow_rename(true);
    assert!(screen.allow_rename());
    feed(&mut screen, b"\x1bkwhile-on\x1b\\");
    assert_eq!(
        screen.take_window_rename_requests(),
        vec!["while-on".to_owned()],
        "the switch takes effect without a respawn"
    );

    screen.set_allow_rename(false);
    assert!(!screen.allow_rename());
    feed(&mut screen, b"\x1bkoff-again\x1b\\");
    assert!(screen.take_window_rename_requests().is_empty());
}

#[test]
fn recovery_projections_carry_the_policy() {
    let mut screen = screen_with(true);

    let (viewport, _) = screen.clone_recovery_viewport_bounded(64, 64, 64, 64);
    assert!(viewport.allow_rename());
    let (projection, _) = screen.clone_recovery_projection_bounded(64, 64, 64, 64, 64);
    assert!(projection.allow_rename());

    screen.set_allow_rename(false);

    let (viewport, _) = screen.clone_recovery_viewport_bounded(64, 64, 64, 64);
    assert!(!viewport.allow_rename());
    let (projection, _) = screen.clone_recovery_projection_bounded(64, 64, 64, 64, 64);
    assert!(!projection.allow_rename());
}
