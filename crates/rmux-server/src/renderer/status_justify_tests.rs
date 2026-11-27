//! `status-justify` on the default status bar.
//!
//! Every expectation here is a column measured on the pinned tmux 3.7b before
//! the correction, through a real attached client, by rebuilding the status row
//! from the bytes that client received
//! (`sj1/native/oracle-justify.json`). The same stimulus is used on
//! both sides: a fourteen-column left component, a three-column right one, and
//! the window list `<w1> [w2] <w3>`.
//!
//! The rendering path exercised is the public one - `render` and
//! `status_bar_lines` - not the `status_bar_runs` helper, which is dead outside
//! its own tests and never draws this bar.

use super::{render, status_bar_lines};
use crate::status_ranges::StatusRangeType;
use rmux_core::OptionStore;
use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

const LEFT: &str = "LEFT-COMPONENT";
const RIGHT: &str = "RGT";
const LIST: &str = "<w1> [w2] <w3>";
const EVEN_WIDTH: usize = 80;
const ODD_WIDTH: usize = 81;
const ROWS: u16 = 24;

/// The rows tmux 3.7b really drew, cell for cell.
const ORACLE_LEFT_EVEN: &str =
    "LEFT-COMPONENT<w1> [w2] <w3>                                                 RGT";
const ORACLE_CENTRE_EVEN: &str =
    "LEFT-COMPONENT                        <w1> [w2] <w3>                         RGT";
const ORACLE_ABSOLUTE_EVEN: &str =
    "LEFT-COMPONENT                   <w1> [w2] <w3>                              RGT";
const ORACLE_RIGHT_EVEN: &str =
    "LEFT-COMPONENT                                                 <w1> [w2] <w3>RGT";
const ORACLE_CENTRE_ODD: &str =
    "LEFT-COMPONENT                         <w1> [w2] <w3>                         RGT";
const ORACLE_ABSOLUTE_ODD: &str =
    "LEFT-COMPONENT                   <w1> [w2] <w3>                               RGT";

/// The measured 1-based columns of the list, for the assertions that only need
/// the placement.
const COLUMN_LEFT_EVEN: usize = 15;
const COLUMN_CENTRE_EVEN: usize = 39;
const COLUMN_ABSOLUTE_EVEN: usize = 34;
const COLUMN_RIGHT_EVEN: usize = 64;
const COLUMN_CENTRE_ODD: usize = 40;
const COLUMN_ABSOLUTE_ODD: usize = 34;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// The same three windows as the oracle, with the middle one current.
fn fixture(name: &str, cols: usize) -> crate::session_fixture::SessionFixture {
    let size = TerminalSize {
        cols: u16::try_from(cols).expect("width fits"),
        rows: ROWS,
    };
    let mut session = crate::session_fixture::SessionFixture::new(session_name(name), size);
    session
        .rename_window(0, "w1".to_owned())
        .expect("window 0 renames");
    for (index, window_name) in [(1_u32, "w2"), (2, "w3")] {
        session
            .insert_window_with_initial_pane(index, size)
            .expect("window inserts");
        session
            .rename_window(index, window_name.to_owned())
            .expect("window renames");
    }
    session.select_window(1).expect("window 1 selects");
    session
}

fn set_global(options: &mut OptionStore, option: OptionName, value: &str) {
    options
        .set(
            ScopeSelector::Global,
            option,
            value.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("global option set succeeds");
}

fn set_session(options: &mut OptionStore, name: &SessionName, option: OptionName, value: &str) {
    options
        .set(
            ScopeSelector::Session(name.clone()),
            option,
            value.to_owned(),
            SetOptionMode::Replace,
        )
        .expect("session option set succeeds");
}

/// The stimulus: an asymmetric bar whose window list is easy to find.
fn stimuli(justify: Option<&str>) -> OptionStore {
    let mut options = OptionStore::new();
    for (option, value) in [
        (OptionName::StatusStyle, "default"),
        (OptionName::StatusLeft, LEFT),
        (OptionName::StatusRight, RIGHT),
        (OptionName::StatusLeftLength, "100"),
        (OptionName::StatusRightLength, "100"),
        (OptionName::WindowStatusFormat, "<#W>"),
        (OptionName::WindowStatusCurrentFormat, "[#W]"),
        (OptionName::WindowStatusSeparator, " "),
    ] {
        set_global(&mut options, option, value);
    }
    if let Some(justify) = justify {
        set_global(&mut options, OptionName::StatusJustify, justify);
    }
    options
}

/// The status row exactly as the frame draws it, escape sequences removed.
fn status_row(frame: &[u8], width: usize) -> String {
    let text = String::from_utf8_lossy(frame).into_owned();
    let marker = format!("\u{1b}[{ROWS};1H");
    let start = text.rfind(&marker).expect("the status row is drawn") + marker.len();
    let mut row = String::new();
    let mut characters = text[start..].chars().peekable();
    while row.chars().count() < width {
        let Some(character) = characters.next() else {
            break;
        };
        if character == '\u{1b}' {
            match characters.next() {
                Some('[') => {
                    for next in characters.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some('(' | ')') => {
                    characters.next();
                }
                _ => {}
            }
            continue;
        }
        if character == '\r' || character == '\n' {
            break;
        }
        row.push(character);
    }
    row
}

fn rendered_row(
    session: &crate::session_fixture::SessionFixture,
    options: &OptionStore,
    width: usize,
) -> String {
    status_row(&render(session.view(), options), width)
}

/// The 1-based column where the window list starts.
fn list_column(row: &str) -> usize {
    row.find(LIST).expect("the window list is drawn") + 1
}

#[test]
fn absolute_centre_centres_the_window_list_on_the_whole_bar() {
    let session = fixture("alpha", EVEN_WIDTH);
    let options = stimuli(Some("absolute-centre"));

    let row = rendered_row(&session, &options, EVEN_WIDTH);

    assert_eq!(row, ORACLE_ABSOLUTE_EVEN, "{row}");
    assert_eq!(list_column(&row), COLUMN_ABSOLUTE_EVEN);
    // The two components are untouched: only the list moved.
    assert!(row.starts_with(LEFT), "{row}");
    assert!(row.ends_with(RIGHT), "{row}");
}

#[test]
fn centre_stays_relative_to_the_room_between_the_components() {
    let session = fixture("alpha", EVEN_WIDTH);
    let options = stimuli(Some("centre"));

    let row = rendered_row(&session, &options, EVEN_WIDTH);

    assert_eq!(row, ORACLE_CENTRE_EVEN, "{row}");
    assert_eq!(list_column(&row), COLUMN_CENTRE_EVEN);
    assert_ne!(COLUMN_CENTRE_EVEN, COLUMN_ABSOLUTE_EVEN);
}

#[test]
fn an_odd_width_keeps_the_rounding_of_each_value() {
    let session = fixture("alpha", ODD_WIDTH);

    let centre = rendered_row(&session, &stimuli(Some("centre")), ODD_WIDTH);
    assert_eq!(centre, ORACLE_CENTRE_ODD, "{centre}");
    assert_eq!(list_column(&centre), COLUMN_CENTRE_ODD);

    let absolute = rendered_row(&session, &stimuli(Some("absolute-centre")), ODD_WIDTH);
    assert_eq!(absolute, ORACLE_ABSOLUTE_ODD, "{absolute}");
    assert_eq!(list_column(&absolute), COLUMN_ABSOLUTE_ODD);
}

#[test]
fn left_and_right_keep_their_columns() {
    let session = fixture("alpha", EVEN_WIDTH);

    let left = rendered_row(&session, &stimuli(Some("left")), EVEN_WIDTH);
    assert_eq!(left, ORACLE_LEFT_EVEN, "{left}");
    assert_eq!(list_column(&left), COLUMN_LEFT_EVEN);

    let right = rendered_row(&session, &stimuli(Some("right")), EVEN_WIDTH);
    assert_eq!(right, ORACLE_RIGHT_EVEN, "{right}");
    assert_eq!(list_column(&right), COLUMN_RIGHT_EVEN);
}

#[test]
fn the_default_value_is_still_the_left_edge() {
    // No status-justify at all: the option's own default answers, and the bar
    // must look exactly like the measured left cell.
    let session = fixture("alpha", EVEN_WIDTH);

    let row = rendered_row(&session, &stimuli(None), EVEN_WIDTH);

    assert_eq!(row, ORACLE_LEFT_EVEN, "{row}");
}

#[test]
fn changing_the_option_moves_the_list_on_the_next_render() {
    let session = fixture("alpha", EVEN_WIDTH);
    let mut options = stimuli(Some("centre"));

    let before = rendered_row(&session, &options, EVEN_WIDTH);
    set_global(&mut options, OptionName::StatusJustify, "absolute-centre");
    let after = rendered_row(&session, &options, EVEN_WIDTH);

    assert_eq!(list_column(&before), COLUMN_CENTRE_EVEN, "{before}");
    assert_eq!(list_column(&after), COLUMN_ABSOLUTE_EVEN, "{after}");
    assert_eq!(after, ORACLE_ABSOLUTE_EVEN, "{after}");
}

#[test]
fn a_session_override_wins_and_its_removal_returns_to_the_inherited_value() {
    let session = fixture("alpha", EVEN_WIDTH);
    let mut options = stimuli(Some("centre"));
    set_session(
        &mut options,
        session.name(),
        OptionName::StatusJustify,
        "absolute-centre",
    );

    let with_override = rendered_row(&session, &options, EVEN_WIDTH);
    assert_eq!(list_column(&with_override), COLUMN_ABSOLUTE_EVEN);

    options
        .set_by_name(
            rmux_proto::types::OptionScopeSelector::Session(session.name().clone()),
            "status-justify",
            None,
            SetOptionMode::Replace,
            false,
            true,
            false,
        )
        .expect("session option unset succeeds");

    let inherited = rendered_row(&session, &options, EVEN_WIDTH);
    assert_eq!(inherited, ORACLE_CENTRE_EVEN, "{inherited}");
    assert_eq!(list_column(&inherited), COLUMN_CENTRE_EVEN);
}

#[test]
fn a_neighbouring_session_keeps_the_global_value() {
    // Two sessions with the same windows, one option store: only alpha carries
    // the override, so beta has to keep drawing the global centre.
    let alpha = fixture("alpha", EVEN_WIDTH);
    let beta = fixture("beta", EVEN_WIDTH);
    let mut options = stimuli(Some("centre"));
    set_session(
        &mut options,
        alpha.name(),
        OptionName::StatusJustify,
        "absolute-centre",
    );

    let alpha_row = rendered_row(&alpha, &options, EVEN_WIDTH);
    let beta_row = rendered_row(&beta, &options, EVEN_WIDTH);

    assert_eq!(alpha_row, ORACLE_ABSOLUTE_EVEN, "{alpha_row}");
    assert_eq!(beta_row, ORACLE_CENTRE_EVEN, "{beta_row}");
    assert_eq!(list_column(&alpha_row), COLUMN_ABSOLUTE_EVEN);
    assert_eq!(list_column(&beta_row), COLUMN_CENTRE_EVEN);
}

#[test]
fn an_explicit_status_format_keeps_its_acquired_behaviour() {
    let session = fixture("alpha", EVEN_WIDTH);
    let explicit = format!(
        "#[align=left]{LEFT}#[list=on align=#{{status-justify}}]{LIST}#[nolist]#[align=right]{RIGHT}"
    );

    for (justify, expected_row, expected_column) in [
        ("centre", ORACLE_CENTRE_EVEN, COLUMN_CENTRE_EVEN),
        (
            "absolute-centre",
            ORACLE_ABSOLUTE_EVEN,
            COLUMN_ABSOLUTE_EVEN,
        ),
    ] {
        let mut options = stimuli(Some(justify));
        options
            .set_by_name(
                rmux_proto::types::OptionScopeSelector::Session(session.name().clone()),
                "status-format[0]",
                Some(explicit.clone()),
                SetOptionMode::Replace,
                false,
                false,
                false,
            )
            .expect("status-format option set succeeds");

        let row = rendered_row(&session, &options, EVEN_WIDTH);

        assert_eq!(row, expected_row, "{justify}: {row}");
        assert_eq!(list_column(&row), expected_column, "{justify}");
    }
}

#[test]
fn the_window_ranges_follow_the_moved_list() {
    let session = fixture("alpha", EVEN_WIDTH);
    let columns = u16::try_from(EVEN_WIDTH).expect("width fits");
    let ids: Vec<u32> = (0..3)
        .map(|index| {
            session
                .window_at(index)
                .expect("window exists")
                .id()
                .as_u32()
        })
        .collect();

    let mut starts = Vec::new();
    for justify in ["centre", "absolute-centre"] {
        let options = stimuli(Some(justify));
        let lines = status_bar_lines(session.view(), &options, columns, 0, 1, None);
        let line = lines.first().expect("one status line");
        let mut windows = Vec::new();
        for id in &ids {
            let range = line
                .ranges
                .iter()
                .find(|range| range.kind == StatusRangeType::Window(*id))
                .unwrap_or_else(|| panic!("{justify}: window {id} has a range"));
            windows.push((*range.x.start(), *range.x.end()));
        }
        starts.push(windows);
    }

    let centre = &starts[0];
    let absolute = &starts[1];
    let shift = u16::try_from(COLUMN_CENTRE_EVEN - COLUMN_ABSOLUTE_EVEN).expect("shift fits");
    for (index, ((centre_start, centre_end), (absolute_start, absolute_end))) in
        centre.iter().zip(absolute.iter()).enumerate()
    {
        assert_eq!(
            *centre_start - shift,
            *absolute_start,
            "window {index} range start follows the list"
        );
        assert_eq!(
            *centre_end - shift,
            *absolute_end,
            "window {index} range end follows the list"
        );
    }
    // The first window's range really starts where the list is drawn: the
    // ranges are the cells' own, not a second computation.
    let row = rendered_row(&session, &stimuli(Some("absolute-centre")), EVEN_WIDTH);
    assert_eq!(usize::from(absolute[0].0) + 1, list_column(&row), "{row}");
}
