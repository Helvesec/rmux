//! A painted background is not written content.
//!
//! Once a whole-screen erase paints the active background, every row of the
//! viewport carries materialised cells. Two different questions are then asked
//! of such a row and they must get different answers:
//!
//! * *Is there content here?* — asked by the transfer to scrollback, by the
//!   reflow paths and by the width fast path. A cell that only carries an
//!   erase background is not content, so the answer is no. tmux 3.7b answers
//!   the same way: `grid_empty_line` resets `cellused` to zero and
//!   `grid_expand_line` re-materialises the background with `cellsize = sx`
//!   without touching `cellused`, so `grid_view_clear_history` transfers
//!   nothing.
//! * *How far do materialised cells reach?* — asked by storage, capture and
//!   the compaction caches. The painted cells are real and must survive, which
//!   is what makes a coloured blank viewport capture as full-width coloured
//!   spaces.
//!
//! The witnesses below are written against the observable consequences of that
//! split: how many rows a repeated erase moves to scrollback, what a resize
//! leaves in the viewport and in history, and what the capture emits. Each
//! case drives a real [`InputParser`] kept alive across its chunks, because the
//! background these erases use is parser state.
//!
//! Partial erases are neighbourhood here, not subject matter: nothing below
//! freezes a new expectation for `ED 0` away from home, `ED 1`, `EL` or `ECH`.

use super::Screen;
use crate::grid::GridCell;
use crate::input::{Colour, InputParser, COLOUR_DEFAULT};
use crate::{GridRenderOptions, ScreenCaptureRange};
use rmux_proto::TerminalSize;

const SIZE: TerminalSize = TerminalSize { cols: 12, rows: 4 };
const NARROW: TerminalSize = TerminalSize { cols: 8, rows: 4 };
const HISTORY_LIMIT: usize = 10;

/// `SGR 41`: a non-default background and nothing else, which is exactly the
/// shape an erase leaves behind — the cells it paints keep no foreground, no
/// attribute and no hyperlink whatever was active.
const ERASE_STYLE: &[u8] = b"\x1b[41m";
const ERASE_BG: Colour = 1;

const _: () = assert!(ERASE_BG != COLOUR_DEFAULT);

/// One whole-screen erase entry. All four reach `Screen::clear_whole_screen`.
#[derive(Clone, Copy)]
struct Entry {
    name: &'static str,
    bytes: &'static [u8],
}

const ENTRIES: [Entry; 4] = [
    Entry {
        name: "ED 2",
        bytes: b"\x1b[H\x1b[2J",
    },
    Entry {
        name: "ED 0 at home",
        bytes: b"\x1b[H\x1b[J",
    },
    Entry {
        name: "DECCOLM set",
        bytes: b"\x1b[?3h",
    },
    Entry {
        name: "DECCOLM reset",
        bytes: b"\x1b[?3l",
    },
];

/// The cell facts these witnesses are about.
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

/// A screen and the parser feeding it, so a case can hand its bytes over in
/// several chunks without losing the SGR state between them.
struct Case {
    screen: Screen,
    parser: InputParser,
}

impl Case {
    fn empty(scroll_on_clear: bool) -> Self {
        let mut screen = Screen::new(SIZE, HISTORY_LIMIT);
        screen.set_scroll_on_clear(scroll_on_clear);
        Self {
            screen,
            parser: InputParser::new(),
        }
    }

    /// `OLD1`/`OLD2` in scrollback, `VIS1`/`VIS2`/`VIS3` visible on the first
    /// three rows and the fourth row untouched.
    fn seeded(scroll_on_clear: bool) -> Self {
        let mut case = Self::empty(scroll_on_clear);
        case.feed(b"OLD1\r\nOLD2\r\nVIS1\r\nVIS2\r\nVIS3\r\n");
        assert_eq!(
            case.screen.history_size(),
            2,
            "fixture: OLD1/OLD2 start in scrollback, scroll_on_clear={scroll_on_clear}"
        );
        case
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.parser.parse(bytes, &mut self.screen);
    }

    fn cols(&self) -> u32 {
        u32::from(self.screen.size().cols)
    }

    fn rows(&self) -> u32 {
        u32::from(self.screen.size().rows)
    }

    fn visible_row(&self, y: u32) -> Vec<CellFacts> {
        let line = self.screen.grid().visible_line(y).expect("visible row");
        (0..self.cols())
            .map(|x| facts(&line.cell(x).unwrap_or_default()))
            .collect()
    }

    fn history_row_at(&self, absolute: usize, cols: u32) -> Vec<CellFacts> {
        let line = self
            .screen
            .grid()
            .absolute_line(absolute)
            .expect("scrollback row");
        (0..cols)
            .map(|x| facts(&line.cell(x).unwrap_or_default()))
            .collect()
    }

    fn history_row(&self, absolute: usize) -> Vec<CellFacts> {
        self.history_row_at(absolute, self.cols())
    }

    fn history(&self) -> Vec<Vec<CellFacts>> {
        self.history_at(self.cols())
    }

    /// Reads scrollback at an explicit width, for the cases where the viewport
    /// is re-widthed and the rows underneath keep the width they were written
    /// at, as an alternate-screen resize does.
    fn history_at(&self, cols: u32) -> Vec<Vec<CellFacts>> {
        (0..self.screen.history_size())
            .map(|absolute| self.history_row_at(absolute, cols))
            .collect()
    }

    /// The number of materialised cells of a visible row, which is what the
    /// painted bound governs.
    fn visible_cell_count(&self, y: u32) -> usize {
        self.screen
            .grid()
            .visible_line(y)
            .expect("visible row")
            .cells()
            .len()
    }

    fn visible_row_wrapped(&self, y: u32) -> bool {
        let absolute = self.screen.history_size() + y as usize;
        self.screen
            .grid()
            .absolute_line_wrapped(absolute)
            .expect("absolute row")
    }
}

/// The row a whole-screen erase with `bg` must leave behind.
fn erased_row(cols: u32, bg: Colour) -> Vec<CellFacts> {
    (0..cols)
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

/// Every viewport row is an erased row painted with `bg`, materialised across
/// the whole width.
fn assert_viewport_painted(case: &Case, bg: Colour, context: &str) {
    let expected = erased_row(case.cols(), bg);
    for y in 0..case.rows() {
        assert_eq!(
            case.visible_row(y),
            expected,
            "{context}: painted viewport row {y}"
        );
        assert_eq!(
            case.visible_cell_count(y),
            case.cols() as usize,
            "{context}: painted row {y} keeps every cell materialised"
        );
    }
}

// --- 1. A repeated erase over untouched rows transfers nothing. ----------

/// The second and third erase find no content and move nothing, so scrollback
/// is byte-identical after each of them.
///
/// This is the witness for the logical bound. With a single bound, the paint
/// the first erase left would read as content and every further erase would
/// flood scrollback with blank painted rows — `1 -> 5 -> 9 -> 10` on this
/// fixture instead of `1 -> 5 -> 5 -> 5`.
#[test]
fn repeated_whole_screen_erases_without_glyphs_transfer_nothing() {
    for entry in ENTRIES {
        for scroll_on_clear in [true, false] {
            let context = format!("{} scroll_on_clear={scroll_on_clear}", entry.name);
            let mut case = Case::seeded(scroll_on_clear);
            case.feed(ERASE_STYLE);

            case.feed(entry.bytes);
            let after_first = case.screen.history_size();
            assert_eq!(
                after_first,
                if scroll_on_clear { 5 } else { 2 },
                "{context}: the first erase moves the used rows the policy allows"
            );
            assert_viewport_painted(&case, ERASE_BG, &context);
            let history_after_first = case.history();

            case.feed(entry.bytes);
            assert_eq!(
                case.screen.history_size(),
                after_first,
                "{context}: the second erase finds no content to move"
            );
            assert_eq!(
                case.history(),
                history_after_first,
                "{context}: scrollback is unchanged by the second erase"
            );
            assert_viewport_painted(&case, ERASE_BG, &context);

            case.feed(entry.bytes);
            assert_eq!(
                case.screen.history_size(),
                after_first,
                "{context}: the third erase finds no content either"
            );
            assert_eq!(
                case.history(),
                history_after_first,
                "{context}: scrollback is unchanged by the third erase"
            );
            assert_viewport_painted(&case, ERASE_BG, &context);
        }
    }
}

// --- 2. One written line is the only thing the next erase moves. ---------

/// After a coloured erase, writing a single line and erasing again moves that
/// one line and nothing else: the other painted rows are still not content.
#[test]
fn writing_one_line_after_a_coloured_erase_transfers_only_that_line() {
    for entry in ENTRIES {
        let context = format!("{} scroll_on_clear=true", entry.name);
        let mut case = Case::seeded(true);
        case.feed(ERASE_STYLE);
        case.feed(entry.bytes);
        let before = case.screen.history_size();
        let history_before = case.history();
        assert_eq!(before, 5, "{context}: the first erase moved the used rows");

        case.feed(b"\x1b[1;1HNEW");
        case.feed(entry.bytes);

        assert_eq!(
            case.screen.history_size(),
            before + 1,
            "{context}: only the written line becomes content"
        );
        let history = case.history();
        assert_eq!(
            history[..before],
            history_before[..],
            "{context}: the rows already in scrollback are untouched"
        );

        let moved = &history[before];
        let glyphs: String = moved.iter().map(|cell| cell.text.as_str()).collect();
        assert_eq!(
            glyphs, "NEW         ",
            "{context}: the moved row is the row that was written"
        );
        for (x, cell) in moved.iter().enumerate() {
            assert_eq!(
                cell.bg, ERASE_BG,
                "{context}: moved row column {x} keeps the background it was painted with"
            );
        }
        assert_viewport_painted(&case, ERASE_BG, &context);
    }
}

// --- 3. A width shrink and regrow after a coloured clear. ----------------

/// Shrinking and regrowing the width over a coloured blank viewport adds no
/// row to scrollback, and the columns that survive the shrink keep their paint.
#[test]
fn width_shrink_and_regrow_after_a_coloured_clear_adds_no_history_row() {
    for scroll_on_clear in [true, false] {
        let context = format!("resize scroll_on_clear={scroll_on_clear}");
        let mut case = Case::seeded(scroll_on_clear);
        case.feed(ERASE_STYLE);
        case.feed(b"\x1b[H\x1b[2J");
        let history_after_erase = case.history();
        let size_after_erase = case.screen.history_size();
        assert_viewport_painted(&case, ERASE_BG, &context);

        case.screen.resize(NARROW);
        assert_eq!(
            case.screen.size(),
            NARROW,
            "{context}: the shrink took effect"
        );
        assert_eq!(
            case.screen.history_size(),
            size_after_erase,
            "{context}: the shrink adds no scrollback row"
        );
        assert_viewport_painted(&case, ERASE_BG, &format!("{context} narrow"));

        case.screen.resize(SIZE);
        assert_eq!(
            case.screen.size(),
            SIZE,
            "{context}: the regrow took effect"
        );
        assert_eq!(
            case.screen.history_size(),
            size_after_erase,
            "{context}: the regrow adds no scrollback row either"
        );
        assert_eq!(
            case.history(),
            history_after_erase,
            "{context}: scrollback keeps its own rows and styles across both resizes"
        );
        assert_eq!(
            (case.screen.cursor_x, case.screen.cursor_y),
            (0, 0),
            "{context}: the cursor stays where the erase left it"
        );

        for y in 0..case.rows() {
            let row = case.visible_row(y);
            for (x, cell) in row.iter().take(NARROW.cols as usize).enumerate() {
                assert_eq!(
                    cell,
                    &erased_row(1, ERASE_BG)[0],
                    "{context}: surviving column {x} of row {y} keeps its paint"
                );
            }
            // Columns created by the regrow follow the existing resize policy,
            // which this lot does not change and does not freeze here: only
            // their materialisation is asserted.
            assert_eq!(
                case.visible_cell_count(y),
                SIZE.cols as usize,
                "{context}: row {y} is materialised to the new width"
            );
        }
    }
}

// --- 4. A global reflow forced by another long line. ---------------------

/// A genuinely long wrapped line in scrollback forces the full reflow path
/// even though the coloured viewport rows are logically empty. Those rows must
/// come out of it as the painted physical rows they are: not rebuilt from the
/// resize background, not split into wrapped rows of blanks, and not pushed
/// into scrollback.
#[test]
fn global_reflow_forced_by_a_long_line_keeps_painted_rows_painted() {
    let mut case = Case::empty(false);
    // Thirty glyphs across a twelve-column screen: three physical rows joined
    // by the wrap flag, which is what makes the width resize take the reflow
    // path instead of the no-reflow fast path.
    case.feed(b"LLLLLLLLLLLLLLLLLLLLLLLLLLLLLL");
    case.feed(b"\r\n\r\n\r\n\r\n");
    assert_eq!(
        case.screen.history_size(),
        3,
        "fixture: the wrapped line is entirely in scrollback"
    );
    assert!(
        case.screen
            .grid()
            .absolute_line_wrapped(0)
            .expect("first scrollback row"),
        "fixture: the long line is still a wrapped chain"
    );

    case.feed(ERASE_STYLE);
    case.feed(b"\x1b[H\x1b[2J");
    assert_eq!(
        case.screen.history_size(),
        3,
        "fixture: scroll-on-clear is off, so the erase moves nothing"
    );
    assert_viewport_painted(&case, ERASE_BG, "reflow fixture");

    case.screen.resize(NARROW);

    // The long line reflows from three twelve-column rows into four
    // eight-column rows; that is the only thing that may change scrollback.
    assert_eq!(
        case.screen.history_size(),
        4,
        "reflow: only the long line re-lays itself out"
    );
    let history_glyphs: String = (0..case.screen.history_size())
        .map(|absolute| {
            case.history_row(absolute)
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .collect();
    assert_eq!(
        history_glyphs, "LLLLLLLLLLLLLLLLLLLLLLLLLLLLLL  ",
        "reflow: scrollback holds the long line and nothing else"
    );
    for absolute in 0..case.screen.history_size() {
        for (x, cell) in case.history_row(absolute).iter().enumerate() {
            assert_eq!(
                cell.bg, COLOUR_DEFAULT,
                "reflow: no painted row reached scrollback (row {absolute}, column {x})"
            );
        }
    }

    assert_viewport_painted(&case, ERASE_BG, "reflow");
    for y in 0..case.rows() {
        assert!(
            !case.visible_row_wrapped(y),
            "reflow: painted row {y} stays one physical row, not a wrapped chain"
        );
    }
    assert_eq!(
        (case.screen.cursor_x, case.screen.cursor_y),
        (0, 0),
        "reflow: the cursor stays on the painted row it was on"
    );
    assert!(
        !case.screen.pending_wrap(),
        "reflow: the cursor is not left waiting to wrap"
    );
}

// --- 5. A painted alternate screen. --------------------------------------

/// Painted alternate rows are not trailing padding: a width resize keeps them,
/// whether the whole alternate viewport is painted or only its tail.
#[test]
fn painted_alternate_rows_survive_a_width_resize() {
    for (label, after_erase) in [
        ("all rows painted", b"".as_slice()),
        ("painted tail only", b"\x1b[H\x1b[49mKEEP".as_slice()),
    ] {
        let mut case = Case::seeded(true);
        case.feed(b"\x1b[?1049h");
        case.feed(b"ALT1\r\nALT2");
        let history_before = case.history_at(SIZE.cols.into());

        case.feed(ERASE_STYLE);
        case.feed(b"\x1b[H\x1b[2J");
        assert!(case.screen.is_alternate(), "{label}: still alternate");
        assert_viewport_painted(&case, ERASE_BG, label);
        case.feed(after_erase);

        let painted_rows: Vec<u32> = if after_erase.is_empty() {
            (0..case.rows()).collect()
        } else {
            (1..case.rows()).collect()
        };

        case.screen.resize(NARROW);

        assert_eq!(
            case.screen.size(),
            NARROW,
            "{label}: the alternate screen resized"
        );
        assert_eq!(
            case.history_at(SIZE.cols.into()),
            history_before,
            "{label}: the alternate screen still transfers nothing to scrollback"
        );
        let expected = erased_row(NARROW.cols.into(), ERASE_BG);
        for y in painted_rows {
            assert_eq!(
                case.visible_row(y),
                expected,
                "{label}: painted alternate row {y} survives the resize"
            );
        }
        if !after_erase.is_empty() {
            let glyphs: String = case
                .visible_row(0)
                .iter()
                .map(|cell| cell.text.as_str())
                .collect();
            assert_eq!(glyphs, "KEEP    ", "{label}: the written row survives too");
        }
    }
}

// --- 6. Explicitly written spaces are content. ---------------------------

/// A space someone typed is content and moves with its row. The logical bound
/// excludes the cells an erase cleared, not every space glyph: it keeps the
/// `CLEARED` flag in the predicate precisely so that a written space still
/// counts.
///
/// The default-background half writes the spaces before a glyph. A row holding
/// nothing but spaces under the default background stays in the compact ASCII
/// encoding, whose cache does not retain a trailing space run; that is a
/// pre-existing property of that cache, which both bounds read identically and
/// which this lot neither changes nor freezes here.
#[test]
fn explicitly_written_spaces_are_content_and_transfer() {
    for (label, style, written, bg) in [
        (
            "painted background",
            ERASE_STYLE,
            b"\x1b[1;1H   ".as_slice(),
            ERASE_BG,
        ),
        (
            "default background",
            b"\x1b[m".as_slice(),
            b"\x1b[1;1H  X".as_slice(),
            COLOUR_DEFAULT,
        ),
    ] {
        let mut case = Case::empty(true);
        case.feed(style);
        case.feed(b"\x1b[H\x1b[2J");
        assert_eq!(
            case.screen.history_size(),
            0,
            "{label}: the erase found nothing to move"
        );

        case.feed(written);
        case.feed(b"\x1b[H\x1b[2J");

        assert_eq!(
            case.screen.history_size(),
            1,
            "{label}: the row of typed spaces is content, so the next erase moves it"
        );
        let moved = case.history_row(0);
        for (x, cell) in moved.iter().enumerate().take(2) {
            assert_eq!(
                cell.text, " ",
                "{label}: typed space at column {x} reached scrollback"
            );
            assert_eq!(
                cell.bg, bg,
                "{label}: typed space at column {x} kept its background"
            );
        }
    }
}

// --- 7. Padding and wide glyphs remain content. --------------------------

/// A wide glyph and the padding cell that follows it are content, including
/// when the padding is the last column of the row.
#[test]
fn padding_and_wide_glyphs_remain_content() {
    let mut case = Case::empty(true);
    case.feed(ERASE_STYLE);
    case.feed(b"\x1b[H\x1b[2J");
    assert_eq!(case.screen.history_size(), 0, "nothing written yet");

    // Column 10 of a twelve-column row: the wide glyph occupies 10 and its
    // padding occupies 11, so the row's content reaches the very last column.
    case.feed("\x1b[1;11H漢".as_bytes());
    case.feed(b"\x1b[H\x1b[2J");

    assert_eq!(
        case.screen.history_size(),
        1,
        "the row carrying a wide glyph is content"
    );
    let moved = case.history_row(0);
    assert_eq!(moved[10].text, "漢", "the wide glyph reached scrollback");
    assert_eq!(moved[10].width, 2, "it is still two columns wide");
    assert!(
        case.screen
            .grid()
            .absolute_line(0)
            .expect("scrollback row")
            .is_padding_cell(11),
        "its padding cell reached scrollback as padding"
    );
    for (x, cell) in moved.iter().enumerate() {
        assert_eq!(
            cell.bg, ERASE_BG,
            "moved row column {x} keeps the background it was painted with"
        );
    }
}

// --- 8. Capture and compaction keep the paint. ---------------------------

/// A coloured blank viewport captures as full-width coloured cells, and a
/// painted row that scrolls into scrollback keeps its cells through the
/// compaction that runs on the way in. Both read the painted bound, which the
/// logical one must not have narrowed.
#[test]
fn capture_and_compaction_keep_the_paint_of_a_blank_coloured_viewport() {
    let mut case = Case::empty(false);
    case.feed(ERASE_STYLE);
    case.feed(b"\x1b[H\x1b[2J");
    assert_viewport_painted(&case, ERASE_BG, "capture");

    // The options `capture-pane -e -N -T` resolves to: inline sequences, no
    // trimming, and no synthetic empty cells beyond what the line materialises.
    let options = GridRenderOptions {
        with_sequences: true,
        trim_spaces: false,
        include_empty_cells: false,
        ..GridRenderOptions::default()
    };
    let rows = case.screen.capture_transcript_lines_independent(
        ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        },
        options,
    );
    assert_eq!(rows.len(), 4, "capture: one row per visible line");
    let expected_row = b"\x1b[41m            ".to_vec();
    for (y, row) in rows.iter().enumerate() {
        assert_eq!(
            row, &expected_row,
            "capture: row {y} renders twelve coloured cells"
        );
    }

    // Scrolling moves the top painted row into scrollback, where
    // `compact_for_history` runs on it.
    case.feed(b"\x1b[4;1H\n");
    assert_eq!(
        case.screen.history_size(),
        1,
        "compaction: the scroll moved one row"
    );
    assert_eq!(
        case.history_row(0),
        erased_row(case.cols(), ERASE_BG),
        "compaction: the scrolled row keeps every painted cell"
    );
    assert_eq!(
        case.screen
            .grid()
            .absolute_line(0)
            .expect("scrollback row")
            .cells()
            .len(),
        case.cols() as usize,
        "compaction: the scrolled row is not truncated to its logical bound"
    );
}

// --- 9. A row that mixes a glyph with a painted suffix. -------------------

/// `X` followed by erase-painted blanks keeps that background through a global
/// reflow.
///
/// This row is the shape the logical bound gets wrong if it is asked how many
/// cells to carry: `used_end` is 1 and `painted_end` is the full width, so an
/// extent taken from the logical bound copies `X` alone and rebuilds columns
/// 1.. from the resize background. The wrapped chain in scrollback is what
/// forces the global reflow path; `EL 2` under `SGR 41` is what paints the
/// suffix without any whole-screen erase, so the precondition is reachable on
/// either side of this correction and nothing here freezes an expectation for
/// partial erases.
#[test]
fn a_glyph_followed_by_a_painted_suffix_keeps_its_background_through_reflow() {
    let mut case = Case::empty(false);
    case.feed(b"LLLLLLLLLLLLLLLLLLLLLLLLLLLLLL");
    case.feed(b"\r\n\r\n\r\n\r\n");
    assert_eq!(
        case.screen.history_size(),
        3,
        "fixture: the wrapped line is entirely in scrollback"
    );
    assert!(
        case.screen
            .grid()
            .absolute_line_wrapped(0)
            .expect("first scrollback row"),
        "fixture: the long line is still a wrapped chain"
    );

    // Home, paint the whole row with EL 2 under the erase style, then put a
    // single glyph back at column 0.
    case.feed(b"\x1b[H");
    case.feed(ERASE_STYLE);
    case.feed(b"\x1b[2K");
    case.feed(b"\x1b[H\x1b[0mX");

    let before = case.visible_row(0);
    assert_eq!(before[0].text, "X", "fixture: the glyph is at column 0");
    for (x, cell) in before.iter().enumerate().skip(1) {
        assert_eq!(
            cell.bg, ERASE_BG,
            "fixture: column {x} is painted by the line erase"
        );
    }

    case.screen.resize(NARROW);

    let after = case.visible_row(0);
    assert_eq!(after[0].text, "X", "reflow: the glyph survives at column 0");
    for (x, cell) in after.iter().enumerate().skip(1) {
        assert_eq!(
            cell.bg, ERASE_BG,
            "reflow: surviving column {x} keeps the painted background"
        );
        assert_eq!(
            cell.text, " ",
            "reflow: surviving column {x} stays blank, no content invented"
        );
    }
    assert_eq!(
        case.visible_cell_count(0),
        case.cols() as usize,
        "reflow: the row keeps every column materialised"
    );
}
