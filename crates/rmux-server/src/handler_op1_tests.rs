//! `other-pane-width` and `other-pane-height` sizing the secondary band.
//!
//! tmux does not carry the band as its own size: it turns a valid other into a
//! main (`layout-set.c:439-455`). With `A` the axis minus its border and `M`
//! the resolved main, a valid `O` with `A - O >= M` makes the main `A - O`, so
//! the band the core derives as `D - main - 1` is exactly `O`. Zero, an
//! unreadable value and a conflict all leave the main alone.
//!
//! At 121x41 the source-derived cells are: main 80 with other 20 gives 100 and
//! 20; main height 24 with other 10 gives 30 and 10; main 50% with other 25%
//! gives 90 and 30; and other 75% cannot push main 60 down, so it stays 60.

use super::RequestHandler;
use rmux_core::command_parser::{CommandParser, ParsedCommands};
use rmux_core::{FloatingPaneRect, PaneGeometry, PaneId};
use rmux_proto::{
    KillSessionRequest, LayoutName, NewSessionRequest, NewWindowRequest, NextLayoutRequest,
    PaneTarget, PreviousLayoutRequest, Request, ResizeWindowRequest, Response, SelectLayoutRequest,
    SelectLayoutTarget, SessionName, SplitWindowRequest, SplitWindowTarget, TerminalSize,
    WindowTarget,
};

const WINDOW: TerminalSize = TerminalSize {
    cols: 121,
    rows: 41,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn parse_command(command: &str) -> ParsedCommands {
    CommandParser::new()
        .parse_one_group(command)
        .expect("command parses")
}

async fn run_command(handler: &RequestHandler, command: &str) {
    handler
        .execute_parsed_commands_for_test(std::process::id(), parse_command(command))
        .await
        .unwrap_or_else(|error| panic!("`{command}` succeeds: {error:?}"));
}

async fn session_with_panes(handler: &RequestHandler, name: &SessionName, panes: usize) {
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name.clone(),
                detached: true,
                size: Some(WINDOW),
                environment: None,
            }))
            .await,
    );
    for _ in 1..panes {
        assert!(matches!(
            handler
                .handle(Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Pane(PaneTarget::new(name.clone(), 0)),
                    direction: rmux_proto::SplitDirection::Vertical,
                    before: false,
                    environment: None,
                }))
                .await,
            Response::SplitWindow(_)
        ));
    }
}

async fn kill_session(handler: &RequestHandler, name: &SessionName) {
    assert!(matches!(
        handler
            .handle(Request::KillSession(KillSessionRequest {
                target: name.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await,
        Response::KillSession(_)
    ));
}

async fn select(handler: &RequestHandler, name: &SessionName, layout: LayoutName) {
    assert_eq!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(name.clone())),
                layout,
            }))
            .await,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse { layout })
    );
}

async fn geometries(
    handler: &RequestHandler,
    name: &SessionName,
    window_index: u32,
) -> Vec<PaneGeometry> {
    let state = handler.state.lock().await;
    let session = state.sessions.session(name).expect("session exists");
    let window = session.window_at(window_index).expect("window exists");
    (0..window.pane_count() as u32)
        .map(|index| window.pane(index).expect("pane exists").geometry())
        .collect()
}

async fn pane_ids(handler: &RequestHandler, name: &SessionName, window_index: u32) -> Vec<PaneId> {
    let state = handler.state.lock().await;
    let session = state.sessions.session(name).expect("session exists");
    let window = session.window_at(window_index).expect("window exists");
    (0..window.pane_count() as u32)
        .map(|index| window.pane(index).expect("pane exists").id())
        .collect()
}

async fn active_window_index(handler: &RequestHandler, name: &SessionName) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(name)
        .expect("session exists")
        .active_window_index()
}

/// The main pane's extent and the secondary band's, on the axis the layout uses.
async fn main_and_band(
    handler: &RequestHandler,
    name: &SessionName,
    window_index: u32,
    vertical: bool,
) -> (u16, u16) {
    let geometry = geometries(handler, name, window_index).await;
    if vertical {
        (geometry[0].cols(), geometry[1].cols())
    } else {
        (geometry[0].rows(), geometry[1].rows())
    }
}

/// Applies one pair of option values on a fresh four-pane session.
async fn cell(main: Option<&str>, other: Option<&str>, layout: LayoutName) -> (u16, u16) {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;
    let vertical = matches!(
        layout,
        LayoutName::MainVertical | LayoutName::MainVerticalMirrored
    );
    let (main_option, other_option) = if vertical {
        ("main-pane-width", "other-pane-width")
    } else {
        ("main-pane-height", "other-pane-height")
    };
    if let Some(main) = main {
        run_command(
            &handler,
            &format!("set-window-option -t alpha:0 {main_option} '{main}'"),
        )
        .await;
    }
    if let Some(other) = other {
        run_command(
            &handler,
            &format!("set-window-option -t alpha:0 {other_option} '{other}'"),
        )
        .await;
    }

    select(&handler, &alpha, layout).await;

    let measured = main_and_band(&handler, &alpha, 0, vertical).await;
    kill_session(&handler, &alpha).await;
    measured
}

#[tokio::test]
async fn the_source_derived_cells_hold_on_both_axes() {
    // main 80 by default, other 20: the band is 20 and the main takes the rest.
    assert_eq!(
        cell(None, Some("20"), LayoutName::MainVertical).await,
        (100, 20)
    );
    // main height 24 by default, other 10.
    assert_eq!(
        cell(None, Some("10"), LayoutName::MainHorizontal).await,
        (30, 10)
    );
    // main 50% resolves to 60, other 25% to 30.
    assert_eq!(
        cell(Some("50%"), Some("25%"), LayoutName::MainVertical).await,
        (90, 30)
    );
    // other 75% would leave 30 for a main that asked for 60: the main wins.
    assert_eq!(
        cell(Some("50%"), Some("75%"), LayoutName::MainVertical).await,
        (60, 60)
    );
}

#[tokio::test]
async fn the_four_main_layouts_all_read_the_other_option() {
    for layout in [LayoutName::MainVertical, LayoutName::MainVerticalMirrored] {
        assert_eq!(
            cell(None, Some("20"), layout).await,
            (100, 20),
            "{layout:?}"
        );
    }
    for layout in [
        LayoutName::MainHorizontal,
        LayoutName::MainHorizontalMirrored,
    ] {
        assert_eq!(cell(None, Some("10"), layout).await, (30, 10), "{layout:?}");
    }
}

/// Zero and every unreadable spelling leave the main exactly where the MP1
/// contract put it, on both axes.
#[tokio::test]
async fn zero_and_unreadable_others_leave_the_main_alone() {
    let width_without_other = cell(None, None, LayoutName::MainVertical).await;
    let height_without_other = cell(None, None, LayoutName::MainHorizontal).await;

    for value in [
        "0",
        "0%",
        "-0",
        "-0%",
        "abc",
        "++20",
        "-+20",
        "20%%",
        "2%0",
        "%",
        "99999999999999",
        "121",
        "20 ",
    ] {
        assert_eq!(
            cell(None, Some(value), LayoutName::MainVertical).await,
            width_without_other,
            "width other {value:?}"
        );
        assert_eq!(
            cell(None, Some(value), LayoutName::MainHorizontal).await,
            height_without_other,
            "height other {value:?}"
        );
    }
}

/// The `strtonum` lexis the source uses for this option: a leading blank and a
/// single sign are readable, the axis is the bound.
#[tokio::test]
async fn the_other_lexis_follows_the_source() {
    assert_eq!(
        cell(None, Some(" 20"), LayoutName::MainVertical).await,
        (100, 20)
    );
    assert_eq!(
        cell(None, Some("+20"), LayoutName::MainVertical).await,
        (100, 20)
    );
    assert_eq!(
        cell(None, Some("020"), LayoutName::MainVertical).await,
        (100, 20)
    );
    assert_eq!(
        cell(None, Some(" 25%"), LayoutName::MainVertical).await,
        (90, 30)
    );
    assert_eq!(
        cell(None, Some("+25%"), LayoutName::MainVertical).await,
        (90, 30)
    );
    // 120 is the whole available axis: it leaves nothing for a main of 80.
    assert_eq!(
        cell(None, Some("120"), LayoutName::MainVertical).await,
        cell(None, None, LayoutName::MainVertical).await
    );
}

/// A main that already fills the axis keeps the existing clamp; the other
/// option does not reopen it.
#[tokio::test]
async fn a_main_that_fills_the_axis_keeps_the_existing_clamp() {
    let clamped = cell(Some("119"), None, LayoutName::MainVertical).await;
    assert_eq!(
        cell(Some("119"), Some("20"), LayoutName::MainVertical).await,
        clamped
    );
    assert_eq!(
        cell(Some("100%"), Some("20"), LayoutName::MainVertical).await,
        cell(Some("100%"), None, LayoutName::MainVertical).await
    );
}

#[tokio::test]
async fn a_global_other_applies_and_a_local_one_wins() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;

    run_command(&handler, "set-option -gw other-pane-width 20").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));

    run_command(&handler, "set-window-option -t alpha:0 other-pane-width 30").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (90, 30));

    run_command(&handler, "set-window-option -u -t alpha:0 other-pane-width").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));

    run_command(&handler, "set-option -gw other-pane-width 0").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (80, 40));
}

/// `set-option` keeps its `EFFECT_NONE`: the value changes and the geometry
/// does not, until the next selection.
#[tokio::test]
async fn a_hot_other_is_read_at_the_next_selection_and_not_before() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    let before = main_and_band(&handler, &alpha, 0, true).await;

    run_command(&handler, "set-window-option -t alpha:0 other-pane-width 20").await;
    assert_eq!(
        main_and_band(&handler, &alpha, 0, true).await,
        before,
        "the hot set must not relayout on its own"
    );

    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));
}

#[tokio::test]
async fn next_and_previous_layout_reaching_main_read_the_other_option() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;
    run_command(&handler, "set-window-option -t alpha:0 other-pane-width 20").await;
    select(&handler, &alpha, LayoutName::MainHorizontalMirrored).await;

    assert_eq!(
        handler
            .handle(Request::NextLayout(NextLayoutRequest {
                target: WindowTarget::new(alpha.clone()),
            }))
            .await,
        Response::NextLayout(rmux_proto::NextLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));

    select(&handler, &alpha, LayoutName::MainVerticalMirrored).await;
    assert_eq!(
        handler
            .handle(Request::PreviousLayout(PreviousLayoutRequest {
                target: WindowTarget::new(alpha.clone()),
            }))
            .await,
        Response::PreviousLayout(rmux_proto::PreviousLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));
}

/// Adds a second, differently sized window and makes it the active one.
async fn make_second_window_active(handler: &RequestHandler, name: &SessionName) {
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: name.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(name.clone(), 1, 0)),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::ResizeWindow(ResizeWindowRequest {
                target: WindowTarget::with_window(name.clone(), 1),
                width: Some(81),
                height: Some(41),
                adjustment: None,
            }))
            .await,
        Response::ResizeWindow(_)
    ));
    run_command(handler, &format!("select-window -t {name}:1")).await;
}

/// One parsed queue resizes the inactive target and then selects on it, so the
/// other option is applied against the axis that same queue produced.
#[tokio::test]
async fn a_parsed_queue_applies_the_other_option_on_the_fresh_axis() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;
    let target_ids = pane_ids(&handler, &alpha, 0).await;
    make_second_window_active(&handler, &alpha).await;
    let neighbour_before = geometries(&handler, &alpha, 1).await;
    let neighbour_ids = pane_ids(&handler, &alpha, 1).await;
    run_command(&handler, "set-option -gw main-pane-width 40").await;
    run_command(&handler, "set-option -gw other-pane-width 20").await;

    // On the window's own 121 axis the same pair gives a main of 100.
    assert_eq!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
                layout: LayoutName::MainVertical,
            }))
            .await,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));

    let queue = parse_command(
        "resize-window -t alpha:0 -x 83 -y 31 ; select-layout -t alpha:0 main-vertical",
    );
    assert_eq!(queue.commands().len(), 2);
    handler
        .execute_parsed_commands_for_test(std::process::id(), queue)
        .await
        .expect("the queue succeeds");

    // A is 82 after the queue, so the same band of 20 now leaves a main of 62:
    // the option was applied against the axis the queue itself produced.
    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (62, 20));
    assert_eq!(pane_ids(&handler, &alpha, 0).await, target_ids);
    assert_eq!(active_window_index(&handler, &alpha).await, 1);
    assert_eq!(geometries(&handler, &alpha, 1).await, neighbour_before);
    assert_eq!(pane_ids(&handler, &alpha, 1).await, neighbour_ids);
}

#[tokio::test]
async fn a_linked_alias_applies_the_other_option_on_the_canonical_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    session_with_panes(&handler, &alpha, 4).await;
    let canonical_ids = pane_ids(&handler, &alpha, 0).await;
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: beta.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 81, rows: 41 }),
                environment: None,
            }))
            .await,
    );
    let beta_active_before = geometries(&handler, &beta, 0).await;
    run_command(&handler, "link-window -d -s alpha:0 -t beta:1").await;
    run_command(&handler, "set-option -gw other-pane-width 20").await;

    assert_eq!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::with_window(beta.clone(), 1)),
                layout: LayoutName::MainVertical,
            }))
            .await,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );

    assert_eq!(
        main_and_band(&handler, &beta, 1, true).await,
        (100, 20),
        "the canonical 121 axis, not beta's active 81"
    );
    assert_eq!(
        geometries(&handler, &alpha, 0).await,
        geometries(&handler, &beta, 1).await
    );
    assert_eq!(pane_ids(&handler, &beta, 1).await, canonical_ids);
    assert_eq!(geometries(&handler, &beta, 0).await, beta_active_before);
    assert_eq!(active_window_index(&handler, &beta).await, 0);
}

#[tokio::test]
async fn a_floating_pane_stays_intact_across_an_other_selection() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 5).await;
    let (floating_id, floating_geometry) = {
        let mut state = handler.state.lock().await;
        let mut window = state
            .sessions
            .window_layout_context_mut(&WindowTarget::new(alpha.clone()))
            .expect("window layout context");
        window
            .float_pane(4, FloatingPaneRect::new(10, 6, 30, 10).expect("valid rect"))
            .expect("pane 4 floats");
        let session = state.sessions.session(&alpha).expect("session exists");
        let window = session.window_at(0).expect("window exists");
        let pane = window.pane(4).expect("pane 4 exists");
        (pane.id(), pane.geometry())
    };

    run_command(&handler, "set-window-option -t alpha:0 other-pane-width 20").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;

    assert_eq!(main_and_band(&handler, &alpha, 0, true).await, (100, 20));
    assert_eq!(geometries(&handler, &alpha, 0).await[4], floating_geometry);
    assert_eq!(pane_ids(&handler, &alpha, 0).await[4], floating_id);
}

/// The MP1 contract is untouched where the other option does not act.
#[tokio::test]
async fn the_main_only_contract_is_unchanged_without_an_other_value() {
    assert_eq!(
        cell(Some("50%"), None, LayoutName::MainVertical).await.0,
        60
    );
    assert_eq!(cell(Some("30"), None, LayoutName::MainVertical).await.0, 30);
    assert_eq!(
        cell(Some("abc"), None, LayoutName::MainVertical).await.0,
        80
    );
    assert_eq!(
        cell(Some("25%"), None, LayoutName::MainHorizontal).await.0,
        10
    );
}
