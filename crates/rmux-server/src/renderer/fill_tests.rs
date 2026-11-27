//! What the background layer is allowed to paint, and what it must leave alone.
//!
//! Every expected glyph below comes from a measured tmux 3.7b receipt, not from
//! what this implementation happens to produce. The oracle ran on the frozen
//! Linux binary sha256 `032198096ed82c1473b41c62a2ee83f16d9796a3f6a26f6830637591c24418a7`
//! before any of this code existed, reading the cells of a real client terminal
//! rather than `capture-pane` — which cannot see a background that never enters
//! a `Screen`. The receipts are `fill1/oracle/runs/`.
//!
//! These tests read the frame the renderer emits, so what they assert is the
//! cell a client would end up showing, including the cells the layer must not
//! have touched.

use rmux_core::input::InputParser;
use rmux_core::style::parse_colour;
use rmux_core::{FloatingPaneRect, OptionStore, Screen};
use rmux_proto::{
    OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize, WindowTarget,
};

use super::{render, render_display_panes_clear};
use crate::pane_terminals::HandlerState;
use crate::session_fixture::SessionFixture;

const TERMINAL: TerminalSize = TerminalSize { cols: 24, rows: 8 };
const WINDOW: TerminalSize = TerminalSize { cols: 12, rows: 4 };

/// The bullet tmux writes on an outside cell when nothing configures one:
/// `CELL_BORDERS[CELL_OUTSIDE]` is the ACS `~`, which resolves to U+00B7.
const FALLBACK: &str = "\u{b7}";

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn options(pairs: &[(OptionName, &str)]) -> OptionStore {
    let mut options = OptionStore::new();
    for (name, value) in [(OptionName::Status, "off")].iter().chain(pairs.iter()) {
        options
            .set(
                ScopeSelector::Global,
                *name,
                (*value).to_owned(),
                SetOptionMode::Replace,
            )
            .expect("option set");
    }
    options
}

fn window_option(options: &mut OptionStore, name: OptionName, value: &str) {
    options
        .set(
            ScopeSelector::Global,
            name,
            value.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("option set");
}

/// A window deliberately smaller than the client terminal, which is the shape
/// that produces margins in the first place.
fn smaller_window(name: &str) -> SessionFixture {
    let mut session = SessionFixture::new(session_name(name), TERMINAL);
    session.resize_active_window_geometry(TERMINAL, WINDOW);
    session
}

/// The cells a client would show after applying the frame.
fn cells(frame: &[u8], size: TerminalSize) -> Vec<Vec<String>> {
    let mut screen = Screen::new(size, 100);
    InputParser::new().parse(frame, &mut screen);
    (0..usize::from(size.rows))
        .map(|row| {
            let mut line = Vec::new();
            assert!(
                screen.visit_visible_line_cells(row, usize::from(size.cols), |cell| {
                    line.push(cell.text().to_owned());
                })
            );
            line
        })
        .collect()
}

fn rendered(session: &SessionFixture, options: &OptionStore) -> Vec<Vec<String>> {
    cells(render(session.view(), options).as_slice(), TERMINAL)
}

fn cell(grid: &[Vec<String>], y: usize, x: usize) -> &str {
    grid[y][x].as_str()
}

/// Coordinates outside the window but inside the terminal, on both axes.
const RIGHT_MARGIN: (usize, usize) = (1, 18);
const BOTTOM_MARGIN: (usize, usize) = (6, 4);
const CORNER: (usize, usize) = (7, 23);
const INSIDE_WINDOW: (usize, usize) = (1, 4);

#[test]
fn the_fill_character_contributes_its_first_scalar_or_nothing_at_all() {
    // The decision table measured on tmux 3.7b, case for case. Two properties
    // are load-bearing and easy to get wrong in the same direction: the option
    // is not a pattern that repeats across cells, and admissibility is a
    // property of the WHOLE string even though only its head is drawn.
    for (value, expected, why) in [
        (
            "",
            FALLBACK,
            "empty is the default, and tmux short-circuits on it",
        ),
        (
            "X",
            "X",
            "printable ASCII is width 1 by construction in utf8_set",
        ),
        (
            "XY",
            "X",
            "the first codepoint only; the tail is never drawn",
        ),
        (
            "X\u{754c}",
            "X",
            "a wide tail does not disqualify a width-1 head",
        ),
        (
            "\u{754c}X",
            FALLBACK,
            "a width-2 head is refused whatever follows",
        ),
        ("\u{754c}", FALLBACK, "width 2"),
        (
            "e\u{301}",
            "e",
            "the head is `e`; graphemes are not recomposed here",
        ),
        ("\u{301}X", FALLBACK, "a width-0 head is refused"),
        (
            "\u{200b}X",
            FALLBACK,
            "zero width, even though it is printable",
        ),
        (
            "X\u{1}",
            FALLBACK,
            "utf8_isvalid rejects a C0 byte anywhere",
        ),
        (
            "X\u{7f}",
            FALLBACK,
            "0x7f is past the printable ASCII range",
        ),
        ("\u{a0}", "\u{a0}", "multibyte and width 1: admissible"),
        ("~", "~", "drawn literally, not through the ACS table"),
        (FALLBACK, FALLBACK, "U+00B7 as an ordinary character"),
        ("X\u{0}Y", "X", "a C string ends at its first NUL"),
        ("\u{0}X", FALLBACK, "so does an empty one"),
    ] {
        let session = smaller_window("alpha");
        let mut store = options(&[]);
        window_option(&mut store, OptionName::FillCharacter, value);
        let grid = rendered(&session, &store);
        assert_eq!(
            cell(&grid, RIGHT_MARGIN.0, RIGHT_MARGIN.1),
            expected,
            "fill-character {value:?}: {why}"
        );
        // Not a repetition: every outside cell carries the same single glyph,
        // whatever the rest of the option said.
        assert_eq!(
            cell(&grid, BOTTOM_MARGIN.0, BOTTOM_MARGIN.1),
            expected,
            "fill-character {value:?} must reach every outside cell alike"
        );
    }
}

#[test]
fn a_window_smaller_than_its_terminal_fills_the_margins_it_leaves() {
    let session = smaller_window("alpha");
    let mut store = options(&[]);
    window_option(&mut store, OptionName::FillCharacter, "X");
    let grid = rendered(&session, &store);

    for y in 0..usize::from(TERMINAL.rows) {
        for x in 0..usize::from(TERMINAL.cols) {
            let inside = y < usize::from(WINDOW.rows) && x < usize::from(WINDOW.cols);
            if inside {
                assert_ne!(
                    cell(&grid, y, x),
                    "X",
                    "({y},{x}) belongs to the window and is not the background's to paint"
                );
            } else {
                assert_eq!(cell(&grid, y, x), "X", "({y},{x}) is a margin cell");
            }
        }
    }
}

#[test]
fn the_pane_border_lines_family_decides_the_glyph_when_nothing_else_does() {
    // Measured on tmux 3.7b: single, double, heavy and number all resolve to
    // the same bullet for this cell — number has no pane index to show — while
    // simple and spaces have their own.
    for (lines, expected) in [
        ("single", FALLBACK),
        ("double", FALLBACK),
        ("heavy", FALLBACK),
        ("number", FALLBACK),
        ("simple", "."),
        ("spaces", " "),
    ] {
        let session = smaller_window("alpha");
        let mut store = options(&[]);
        window_option(&mut store, OptionName::PaneBorderLines, lines);
        let grid = rendered(&session, &store);
        assert_eq!(
            cell(&grid, BOTTOM_MARGIN.0, BOTTOM_MARGIN.1),
            expected,
            "pane-border-lines {lines}"
        );

        // And an admissible fill-character wins over every one of them.
        window_option(&mut store, OptionName::FillCharacter, "X");
        let grid = rendered(&session, &store);
        assert_eq!(
            cell(&grid, BOTTOM_MARGIN.0, BOTTOM_MARGIN.1),
            "X",
            "pane-border-lines {lines} with a fill character"
        );
    }
}

#[test]
fn the_status_lines_are_never_part_of_the_filled_surface() {
    // The surface is the physical terminal minus the status band, and the
    // content offset is applied exactly once. Both positions are checked
    // because only one of them moves the content down.
    for (position, status_row, first_content_row) in [("top", 0, 1), ("bottom", 7, 0)] {
        let mut session = SessionFixture::new(session_name("alpha"), TERMINAL);
        session.resize_active_window_geometry(TERMINAL, WINDOW);
        let mut store = OptionStore::new();
        window_option(&mut store, OptionName::Status, "on");
        window_option(&mut store, OptionName::StatusPosition, position);
        window_option(&mut store, OptionName::FillCharacter, "X");
        let grid = cells(render(session.view(), &store).as_slice(), TERMINAL);

        // The status bar is composed after the background, so reading the
        // finished cells alone could not tell a status row that was never
        // filled from one that was filled and then painted over. Both are
        // checked: the runs say what the layer claimed, the cells say what the
        // client ends up showing.
        let runs = super::projection::TerminalSceneProjection::resolve(session.view(), &store)
            .outside_runs();
        assert!(!runs.is_empty(), "{position}: the fixture has margins");
        assert!(
            runs.iter().all(|run| usize::from(run.y) != status_row),
            "{position}: the status band is not part of the filled surface: {runs:?}"
        );
        for x in 0..usize::from(TERMINAL.cols) {
            assert_ne!(
                cell(&grid, status_row, x),
                "X",
                "{position}: the status row must keep its own rendering at column {x}"
            );
        }
        assert_eq!(
            cell(&grid, first_content_row, usize::from(WINDOW.cols)),
            "X",
            "{position}: the first content row's margin is filled"
        );
        // The window occupies WINDOW.rows starting at the content offset, so
        // the row just past it is background on both sides of the offset.
        let below = first_content_row + usize::from(WINDOW.rows);
        assert_eq!(
            cell(&grid, below, 0),
            "X",
            "{position}: the row below the window is filled"
        );
        assert_ne!(
            cell(&grid, first_content_row, 0),
            "X",
            "{position}: the window's own first cell is not the background's"
        );
    }
}

#[test]
fn a_float_gives_its_cells_back_to_the_background_only_when_it_is_not_shown() {
    let mut session = SessionFixture::new(session_name("alpha"), TERMINAL);
    let mut store = options(&[]);
    window_option(&mut store, OptionName::FillCharacter, "X");

    // The window's only pane floats, so the tiled layout empties and the cells
    // it used to cover have no owner but the float itself.
    let target = WindowTarget::with_window(session.name().clone(), session.active_window_index());
    session
        .store
        .window_layout_context_mut(&target)
        .expect("window layout context")
        .float_pane(0, FloatingPaneRect::new(4, 2, 6, 3).expect("rect"))
        .expect("pane can float");

    let visible = rendered(&session, &store);
    assert_ne!(
        cell(&visible, 3, 6),
        "X",
        "a visible float owns its own content"
    );
    assert_ne!(
        cell(&visible, 1, 4),
        "X",
        "and the frame it draws around that content"
    );
    assert_eq!(
        cell(&visible, 6, 20),
        "X",
        "everything the float does not cover is background"
    );

    session
        .store
        .window_layout_context_mut(&target)
        .expect("window layout context")
        .set_floating_pane_visible(0, false)
        .expect("float can hide");

    let hidden = rendered(&session, &store);
    assert_eq!(
        cell(&hidden, 3, 6),
        "X",
        "a hidden float claims nothing, so its cells become background"
    );
    assert_eq!(
        cell(&hidden, 1, 4),
        "X",
        "including the cells its frame used to occupy"
    );
}

#[test]
fn borders_scrollbars_and_pane_bodies_keep_every_cell_they_own() {
    // A split window at full size has no margin at all: everything the
    // background could reach is already owned by a pane, its border or its
    // scrollbar. The assertion is therefore the strongest available one — the
    // layer contributes no bytes whatsoever.
    let mut session = SessionFixture::new(session_name("alpha"), TERMINAL);
    session.split_active_pane().expect("split succeeds");
    let mut store = options(&[
        (OptionName::PaneScrollbars, "on"),
        (OptionName::PaneScrollbarsPosition, "right"),
    ]);
    window_option(&mut store, OptionName::FillCharacter, "X");

    let grid = rendered(&session, &store);
    for (y, row) in grid.iter().enumerate() {
        for (x, text) in row.iter().enumerate() {
            assert_ne!(
                text.as_str(),
                "X",
                "({y},{x}) is owned by a pane, its border or its scrollbar"
            );
        }
    }
    assert!(
        super::fill::render_outside_fill(session.view(), &store).is_empty(),
        "a fully covered surface must not even open a background layer"
    );

    // A pane's own empty cells are still the pane's: shrink the window so only
    // the margin is background, and check the split's border survives.
    session.resize_active_window_geometry(TERMINAL, WINDOW);
    let grid = rendered(&session, &store);
    assert_eq!(
        cell(&grid, 0, usize::from(WINDOW.cols)),
        "X",
        "the margin beside the window is background"
    );
    let border_column = grid
        .iter()
        .take(usize::from(WINDOW.rows))
        .filter_map(|row| {
            row.iter()
                .take(usize::from(WINDOW.cols))
                .position(|text| text == "\u{2502}")
        })
        .next();
    let border_column = border_column.expect("the split draws a vertical border inside the window");
    for row in grid.iter().take(usize::from(WINDOW.rows)) {
        assert_ne!(
            row[border_column].as_str(),
            "X",
            "the border column keeps its glyph"
        );
    }
}

#[test]
fn changing_the_option_moves_outside_cells_and_only_outside_cells() {
    let session = smaller_window("alpha");
    let mut store = options(&[]);
    window_option(&mut store, OptionName::FillCharacter, "X");
    let before = rendered(&session, &store);

    window_option(&mut store, OptionName::FillCharacter, "Y");
    let after = rendered(&session, &store);

    let mut moved = 0_usize;
    for y in 0..usize::from(TERMINAL.rows) {
        for x in 0..usize::from(TERMINAL.cols) {
            if cell(&before, y, x) == cell(&after, y, x) {
                continue;
            }
            moved += 1;
            let inside = y < usize::from(WINDOW.rows) && x < usize::from(WINDOW.cols);
            assert!(!inside, "({y},{x}) is inside the window and must not move");
            assert_eq!(cell(&before, y, x), "X");
            assert_eq!(cell(&after, y, x), "Y");
        }
    }
    let surface = usize::from(TERMINAL.cols) * usize::from(TERMINAL.rows);
    let window = usize::from(WINDOW.cols) * usize::from(WINDOW.rows);
    assert_eq!(
        moved,
        surface - window,
        "every outside cell moved, and nothing else did"
    );

    // Unsetting it returns the surface to the pane-border-lines glyph rather
    // than to the blank cells rmux used to leave there.
    window_option(&mut store, OptionName::FillCharacter, "");
    let unset = rendered(&session, &store);
    assert_eq!(cell(&unset, CORNER.0, CORNER.1), FALLBACK);
    assert_ne!(cell(&unset, INSIDE_WINDOW.0, INSIDE_WINDOW.1), FALLBACK);
}

#[test]
fn the_display_panes_overlay_leaves_the_background_it_found() {
    let session = smaller_window("alpha");
    let mut store = options(&[]);
    window_option(&mut store, OptionName::FillCharacter, "X");
    let plain = rendered(&session, &store);

    let state = HandlerState::default();
    let overlay = render_display_panes_clear(session.view(), &store, &state);
    assert!(!overlay.is_empty(), "the overlay renders something");
    let overlaid = cells(overlay.as_slice(), TERMINAL);

    // The overlay clears and labels pane cells; it composes the same base frame
    // afterwards, so every background cell must read exactly as it did without
    // the overlay — and must still be the background, not a cell the clear
    // blanked on its way past.
    let mut background_cells = 0_usize;
    for y in 0..usize::from(TERMINAL.rows) {
        for x in 0..usize::from(TERMINAL.cols) {
            if y < usize::from(WINDOW.rows) && x < usize::from(WINDOW.cols) {
                continue;
            }
            background_cells += 1;
            assert_eq!(
                cell(&overlaid, y, x),
                "X",
                "({y},{x}) is background and must survive the overlay"
            );
            assert_eq!(
                cell(&overlaid, y, x),
                cell(&plain, y, x),
                "({y},{x}) is background and the overlay never owned it"
            );
        }
    }
    assert_eq!(
        background_cells,
        usize::from(TERMINAL.cols) * usize::from(TERMINAL.rows)
            - usize::from(WINDOW.cols) * usize::from(WINDOW.rows),
        "the fixture must actually have a background for this to mean anything"
    );
}

#[test]
fn the_background_takes_the_window_border_style_and_never_the_active_one() {
    // tmux styles this cell from the window's pane-border-style over the
    // default cell, with a window format context and no pane, precisely
    // because no pane owns it.
    let session = smaller_window("alpha");
    let mut store = options(&[]);
    window_option(&mut store, OptionName::FillCharacter, "X");
    window_option(&mut store, OptionName::PaneBorderStyle, "fg=red");
    window_option(&mut store, OptionName::PaneActiveBorderStyle, "fg=green");

    let frame = render(session.view(), &store);
    let mut screen = Screen::new(TERMINAL, 100);
    InputParser::new().parse(frame.as_slice(), &mut screen);
    let mut row = Vec::new();
    assert!(screen
        .visit_visible_line_cells(BOTTOM_MARGIN.0, usize::from(TERMINAL.cols), |cell| row
            .push((cell.text().to_owned(), cell.fg())),));
    let (text, foreground) = row[BOTTOM_MARGIN.1].clone();
    assert_eq!(text, "X");
    assert_eq!(
        foreground,
        parse_colour("red").expect("red parses"),
        "the background wears pane-border-style"
    );
    assert_ne!(
        foreground,
        parse_colour("green").expect("green parses"),
        "and never pane-active-border-style: no pane owns the cell"
    );
}

#[test]
fn nothing_a_pane_owns_ever_reaches_an_outside_run() {
    // Composition would hide a mistake here: the background is written before
    // the frames and the pane-border-status rows, so a cell it wrongly claimed
    // would simply be painted over. The runs themselves are the thing to
    // assert, and they are asserted against the whole window rectangle rather
    // than against a list of features, so a new one cannot be forgotten.
    for status in ["off", "top", "bottom"] {
        let mut session = SessionFixture::new(session_name("alpha"), TERMINAL);
        session.split_active_pane().expect("split succeeds");
        session.resize_active_window_geometry(TERMINAL, WINDOW);
        let store = options(&[
            (OptionName::PaneBorderStatus, status),
            (OptionName::PaneBorderFormat, "S"),
            (OptionName::PaneScrollbars, "on"),
            (OptionName::FillCharacter, "X"),
        ]);
        let runs = super::projection::TerminalSceneProjection::resolve(session.view(), &store)
            .outside_runs();
        let mut covered = 0_usize;
        for run in &runs {
            for x in run.x..run.x.saturating_add(run.cols) {
                covered += 1;
                assert!(
                    run.y >= WINDOW.rows || x >= WINDOW.cols,
                    "pane-border-status {status}: ({},{x}) is inside the window",
                    run.y
                );
            }
        }
        assert_eq!(
            covered,
            usize::from(TERMINAL.cols) * usize::from(TERMINAL.rows)
                - usize::from(WINDOW.cols) * usize::from(WINDOW.rows),
            "pane-border-status {status}: the runs cover exactly the margins"
        );
    }
}

#[test]
fn a_zoomed_window_repaints_the_margins_its_own_clear_wiped() {
    // The zoom clear erases the whole terminal, margins included, which is
    // precisely why the background is composed after it and not before.
    let mut session = SessionFixture::new(session_name("alpha"), TERMINAL);
    session.split_active_pane().expect("split succeeds");
    session.resize_active_window_geometry(TERMINAL, WINDOW);
    let window_index = session.active_window_index();
    session
        .toggle_zoom_in_window(window_index, 0)
        .expect("zoom succeeds");
    assert!(
        session.window().is_zoomed(),
        "the fixture is actually zoomed"
    );

    let store = options(&[(OptionName::FillCharacter, "X")]);
    let grid = rendered(&session, &store);
    assert_eq!(
        cell(&grid, 0, usize::from(WINDOW.cols)),
        "X",
        "the margin beside a zoomed window is still background"
    );
    assert_eq!(
        cell(&grid, usize::from(WINDOW.rows), 0),
        "X",
        "and so is the one below it"
    );
    for y in 0..usize::from(WINDOW.rows) {
        for x in 0..usize::from(WINDOW.cols) {
            assert_ne!(
                cell(&grid, y, x),
                "X",
                "({y},{x}) belongs to the zoomed pane"
            );
        }
    }
}

#[test]
fn a_printable_ascii_fill_character_is_one_cell_whatever_codepoint_widths_says() {
    // tmux never asks the width cache about ASCII: `utf8_fromcstr` hands a
    // single byte to `utf8_set`, whose constant record fixes size, have and
    // width to 1 (utf8.c:531). `codepoint-widths` only feeds `utf8_width`,
    // which that path never reaches. So an override on `X` changes nothing
    // about whether `X` may fill.
    for width in ["0", "2"] {
        let session = smaller_window("alpha");
        let mut store = options(&[]);
        window_option(
            &mut store,
            OptionName::CodepointWidths,
            &format!("U+0058={width}"),
        );
        window_option(&mut store, OptionName::FillCharacter, "X");
        let grid = rendered(&session, &store);
        assert_eq!(
            cell(&grid, RIGHT_MARGIN.0, RIGHT_MARGIN.1),
            "X",
            "codepoint-widths U+0058={width} must not disqualify an ASCII fill"
        );
        assert_eq!(
            cell(&grid, BOTTOM_MARGIN.0, BOTTOM_MARGIN.1),
            "X",
            "codepoint-widths U+0058={width}: every outside cell alike"
        );

        // The override must not become a way past the admissibility gate
        // either: the whole effective string is still validated first.
        window_option(&mut store, OptionName::FillCharacter, "X\u{1}");
        let guarded = rendered(&session, &store);
        assert_eq!(
            cell(&guarded, BOTTOM_MARGIN.0, BOTTOM_MARGIN.1),
            FALLBACK,
            "codepoint-widths U+0058={width}: a C0 byte anywhere still refuses \
             the whole string"
        );
    }
}

#[test]
fn a_multibyte_fill_character_still_obeys_the_configured_width() {
    // The other half of the same decision: a multibyte head keeps consulting
    // the width configuration, exactly as `utf8_width` does in tmux.
    for (override_value, expected, why) in [
        ("", "\u{e9}", "U+00E9 is naturally one cell"),
        ("U+00E9=1", "\u{e9}", "declared one cell, so admissible"),
        ("U+00E9=2", FALLBACK, "declared two cells, so refused"),
        ("U+00E9=0", FALLBACK, "declared zero cells, so refused"),
    ] {
        let session = smaller_window("alpha");
        let mut store = options(&[]);
        if !override_value.is_empty() {
            window_option(&mut store, OptionName::CodepointWidths, override_value);
        }
        window_option(&mut store, OptionName::FillCharacter, "\u{e9}");
        let grid = rendered(&session, &store);
        assert_eq!(
            cell(&grid, BOTTOM_MARGIN.0, BOTTOM_MARGIN.1),
            expected,
            "codepoint-widths {override_value:?}: {why}"
        );
    }
}
