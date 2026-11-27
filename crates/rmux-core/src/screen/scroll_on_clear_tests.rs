//! Behaviour of the `scroll-on-clear` policy on whole-screen erases.
//!
//! Expectations are derived from the pinned tmux 3.7b oracle: only a whole
//! screen erase consults the policy, it transfers the used rows and not the
//! full viewport, and every neighbouring erase stays identical on both sides
//! of the switch.

use super::Screen;
use crate::grid::GridLineFlags;
use crate::input::InputParser;
use crate::{GridRenderOptions, ScreenCaptureRange};
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 12, rows: 4 };

fn screen_with(scroll_on_clear: bool, bytes: &[u8]) -> Screen {
    let mut screen = Screen::new(SIZE, 10);
    assert!(
        screen.scroll_on_clear(),
        "the constructed default is tmux's"
    );
    screen.set_scroll_on_clear(scroll_on_clear);
    feed(&mut screen, bytes);
    screen
}

fn feed(screen: &mut Screen, bytes: &[u8]) {
    let mut parser = InputParser::new();
    parser.parse(bytes, screen);
}

fn transcript(screen: &Screen) -> Vec<u8> {
    screen.capture_transcript(
        ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        },
        GridRenderOptions::default(),
    )
}

fn joined_transcript(screen: &Screen) -> Vec<u8> {
    screen.capture_transcript(
        ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        },
        GridRenderOptions {
            join_wrapped: true,
            ..GridRenderOptions::default()
        },
    )
}

#[test]
fn erase_display_with_scroll_on_clear_off_discards_used_visible_rows() {
    let screen = screen_with(false, b"$ printf\r\n$ clear-x\x1b[H\x1b[2J$");

    assert_eq!(screen.history_size(), 0);
    assert_eq!(transcript(&screen), b"$\n\n\n\n");
}

#[test]
fn erase_display_with_scroll_on_clear_on_keeps_the_measured_default() {
    let screen = screen_with(true, b"$ printf\r\n$ clear-x\x1b[H\x1b[2J$");

    assert_eq!(screen.history_size(), 2);
    assert_eq!(transcript(&screen), b"$ printf\n$ clear-x\n$\n\n\n\n");
}

#[test]
fn erase_to_end_from_home_with_scroll_on_clear_off_discards_used_visible_rows() {
    let screen = screen_with(false, b"$ printf\r\n$ clear-x\x1b[H\x1b[J$");

    assert_eq!(screen.history_size(), 0);
    assert_eq!(transcript(&screen), b"$\n\n\n\n");
}

#[test]
fn erase_to_end_away_from_home_ignores_scroll_on_clear() {
    for scroll_on_clear in [true, false] {
        let screen = screen_with(scroll_on_clear, b"one\r\ntwo\r\nthree\x1b[2;1H\x1b[J");

        assert_eq!(
            screen.history_size(),
            0,
            "scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            transcript(&screen),
            b"one\n\n\n\n",
            "scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn erase_start_of_screen_ignores_scroll_on_clear() {
    for scroll_on_clear in [true, false] {
        let screen = screen_with(scroll_on_clear, b"one\r\ntwo\r\nthree\x1b[2;4H\x1b[1J");

        assert_eq!(
            screen.history_size(),
            0,
            "scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            transcript(&screen),
            b"\n\nthree\n\n",
            "scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn clear_history_ignores_scroll_on_clear() {
    for scroll_on_clear in [true, false] {
        let mut screen = screen_with(scroll_on_clear, b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let before = screen.history_size();
        assert_eq!(before, 1, "scroll_on_clear={scroll_on_clear}");

        feed(&mut screen, b"\x1b[3J");

        assert_eq!(
            screen.history_size(),
            0,
            "scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            transcript(&screen),
            b"two\nthree\nfour\nfive\n",
            "scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn deccolm_follows_scroll_on_clear() {
    for (scroll_on_clear, expected_history) in [(true, 2), (false, 0)] {
        let mut screen = screen_with(scroll_on_clear, b"one\r\ntwo");

        feed(&mut screen, b"\x1b[?3h");

        assert_eq!(
            screen.history_size(),
            expected_history,
            "set, scroll_on_clear={scroll_on_clear}"
        );

        let mut screen = screen_with(scroll_on_clear, b"one\r\ntwo");

        feed(&mut screen, b"\x1b[?3l");

        assert_eq!(
            screen.history_size(),
            expected_history,
            "reset, scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn alternate_screen_erase_ignores_scroll_on_clear() {
    for scroll_on_clear in [true, false] {
        let mut screen = screen_with(scroll_on_clear, b"one\r\ntwo");
        assert_eq!(screen.history_size(), 0);

        feed(&mut screen, b"\x1b[?1049hALT\x1b[H\x1b[2J\x1b[?1049l");

        assert_eq!(
            screen.history_size(),
            0,
            "scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            transcript(&screen),
            b"one\ntwo\n\n\n",
            "scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn scroll_on_clear_off_preserves_existing_history() {
    let mut screen = screen_with(false, b"old1\r\nold2\r\nold3\r\nold4\r\nvis1\r\nvis2");
    assert_eq!(screen.history_size(), 2);

    feed(&mut screen, b"\x1b[H\x1b[2Jafter");

    assert_eq!(screen.history_size(), 2);
    assert_eq!(transcript(&screen), b"old1\nold2\nafter\n\n\n\n");
}

#[test]
fn scroll_on_clear_transfers_only_used_rows() {
    let mut screen = screen_with(true, b"one\r\ntwo");

    feed(&mut screen, b"\x1b[H\x1b[2J");

    assert_eq!(
        screen.history_size(),
        2,
        "the two blank rows are not stored"
    );
}

#[test]
fn scroll_on_clear_off_breaks_the_wrap_into_the_first_visible_row() {
    // The long line wraps, then scrolls so its head sits in scrollback while
    // its tail is still the first visible row.
    let mut screen = screen_with(false, b"abcdefghijklmnop\r\nb\r\nc\r\nd");
    assert_eq!(screen.history_size(), 1);
    assert_eq!(joined_transcript(&screen), b"abcdefghijklmnop\nb\nc\nd\n");

    feed(&mut screen, b"\x1b[H\x1b[2Jtail");

    assert_eq!(screen.history_size(), 1);
    assert_eq!(joined_transcript(&screen), b"abcdefghijkl\ntail\n\n\n\n");
    assert!(!screen
        .grid()
        .absolute_line(0)
        .expect("history row")
        .flags()
        .contains(GridLineFlags::WRAPPED));
}

#[test]
fn scroll_on_clear_is_re_readable_and_reversible_on_a_live_screen() {
    let mut screen = screen_with(true, b"one\r\ntwo");

    feed(&mut screen, b"\x1b[H\x1b[2Jthree\r\nfour");
    assert_eq!(screen.history_size(), 2);

    screen.set_scroll_on_clear(false);
    assert!(!screen.scroll_on_clear());
    feed(&mut screen, b"\x1b[H\x1b[2Jfive\r\nsix");
    assert_eq!(screen.history_size(), 2, "the erase discarded its rows");

    screen.set_scroll_on_clear(true);
    assert!(screen.scroll_on_clear());
    feed(&mut screen, b"\x1b[H\x1b[2Jseven");
    assert_eq!(
        screen.history_size(),
        4,
        "scrolling resumes without a respawn"
    );
    assert_eq!(transcript(&screen), b"one\ntwo\nfive\nsix\nseven\n\n\n\n");
}

/// Builds a principal screen holding `OLD1`/`OLD2` in history and
/// `VIS1`/`VIS2`/`VIS3` in the viewport, with the last visible row unused so
/// the transfer limit stays observable.
fn principal_with_history_and_used_rows(scroll_on_clear: bool) -> Screen {
    let screen = screen_with(scroll_on_clear, b"OLD1\r\nOLD2\r\nVIS1\r\nVIS2\r\nVIS3\r\n");

    assert_eq!(
        screen.history_size(),
        2,
        "OLD1/OLD2 are already history, scroll_on_clear={scroll_on_clear}"
    );
    assert_eq!(
        transcript(&screen),
        b"OLD1\nOLD2\nVIS1\nVIS2\nVIS3\n\n",
        "three used visible rows and one unused, scroll_on_clear={scroll_on_clear}"
    );
    screen
}

#[test]
fn reset_on_the_principal_screen_with_scroll_on_clear_on_transfers_used_rows() {
    // `ESC c` reaching the principal screen is a whole-screen erase, so tmux
    // 3.7b routes it through `screen_write_clearscreen` and the policy moves
    // the used visible rows to history instead of destroying them.
    let mut screen = principal_with_history_and_used_rows(true);

    feed(&mut screen, b"\x1bc");

    assert_eq!(
        screen.history_size(),
        5,
        "OLD1/OLD2 kept, VIS1/VIS2/VIS3 appended, the unused row not stored"
    );
    assert_eq!(
        transcript(&screen),
        b"OLD1\nOLD2\nVIS1\nVIS2\nVIS3\n\n\n\n\n",
        "the earlier history stays in order ahead of the transferred rows and \
         the viewport is empty"
    );
}

#[test]
fn reset_on_the_principal_screen_with_scroll_on_clear_off_discards_used_rows() {
    let mut screen = principal_with_history_and_used_rows(false);

    feed(&mut screen, b"\x1bc");

    assert_eq!(
        screen.history_size(),
        2,
        "the earlier history survives and nothing was appended"
    );
    assert_eq!(
        transcript(&screen),
        b"OLD1\nOLD2\n\n\n\n\n",
        "VIS1/VIS2/VIS3 are destroyed and the viewport is empty"
    );
}

#[test]
fn reset_entered_from_the_alternate_screen_keeps_the_principal_screen() {
    // R3510 M5, measured on tmux 3.7b: `screen_write_reset` does not leave
    // the alternate screen. It blanks the alternate grid, which keeps no
    // history, and the later `?1049l` restores the principal rows, their
    // history and the cursor `?1049h` saved, under either policy.
    for scroll_on_clear in [true, false] {
        let mut screen = principal_with_history_and_used_rows(scroll_on_clear);

        feed(&mut screen, b"\x1b[2;3H\x1b[?1049hALT1\r\nALT2");
        assert!(
            screen.is_alternate(),
            "the reset is entered from the alternate screen, \
             scroll_on_clear={scroll_on_clear}"
        );

        feed(&mut screen, b"\x1bcX");

        assert!(
            screen.is_alternate(),
            "the reset stays on the alternate screen, scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            (screen.cursor_x, screen.cursor_y),
            (1, 0),
            "the reset homes the alternate cursor, scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            transcript(&screen),
            b"OLD1\nOLD2\nX\n\n\n\n",
            "the alternate rows are blanked in place and nothing reaches history, \
             scroll_on_clear={scroll_on_clear}"
        );

        feed(&mut screen, b"\x1b[?1049lY");

        assert!(!screen.is_alternate(), "scroll_on_clear={scroll_on_clear}");
        assert_eq!(
            screen.history_size(),
            2,
            "the principal history survives, scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            transcript(&screen),
            b"OLD1\nOLD2\nVIS1\nVIY2\nVIS3\n\n",
            "the principal rows come back and `Y` lands on the saved cursor, \
             scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn reset_in_the_alternate_screen_matches_the_measured_r3510_m5_reproduction() {
    // `MAIN\r\nM2\e[?1049hALT\ecX\e[?1049lY` on tmux 3.7b: `X` stays on the
    // alternate screen until `?1049l`, then the capture reads `MAIN` / `M2Y`.
    let mut screen = screen_with(true, b"MAIN\r\nM2\x1b[?1049hALT\x1bcX");

    assert!(screen.is_alternate());
    assert_eq!(transcript(&screen), b"X\n\n\n\n");

    feed(&mut screen, b"\x1b[?1049lY");

    assert!(!screen.is_alternate());
    assert_eq!(screen.history_size(), 0);
    assert_eq!(transcript(&screen), b"MAIN\nM2Y\n\n\n");
    assert_eq!((screen.cursor_x, screen.cursor_y), (3, 1));
}

const LINKED_MAIN: &[u8] = b"\x1b]8;;https://example.com/main\x1b\\MAIN\x1b]8;;\x1b\\\r\n";

fn hyperlinks(screen: &Screen) -> Vec<u8> {
    screen.capture_hyperlinks(
        ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        },
        false,
    )
}

#[test]
fn reset_in_the_alternate_screen_keeps_the_principal_hyperlinks() {
    // R3630 M2, measured on tmux 3.7b: `capture-pane -p -H -S -` reads the
    // principal's URI after `?1049h`, RIS and `?1049l` under either policy,
    // exactly as the control without RIS does.
    for scroll_on_clear in [true, false] {
        for (sequence, label) in [
            (&b"\x1b[?1049hALT\x1bc\x1b[?1049l"[..], "with RIS"),
            (&b"\x1b[?1049hALT\x1b[?1049l"[..], "control without RIS"),
        ] {
            let mut screen = screen_with(scroll_on_clear, LINKED_MAIN);
            feed(&mut screen, sequence);

            assert!(!screen.is_alternate());
            assert_eq!(
                transcript(&screen),
                b"MAIN\n\n\n\n",
                "{label}, scroll_on_clear={scroll_on_clear}"
            );
            assert_eq!(
                hyperlinks(&screen),
                b"https://example.com/main\n",
                "{label}, scroll_on_clear={scroll_on_clear}"
            );
        }
    }
}

#[test]
fn reset_in_the_principal_screen_keeps_the_hyperlinks_of_transferred_rows() {
    // tmux 3.7b keeps the URI of the row `scroll-on-clear` moved to history;
    // with the policy off no cell references the link and the capture is
    // empty (`-p` prints only its line feed).
    for (scroll_on_clear, expected) in [
        (true, &b"https://example.com/main\n"[..]),
        (false, &b""[..]),
    ] {
        let mut screen = screen_with(scroll_on_clear, LINKED_MAIN);
        feed(&mut screen, b"\x1bc");

        assert_eq!(
            hyperlinks(&screen),
            expected,
            "scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn reset_with_scroll_on_clear_off_leaves_the_wrap_into_the_first_visible_row() {
    // The off branch deliberately keeps `Grid::clear_visible` rather than the
    // erase path's `clear_visible_without_history`, whose wrap break would be a
    // second change. Nothing else observes that choice, so pin it here: a swap
    // to the erase helper would clear this flag and fail.
    let mut screen = screen_with(false, b"abcdefghijklmnop\r\nb\r\nc\r\nd");
    assert_eq!(screen.history_size(), 1);
    assert!(screen
        .grid()
        .absolute_line(0)
        .expect("history row")
        .flags()
        .contains(GridLineFlags::WRAPPED));

    feed(&mut screen, b"\x1bc");

    assert_eq!(screen.history_size(), 1, "off transfers nothing");
    assert!(
        screen
            .grid()
            .absolute_line(0)
            .expect("history row")
            .flags()
            .contains(GridLineFlags::WRAPPED),
        "the reset must not break the wrap the erase path breaks"
    );
}

#[test]
fn reset_preserves_the_effective_scroll_on_clear_policy() {
    for (scroll_on_clear, expected_history) in [(true, 2), (false, 0)] {
        let mut screen = screen_with(scroll_on_clear, b"");

        feed(&mut screen, b"\x1bc");

        assert_eq!(
            screen.scroll_on_clear(),
            scroll_on_clear,
            "the reset does not return the option to its default"
        );
        assert_eq!(
            screen.history_size(),
            0,
            "an empty viewport transfers nothing, \
             scroll_on_clear={scroll_on_clear}"
        );

        feed(&mut screen, b"one\r\ntwo\x1b[H\x1b[2J");

        assert_eq!(
            screen.history_size(),
            expected_history,
            "the whole-screen erase after the reset still follows the policy, \
             scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn reset_still_restores_the_cursor_and_the_scroll_region() {
    for scroll_on_clear in [true, false] {
        let mut screen = principal_with_history_and_used_rows(scroll_on_clear);

        feed(&mut screen, b"\x1b[2;3r\x1b[3;5H");
        assert_eq!(
            (screen.rupper, screen.rlower),
            (1, 2),
            "the margins are set before the reset, \
             scroll_on_clear={scroll_on_clear}"
        );

        feed(&mut screen, b"\x1bc");

        assert_eq!(
            (screen.cursor_x, screen.cursor_y),
            (0, 0),
            "the cursor returns HOME, scroll_on_clear={scroll_on_clear}"
        );
        assert!(
            !screen.pending_wrap,
            "pending wrap is cleared, scroll_on_clear={scroll_on_clear}"
        );
        assert_eq!(
            (screen.rupper, screen.rlower),
            (0, u32::from(SIZE.rows) - 1),
            "the region is the full screen again, \
             scroll_on_clear={scroll_on_clear}"
        );
        assert!(
            !screen.is_alternate(),
            "the reset stays on the principal screen, \
             scroll_on_clear={scroll_on_clear}"
        );
    }
}

#[test]
fn recovery_projections_carry_the_policy() {
    let mut screen = screen_with(false, b"one\r\ntwo");

    let (viewport, _) = screen.clone_recovery_viewport_bounded(64, 64, 64, 64);
    assert!(!viewport.scroll_on_clear());
    let (projection, _) = screen.clone_recovery_projection_bounded(64, 64, 64, 64, 64);
    assert!(!projection.scroll_on_clear());

    screen.set_scroll_on_clear(true);

    let (viewport, _) = screen.clone_recovery_viewport_bounded(64, 64, 64, 64);
    assert!(viewport.scroll_on_clear());
    let (projection, _) = screen.clone_recovery_projection_bounded(64, 64, 64, 64, 64);
    assert!(projection.scroll_on_clear());
}
