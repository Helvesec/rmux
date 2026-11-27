//! `#{pane_zoomed_flag}` marks the one pane a zoomed window is showing.
//!
//! The expectations come from the primary text, never from the accessor under
//! test. tmux answers `1` when THAT pane carries `PANE_ZOOMED`, `0` otherwise,
//! and nothing at all without a pane (`format.c:2444-2456`); `window_zoom` sets
//! that flag on the one pane it makes active and `WINDOW_ZOOMED` on the window
//! (`window.c:719-745`), and `window_unzoom` clears both again
//! (`window.c:747-771`). Measured once on the pinned tmux 3.7b before these
//! witnesses were written (`pzoom1/native/oracle2.txt`): two tiled
//! panes answer `0` and `0`, the zoomed one answers `1` while its neighbour
//! still answers `0`, and both answer `0` again once the window is unzoomed.
//!
//! The fixture is the store the server itself uses, zoomed through the same
//! layout entry the resize path calls, and the reading is the runtime
//! resolution every caller goes through - never a mirror of the getter. The
//! panes are the ones the fixture returned, taken by their own identities.

use rmux_core::formats::{FormatContext, FormatVariables};
use rmux_core::{Pane, PaneId};
use rmux_proto::{SessionName, TerminalSize};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::session_fixture::SessionFixture;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

/// A window of two panes, in the store the server itself keeps.
fn two_panes(name: &str) -> (SessionFixture, PaneId, PaneId) {
    let mut fixture = SessionFixture::new(session_name(name), SIZE);
    let first = fixture
        .window()
        .panes()
        .first()
        .map(Pane::id)
        .expect("the session starts with one pane");
    let second_index = fixture.split_active_pane().expect("a second pane");
    let second = fixture
        .window()
        .panes()
        .iter()
        .find(|pane| pane.index() == second_index)
        .map(Pane::id)
        .expect("the split reported the pane it created");
    assert_ne!(first, second, "the fixture holds two distinct panes");
    (fixture, first, second)
}

/// What the runtime answers for that pane, through the real resolution.
fn flag_of(fixture: &SessionFixture, pane: PaneId) -> Option<String> {
    let view = fixture.view();
    let window_index = view
        .window_index_for_pane_id(pane)
        .expect("the fixture pane is in a window");
    let window = view.window_at(window_index).expect("the window");
    let pane = window
        .panes()
        .iter()
        .find(|candidate| candidate.id() == pane)
        .expect("the pane");
    RuntimeFormatContext::new(FormatContext::new())
        .with_session(view)
        .with_window(window_index, window)
        .with_pane(pane)
        .format_value_by_name("pane_zoomed_flag")
}

/// What the window answers for itself, in its own context.
fn window_flag(fixture: &SessionFixture) -> Option<String> {
    let view = fixture.view();
    let window_index = view.active_window_index();
    let window = view.window_at(window_index).expect("the window");
    RuntimeFormatContext::new(FormatContext::new())
        .with_session(view)
        .with_window(window_index, window)
        .format_value_by_name("window_zoomed_flag")
}

#[test]
fn the_zoomed_pane_is_the_one_the_window_holds_and_its_neighbour_is_not() {
    let (mut fixture, first, second) = two_panes("pzoom-marked");
    let window_index = fixture.active_window_index();
    let first_index = fixture
        .window()
        .panes()
        .iter()
        .find(|pane| pane.id() == first)
        .map(Pane::index)
        .expect("the first pane is in the window");

    fixture
        .toggle_zoom_in_window(window_index, first_index)
        .expect("the window zooms that pane");
    assert!(
        fixture.window().is_zoomed(),
        "the fixture is actually zoomed before anything is read"
    );

    // The pane the window is showing, first and before any guard.
    assert_eq!(flag_of(&fixture, first).as_deref(), Some("1"));
    // The pane beside it is not the one being shown.
    assert_eq!(flag_of(&fixture, second).as_deref(), Some("0"));
    // And the window says so for itself, in its own context.
    assert_eq!(window_flag(&fixture).as_deref(), Some("1"));

    fixture
        .toggle_zoom_in_window(window_index, first_index)
        .expect("the window unzooms");
    assert!(!fixture.window().is_zoomed(), "the fixture is back");
    assert_eq!(flag_of(&fixture, first).as_deref(), Some("0"));
    assert_eq!(flag_of(&fixture, second).as_deref(), Some("0"));
    assert_eq!(window_flag(&fixture).as_deref(), Some("0"));
}

#[test]
fn a_format_without_a_pane_has_no_zoom_to_report() {
    let (mut fixture, first, _second) = two_panes("pzoom-absent");
    let window_index = fixture.active_window_index();
    let first_index = fixture
        .window()
        .panes()
        .iter()
        .find(|pane| pane.id() == first)
        .map(Pane::index)
        .expect("the first pane is in the window");
    fixture
        .toggle_zoom_in_window(window_index, first_index)
        .expect("the window zooms that pane");

    // A format that never reaches a pane has no pane to report a zoom for, and
    // must not borrow the mark of the pane the window is showing. The empty
    // string it renders is the registry's known-empty fallback, which every
    // tmux format-table variable shares.
    let view = fixture.view();
    let without_pane = RuntimeFormatContext::new(FormatContext::new())
        .with_session(view)
        .with_window(window_index, view.window_at(window_index).expect("window"));
    assert_eq!(
        render_runtime_template("[#{pane_zoomed_flag}]", &without_pane, false),
        "[]"
    );
    assert_ne!(
        without_pane
            .format_value_by_name("pane_zoomed_flag")
            .as_deref(),
        Some("1")
    );
}
