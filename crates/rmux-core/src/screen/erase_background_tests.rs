//! Whole-screen erases paint the active SGR background.
//!
//! Four entries reach `Screen::clear_whole_screen`: `ED 2` at any cursor
//! position, `ED 0` issued at the home cell, and DECCOLM set (`CSI ?3h`) and
//! reset (`CSI ?3l`). tmux 3.7b forwards the background the parser holds down
//! to the grid primitives on all of them (`input.c:1718-1725`,
//! `input.c:1913-1915`, `input.c:2012-2014`, `screen-write.c:1792-1802` and
//! `screen-write.c:1943-1951`, in both the scroll-to-history branch and the
//! in-place branch), so the erased viewport takes the active background on
//! either side of the `scroll-on-clear` policy.
//!
//! Every case drives a real `InputParser` that stays alive across the chunks
//! of that case: the background these erases must use is parser state, so a
//! second parser would not carry it.
//!
//! The rows that already sit in scrollback, and the used rows this erase moves
//! there, keep the styles they were written with. Only the viewport is
//! repainted.

use super::Screen;
use crate::grid::{GridCell, GridLine};
use crate::input::{
    colour_join_rgb, Colour, GridAttr, InputParser, COLOUR_DEFAULT, COLOUR_FLAG_256,
};
use crate::{GridRenderOptions, ScreenCaptureRange};
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 12, rows: 4 };
const HISTORY_LIMIT: usize = 10;

/// `SGR 1;7;36;42` plus an OSC 8 hyperlink: the style the probe glyph is
/// written with, and the style its row must still carry once it is scrollback.
const WITNESS_STYLE: &[u8] = b"\x1b[1;7;36;42m\x1b]8;;https://example.invalid/witness\x1b\\";
const WITNESS_FG: Colour = 6;
const WITNESS_BG: Colour = 2;
const WITNESS_ATTR: u16 = GridAttr::BRIGHT | GridAttr::REVERSE;

/// `SGR 33;41`: changes only the colours, so bold, inverse and the hyperlink
/// are still active when the erase arrives and must not reach the erased
/// cells.
const ERASE_STYLE: &[u8] = b"\x1b[33;41m";
const ERASE_FG: Colour = 3;
const ERASE_BG: Colour = 1;

/// The erase style must differ from the default in every channel the erased
/// cells are checked against, or those checks would not discriminate.
const _: () = assert!(
    ERASE_FG != COLOUR_DEFAULT
        && ERASE_BG != COLOUR_DEFAULT
        && WITNESS_BG != ERASE_BG
        && WITNESS_ATTR != 0
);

/// Drops every attribute and the hyperlink, back to the parser defaults.
const PLAIN_STYLE: &[u8] = b"\x1b[m\x1b]8;;\x1b\\";

fn cols() -> u32 {
    u32::from(SIZE.cols)
}

fn rows() -> u32 {
    u32::from(SIZE.rows)
}

/// One whole-screen erase entry, with the cursor it must leave behind.
#[derive(Clone, Copy)]
struct Entry {
    name: &'static str,
    bytes: &'static [u8],
    cursor_after: (u32, u32),
}

const ED2_FROM_HOME: Entry = Entry {
    name: "ED 2 from home",
    bytes: b"\x1b[H\x1b[2J",
    cursor_after: (0, 0),
};

const ED2_AWAY_FROM_HOME: Entry = Entry {
    name: "ED 2 away from home",
    bytes: b"\x1b[3;5H\x1b[2J",
    cursor_after: (4, 2),
};

const ED0_AT_HOME: Entry = Entry {
    name: "ED 0 at home",
    bytes: b"\x1b[H\x1b[J",
    cursor_after: (0, 0),
};

const DECCOLM_SET: Entry = Entry {
    name: "DECCOLM set",
    bytes: b"\x1b[?3h",
    cursor_after: (0, 0),
};

const DECCOLM_RESET: Entry = Entry {
    name: "DECCOLM reset",
    bytes: b"\x1b[?3l",
    cursor_after: (0, 0),
};

/// A screen and the parser feeding it, kept together so a case can hand the
/// bytes over in several chunks without losing the SGR state.
struct Case {
    screen: Screen,
    parser: InputParser,
}

impl Case {
    fn new(scroll_on_clear: bool) -> Self {
        let mut screen = Screen::new(SIZE, HISTORY_LIMIT);
        screen.set_scroll_on_clear(scroll_on_clear);
        let mut case = Self {
            screen,
            parser: InputParser::new(),
        };
        case.feed(b"OLD1\r\nOLD2\r\nVIS1\r\nVIS2\r\nVIS3\r\n");
        assert_eq!(
            case.screen.history_size(),
            2,
            "OLD1/OLD2 start in scrollback, scroll_on_clear={scroll_on_clear}"
        );
        case
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.parse(bytes, &mut self.screen);
    }
}

/// Builds the fixture and leaves the witness style active: `VIS1`/`VIS2`/`VIS3`
/// in the default style and a styled `Z` on the last visible row, so the erase
/// has four used rows to move and one of them carries a background, a
/// foreground, two attributes and a hyperlink of its own.
fn styled_case(scroll_on_clear: bool) -> Case {
    let mut case = Case::new(scroll_on_clear);
    case.feed(WITNESS_STYLE);
    case.feed(b"\x1b[4;1HZ");

    let probe = facts(
        &case
            .screen
            .grid()
            .visible_line(3)
            .expect("probe row")
            .cell(0)
            .expect("probe column"),
    );
    assert_eq!(
        probe.text, "Z",
        "the probe glyph reached the grid, scroll_on_clear={scroll_on_clear}"
    );
    assert_eq!(
        (probe.fg, probe.bg, probe.attr),
        (WITNESS_FG, WITNESS_BG, WITNESS_ATTR),
        "the witness style is live in the parser, scroll_on_clear={scroll_on_clear}"
    );
    assert_ne!(
        probe.link, 0,
        "the OSC 8 hyperlink is live in the parser, scroll_on_clear={scroll_on_clear}"
    );
    case
}

/// The cell facts this lot is about: the glyph, its width, and every style
/// channel an erase must either set or drop.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CellFacts {
    text: String,
    width: u8,
    fg: Colour,
    bg: Colour,
    attr: u16,
    link: u32,
}

fn facts(cell: &GridCell) -> CellFacts {
    CellFacts {
        text: cell.text().to_owned(),
        width: cell.width(),
        fg: cell.fg(),
        bg: cell.bg(),
        attr: cell.attr(),
        link: cell.link(),
    }
}

/// Reads every column of a row. A column the line does not materialise is an
/// unused default blank, which is what `GridCell::default()` is.
fn row_facts(line: &GridLine) -> Vec<CellFacts> {
    (0..cols())
        .map(|x| facts(&line.cell(x).unwrap_or_default()))
        .collect()
}

fn visible_row_facts(screen: &Screen, y: u32) -> Vec<CellFacts> {
    row_facts(screen.grid().visible_line(y).expect("visible row"))
}

fn history_facts(screen: &Screen) -> Vec<Vec<CellFacts>> {
    (0..screen.history_size())
        .map(|absolute| {
            row_facts(
                screen
                    .grid()
                    .absolute_line(absolute)
                    .expect("scrollback row"),
            )
        })
        .collect()
}

fn viewport_facts(screen: &Screen) -> Vec<Vec<CellFacts>> {
    (0..rows()).map(|y| visible_row_facts(screen, y)).collect()
}

/// The row an erase with `bg` must leave: spaces of width 1 carrying `bg` and
/// nothing else.
fn erased_row(bg: Colour) -> Vec<CellFacts> {
    (0..cols())
        .map(|_| CellFacts {
            text: " ".to_owned(),
            width: 1,
            fg: COLOUR_DEFAULT,
            bg,
            attr: 0,
            link: 0,
        })
        .collect()
}

/// Asserts every erased cell of the viewport, and the storage encoding of the
/// rows: a default background stays the compact plain-text line, a non-default
/// one materialises the whole width.
fn assert_viewport_erased(screen: &Screen, bg: Colour, context: &str) {
    let expected = erased_row(bg);
    for y in 0..rows() {
        assert_eq!(
            visible_row_facts(screen, y),
            expected,
            "{context}: erased viewport row {y}"
        );
        let line = screen.grid().visible_line(y).expect("visible row");
        if bg == COLOUR_DEFAULT {
            assert!(
                line.cells().is_empty(),
                "{context}: default-background row {y} keeps the compact encoding"
            );
        } else {
            assert_eq!(
                line.cells().len(),
                cols() as usize,
                "{context}: coloured row {y} materialises every cell"
            );
        }
    }
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

/// Drives one entry under one policy value and checks the whole contract:
/// every erased cell takes the active background and drops the active
/// foreground, attributes and hyperlink; the pre-existing scrollback and the
/// rows this erase transfers keep their own styles; the cursor, the geometry
/// and the policy are where they were.
fn run_entry(entry: Entry, scroll_on_clear: bool) {
    let context = format!("{} scroll_on_clear={scroll_on_clear}", entry.name);
    let mut case = styled_case(scroll_on_clear);
    case.feed(ERASE_STYLE);
    case.feed(b"\x1b[4;3HY");

    // The erase style is live in the parser at this point, in its own chunk:
    // a non-default foreground, a non-default background, two attributes and
    // the hyperlink the witness chunk opened.
    let live = facts(
        &case
            .screen
            .grid()
            .visible_line(3)
            .expect("probe row")
            .cell(2)
            .expect("probe column"),
    );
    assert_eq!(
        (live.text.as_str(), live.fg, live.bg, live.attr),
        ("Y", ERASE_FG, ERASE_BG, WITNESS_ATTR),
        "{context}: the erase style is live when the erase arrives"
    );
    assert_ne!(live.link, 0, "{context}: the hyperlink is still live");

    let history_before = history_facts(&case.screen);
    let viewport_before = viewport_facts(&case.screen);
    assert_eq!(history_before.len(), 2, "{context}: OLD1/OLD2 only");

    case.feed(entry.bytes);

    assert_viewport_erased(&case.screen, ERASE_BG, &context);

    let expected_history = if scroll_on_clear {
        history_before
            .iter()
            .chain(viewport_before.iter())
            .cloned()
            .collect()
    } else {
        history_before
    };
    assert_eq!(
        case.screen.history_size(),
        expected_history.len(),
        "{context}: the policy decides the transfer, the background does not"
    );
    assert_eq!(
        history_facts(&case.screen),
        expected_history,
        "{context}: scrollback keeps its own text and styles, unrecoloured"
    );

    assert_eq!(
        (case.screen.cursor_x, case.screen.cursor_y),
        entry.cursor_after,
        "{context}: cursor"
    );
    assert_eq!(case.screen.size(), SIZE, "{context}: geometry");
    assert_eq!(
        case.screen.scroll_on_clear(),
        scroll_on_clear,
        "{context}: the policy value survives"
    );
}

// --- The four entries, each on both sides of the policy. ----------------
//
// Each of these ten tests fails without the background forwarded down to the
// grid primitives: the erased viewport keeps `COLOUR_DEFAULT` instead of the
// red the parser holds, so the `assert_viewport_erased` comparison
// discriminates independently with `scroll-on-clear` on and with it off.

#[test]
fn ed2_from_home_with_scroll_on_clear_on_paints_the_active_background() {
    run_entry(ED2_FROM_HOME, true);
}

#[test]
fn ed2_from_home_with_scroll_on_clear_off_paints_the_active_background() {
    run_entry(ED2_FROM_HOME, false);
}

#[test]
fn ed2_away_from_home_with_scroll_on_clear_on_paints_the_active_background() {
    run_entry(ED2_AWAY_FROM_HOME, true);
}

#[test]
fn ed2_away_from_home_with_scroll_on_clear_off_paints_the_active_background() {
    run_entry(ED2_AWAY_FROM_HOME, false);
}

#[test]
fn ed0_at_home_with_scroll_on_clear_on_paints_the_active_background() {
    run_entry(ED0_AT_HOME, true);
}

#[test]
fn ed0_at_home_with_scroll_on_clear_off_paints_the_active_background() {
    run_entry(ED0_AT_HOME, false);
}

#[test]
fn deccolm_set_with_scroll_on_clear_on_paints_the_active_background() {
    run_entry(DECCOLM_SET, true);
}

#[test]
fn deccolm_set_with_scroll_on_clear_off_paints_the_active_background() {
    run_entry(DECCOLM_SET, false);
}

#[test]
fn deccolm_reset_with_scroll_on_clear_on_paints_the_active_background() {
    run_entry(DECCOLM_RESET, true);
}

#[test]
fn deccolm_reset_with_scroll_on_clear_off_paints_the_active_background() {
    run_entry(DECCOLM_RESET, false);
}

// --- Guards. ------------------------------------------------------------

/// The default background keeps producing the erase the crate already had, in
/// the compact encoding, so nothing about the existing default path moved.
#[test]
fn default_background_erases_keep_the_established_encoding() {
    for (label, style) in [
        ("no SGR at all", b"".as_slice()),
        ("SGR 49 after SGR 41", b"\x1b[41m\x1b[49m".as_slice()),
    ] {
        for scroll_on_clear in [true, false] {
            let context = format!("{label} scroll_on_clear={scroll_on_clear}");
            let mut case = Case::new(scroll_on_clear);
            case.feed(b"\x1b[4;1HZ");
            case.feed(style);
            case.feed(b"\x1b[H\x1b[2J");

            assert_viewport_erased(&case.screen, COLOUR_DEFAULT, &context);
            assert_eq!(
                case.screen.history_size(),
                if scroll_on_clear { 6 } else { 2 },
                "{context}: the policy is untouched"
            );
            assert_eq!(
                transcript(&case.screen),
                if scroll_on_clear {
                    b"OLD1\nOLD2\nVIS1\nVIS2\nVIS3\nZ\n\n\n\n\n".as_slice()
                } else {
                    b"OLD1\nOLD2\n\n\n\n\n".as_slice()
                },
                "{context}: transcript"
            );
        }
    }
}

/// Both non-basic encodings of a background reach every erased cell unchanged.
#[test]
fn indexed_and_rgb_backgrounds_reach_every_erased_cell() {
    for (label, style, bg) in [
        (
            "indexed SGR 48;5;93",
            b"\x1b[48;5;93m".as_slice(),
            COLOUR_FLAG_256 | 93,
        ),
        (
            "rgb SGR 48;2;10;200;30",
            b"\x1b[48;2;10;200;30m".as_slice(),
            colour_join_rgb(10, 200, 30),
        ),
    ] {
        for scroll_on_clear in [true, false] {
            let context = format!("{label} scroll_on_clear={scroll_on_clear}");
            let mut case = styled_case(scroll_on_clear);
            case.feed(style);
            case.feed(b"\x1b[H\x1b[2J");

            assert_viewport_erased(&case.screen, bg, &context);
        }
    }
}

/// An erase on the alternate screen takes the active background, transfers
/// nothing to scrollback and leaves the saved principal screen alone.
#[test]
fn alternate_screen_erase_paints_without_touching_history_or_the_saved_main() {
    for scroll_on_clear in [true, false] {
        let context = format!("alternate scroll_on_clear={scroll_on_clear}");
        let mut case = styled_case(scroll_on_clear);
        case.feed(PLAIN_STYLE);

        let history_before = history_facts(&case.screen);
        let main_before = transcript(&case.screen);

        case.feed(b"\x1b[?1049h");
        case.feed(b"ALT1\r\nALT2");
        case.feed(WITNESS_STYLE);
        case.feed(ERASE_STYLE);
        case.feed(b"\x1b[H\x1b[2J");

        assert!(case.screen.is_alternate(), "{context}: still alternate");
        assert_viewport_erased(&case.screen, ERASE_BG, &context);
        assert_eq!(
            case.screen.history_size(),
            2,
            "{context}: the alternate screen transfers nothing"
        );
        assert_eq!(
            history_facts(&case.screen),
            history_before,
            "{context}: scrollback untouched"
        );
        assert_eq!(
            case.screen
                .capture_saved_transcript(
                    ScreenCaptureRange {
                        start_is_absolute: true,
                        end_is_absolute: true,
                        ..ScreenCaptureRange::default()
                    },
                    GridRenderOptions::default(),
                )
                .expect("saved principal screen"),
            b"VIS1\nVIS2\nVIS3\nZ\n",
            "{context}: the saved principal screen is unchanged"
        );

        case.feed(b"\x1b[?1049l");
        assert!(!case.screen.is_alternate(), "{context}: back on principal");
        assert_eq!(
            transcript(&case.screen),
            main_before,
            "{context}: the restored principal screen is unchanged"
        );
    }
}

/// `RIS` keeps the behaviour RIS1 accepted, including its default background,
/// even with a non-default background active in the parser.
#[test]
fn reset_still_erases_to_the_default_background() {
    for scroll_on_clear in [true, false] {
        let context = format!("RIS scroll_on_clear={scroll_on_clear}");
        let mut case = styled_case(scroll_on_clear);
        case.feed(ERASE_STYLE);

        let history_before = history_facts(&case.screen);
        let viewport_before = viewport_facts(&case.screen);

        case.feed(b"\x1bc");

        assert_viewport_erased(&case.screen, COLOUR_DEFAULT, &context);
        let expected_history: Vec<Vec<CellFacts>> = if scroll_on_clear {
            history_before
                .iter()
                .chain(viewport_before.iter())
                .cloned()
                .collect()
        } else {
            history_before
        };
        assert_eq!(
            history_facts(&case.screen),
            expected_history,
            "{context}: the reset keeps its accepted transfer behaviour"
        );
        assert_eq!(
            (case.screen.cursor_x, case.screen.cursor_y),
            (0, 0),
            "{context}: the reset still homes the cursor"
        );
        assert_eq!(
            case.screen.scroll_on_clear(),
            scroll_on_clear,
            "{context}: the reset does not move the policy"
        );
    }
}

/// The colour change moves nothing else: `ED 2` leaves the cursor where it
/// was, DECCOLM keeps homing it, the geometry and the scroll region keep the
/// semantics they had, and the policy is still readable and effective.
#[test]
fn coloured_erases_keep_the_cursor_the_geometry_and_the_policy() {
    for entry in [
        ED2_FROM_HOME,
        ED2_AWAY_FROM_HOME,
        ED0_AT_HOME,
        DECCOLM_SET,
        DECCOLM_RESET,
    ] {
        for scroll_on_clear in [true, false] {
            let context = format!("{} scroll_on_clear={scroll_on_clear}", entry.name);
            let mut case = styled_case(scroll_on_clear);
            case.feed(b"\x1b[2;3r");
            case.feed(ERASE_STYLE);
            assert_eq!(
                (case.screen.rupper, case.screen.rlower),
                (1, 2),
                "{context}: the region is set before the erase"
            );

            case.feed(entry.bytes);

            assert_eq!(
                (case.screen.cursor_x, case.screen.cursor_y),
                entry.cursor_after,
                "{context}: cursor"
            );
            assert!(!case.screen.pending_wrap(), "{context}: no pending wrap");
            assert_eq!(case.screen.size(), SIZE, "{context}: geometry");
            assert_eq!(
                (case.screen.rupper, case.screen.rlower),
                (1, 2),
                "{context}: the erase does not touch the scroll region"
            );
            assert_eq!(
                case.screen.scroll_on_clear(),
                scroll_on_clear,
                "{context}: the policy value survives"
            );

            // The policy is still effective afterwards.
            let before = case.screen.history_size();
            case.feed(b"\x1b[H\x1b[49mNEXT");
            case.feed(b"\x1b[H\x1b[2J");
            assert_eq!(
                case.screen.history_size() > before,
                scroll_on_clear,
                "{context}: the next erase still follows the policy"
            );
        }
    }
}
