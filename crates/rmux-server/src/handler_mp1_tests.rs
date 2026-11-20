//! `main-pane-width` and `main-pane-height` expressed as percentages.
//!
//! The resolved value follows `args_string_percentage` as the pinned tmux 3.7b
//! source calls it from `layout-set.c`: the axis of the canonical window minus
//! the one cell taken off for the separating border, then
//! `floor(available * percent / 100)`. At 121x41 that makes `main-pane-width
//! 50%` resolve to 60 and `main-pane-height 25%` resolve to 10.

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

/// Adds a second, differently sized window and makes it the active one, so the
/// first window becomes a genuinely inactive target.
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

async fn active_window_index(handler: &RequestHandler, name: &SessionName) -> u32 {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(name)
        .expect("session exists")
        .active_window_index()
}

/// The value the registry actually stored, not just the option name.
async fn stored_option(handler: &RequestHandler, target: &str, option: &str) -> String {
    let output = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            parse_command(&format!("show-window-options -t {target} {option}")),
        )
        .await
        .expect("show-window-options succeeds");
    String::from_utf8_lossy(output.stdout()).trim().to_owned()
}

async fn window_size(
    handler: &RequestHandler,
    name: &SessionName,
    window_index: u32,
) -> TerminalSize {
    let state = handler.state.lock().await;
    state
        .sessions
        .session(name)
        .expect("session exists")
        .window_at(window_index)
        .expect("window exists")
        .size()
}

/// Releases a session's panes before the next iteration builds its own.
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

/// The width of the main pane, which is the value the option controls.
async fn main_width(handler: &RequestHandler, name: &SessionName) -> u16 {
    geometries(handler, name, 0).await[0].cols()
}

async fn main_height(handler: &RequestHandler, name: &SessionName) -> u16 {
    geometries(handler, name, 0).await[0].rows()
}

async fn pane_ids(handler: &RequestHandler, name: &SessionName, window_index: u32) -> Vec<PaneId> {
    let state = handler.state.lock().await;
    let session = state.sessions.session(name).expect("session exists");
    let window = session.window_at(window_index).expect("window exists");
    (0..window.pane_count() as u32)
        .map(|index| window.pane(index).expect("pane exists").id())
        .collect()
}

#[tokio::test]
async fn a_width_percentage_resolves_against_the_window_axis_minus_the_border() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;

    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 50%").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;

    assert_eq!(main_width(&handler, &alpha).await, 60);
}

#[tokio::test]
async fn a_height_percentage_resolves_against_the_window_axis_minus_the_border() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;

    run_command(
        &handler,
        "set-window-option -t alpha:0 main-pane-height 25%",
    )
    .await;
    select(&handler, &alpha, LayoutName::MainHorizontal).await;

    assert_eq!(main_height(&handler, &alpha).await, 10);
}

#[tokio::test]
async fn the_four_main_layouts_all_read_the_percentage() {
    for (layout, value, expected) in [
        (LayoutName::MainVertical, "main-pane-width 50%", 60u16),
        (LayoutName::MainVerticalMirrored, "main-pane-width 50%", 60),
        (LayoutName::MainHorizontal, "main-pane-height 25%", 10),
        (
            LayoutName::MainHorizontalMirrored,
            "main-pane-height 25%",
            10,
        ),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        session_with_panes(&handler, &alpha, 2).await;
        run_command(&handler, &format!("set-window-option -t alpha:0 {value}")).await;

        select(&handler, &alpha, layout).await;

        let main = geometries(&handler, &alpha, 0).await[0];
        let actual = if value.starts_with("main-pane-width") {
            main.cols()
        } else {
            main.rows()
        };
        assert_eq!(actual, expected, "{layout:?}");
        kill_session(&handler, &alpha).await;
    }
}

/// The spellings measured on the pinned tmux 3.7b at 121x41, plus the rounding
/// of an odd percentage which is the same integer division.
#[tokio::test]
async fn the_measured_percentage_spellings_resolve_as_the_oracle_did() {
    for (value, expected) in [
        ("50%", 60u16),
        (" 50%", 60),
        ("+50%", 60),
        ("050%", 60),
        // The option resolves to the whole available axis, 120; the layout's
        // own clamp then leaves 119, the width the oracle measured for `100%`.
        ("100%", 119),
        ("33%", 39),
        ("1%", 1),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        session_with_panes(&handler, &alpha, 2).await;
        run_command(
            &handler,
            &format!("set-window-option -t alpha:0 main-pane-width '{value}'"),
        )
        .await;

        select(&handler, &alpha, LayoutName::MainVertical).await;

        assert_eq!(
            main_width(&handler, &alpha).await,
            expected,
            "value {value:?}"
        );
        kill_session(&handler, &alpha).await;
    }
}

/// Every spelling the source rejects keeps the layout's own default, which is
/// exactly what an unset option produces. No value is refused at `set-option`.
#[tokio::test]
async fn rejected_spellings_keep_the_default_and_are_still_accepted_by_set_option() {
    let handler = RequestHandler::new();
    let unset = session_name("unset");
    session_with_panes(&handler, &unset, 2).await;
    select(&handler, &unset, LayoutName::MainVertical).await;
    let default_width = main_width(&handler, &unset).await;
    kill_session(&handler, &unset).await;

    for value in ["-5%", "101%", "50%%", "5%0", "%", "abc", "50% "] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        session_with_panes(&handler, &alpha, 2).await;
        run_command(
            &handler,
            &format!("set-window-option -t alpha:0 main-pane-width '{value}'"),
        )
        .await;
        let stored = handler
            .execute_parsed_commands_for_test(
                std::process::id(),
                parse_command("show-window-options -t alpha:0 main-pane-width"),
            )
            .await
            .expect("show-window-options succeeds");
        assert!(
            String::from_utf8_lossy(stored.stdout()).contains("main-pane-width"),
            "the value stays stored, {value:?}"
        );

        select(&handler, &alpha, LayoutName::MainVertical).await;

        assert_eq!(
            main_width(&handler, &alpha).await,
            default_width,
            "value {value:?} keeps the default"
        );
        kill_session(&handler, &alpha).await;
    }
}

#[tokio::test]
async fn an_absolute_value_keeps_its_current_meaning() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;

    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 30").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;

    assert_eq!(main_width(&handler, &alpha).await, 30);
}

#[tokio::test]
async fn a_global_window_value_applies_and_a_local_one_wins() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;

    run_command(&handler, "set-option -gw main-pane-width 50%").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_width(&handler, &alpha).await, 60);

    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 25%").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_width(&handler, &alpha).await, 30);

    run_command(&handler, "set-window-option -u -t alpha:0 main-pane-width").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_width(&handler, &alpha).await, 60);
}

/// `set-option` keeps its `EFFECT_NONE`: the stored value changes and the
/// geometry does not until the next selection.
#[tokio::test]
async fn a_hot_change_is_read_at_the_next_selection_and_not_before() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;
    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 50%").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_width(&handler, &alpha).await, 60);

    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 25%").await;
    assert_eq!(
        main_width(&handler, &alpha).await,
        60,
        "the hot set must not relayout on its own"
    );

    select(&handler, &alpha, LayoutName::MainVertical).await;
    assert_eq!(main_width(&handler, &alpha).await, 30);
}

/// The percentage is a percentage of the window as it is now, never of a value
/// cached from an earlier size.
///
/// The resize and the selection travel together in one parsed queue, so the
/// axis the selection reads is the one the queue's own resize just produced,
/// and the addressed window is the inactive one.
#[tokio::test]
async fn a_parsed_queue_resizes_then_selects_on_a_really_inactive_target() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 2).await;
    let target_ids = pane_ids(&handler, &alpha, 0).await;
    make_second_window_active(&handler, &alpha).await;
    let neighbour_before = geometries(&handler, &alpha, 1).await;
    let neighbour_ids = pane_ids(&handler, &alpha, 1).await;
    run_command(&handler, "set-option -gw main-pane-width 50%").await;

    // One parsed group: the resize and the selection are the same queue.
    let queue = parse_command(
        "resize-window -t alpha:0 -x 83 -y 31 ; select-layout -t alpha:0 main-vertical",
    );
    assert_eq!(
        queue.commands().len(),
        2,
        "the queue really holds both commands"
    );
    handler
        .execute_parsed_commands_for_test(std::process::id(), queue)
        .await
        .expect("the queue succeeds");

    assert_eq!(
        window_size(&handler, &alpha, 0).await,
        TerminalSize { cols: 83, rows: 31 }
    );
    assert_eq!(
        geometries(&handler, &alpha, 0).await[0].cols(),
        41,
        "the selection read the axis the same queue had just resized, 83 - 1 halved"
    );
    assert_eq!(pane_ids(&handler, &alpha, 0).await, target_ids);
    assert_eq!(
        active_window_index(&handler, &alpha).await,
        1,
        "the queue did not move the selection"
    );
    assert_eq!(geometries(&handler, &alpha, 1).await, neighbour_before);
    assert_eq!(pane_ids(&handler, &alpha, 1).await, neighbour_ids);
}

/// The addressed window is genuinely inactive and the active one has a
/// different size, so the axis can only have come from the addressed window.
#[tokio::test]
async fn a_window_that_is_really_inactive_uses_its_own_axis() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 2).await;
    let target_ids = pane_ids(&handler, &alpha, 0).await;
    make_second_window_active(&handler, &alpha).await;
    let neighbour_before = geometries(&handler, &alpha, 1).await;
    let neighbour_ids = pane_ids(&handler, &alpha, 1).await;

    assert_eq!(active_window_index(&handler, &alpha).await, 1);
    assert_eq!(
        window_size(&handler, &alpha, 0).await,
        TerminalSize {
            cols: 121,
            rows: 41
        }
    );
    assert_eq!(
        window_size(&handler, &alpha, 1).await,
        TerminalSize { cols: 81, rows: 41 },
        "the active neighbour is a different size"
    );

    run_command(&handler, "set-option -gw main-pane-width 50%").await;
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

    assert_eq!(
        geometries(&handler, &alpha, 0).await[0].cols(),
        60,
        "121 - 1 halved, not the active window's 81"
    );
    assert_eq!(pane_ids(&handler, &alpha, 0).await, target_ids);
    assert_eq!(active_window_index(&handler, &alpha).await, 1);
    assert_eq!(geometries(&handler, &alpha, 1).await, neighbour_before);
    assert_eq!(pane_ids(&handler, &alpha, 1).await, neighbour_ids);
}

/// A window linked into a second session is one canonical window seen through
/// two aliases: addressing it from either alias resolves on that canonical
/// size, and the other session's differently sized active window is untouched.
#[tokio::test]
async fn a_linked_alias_resolves_on_the_canonical_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    session_with_panes(&handler, &alpha, 2).await;
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
    let beta_active_ids = pane_ids(&handler, &beta, 0).await;
    run_command(&handler, "link-window -d -s alpha:0 -t beta:1").await;

    assert_eq!(
        window_size(&handler, &beta, 1).await,
        TerminalSize {
            cols: 121,
            rows: 41
        },
        "the alias sees the canonical window"
    );
    assert_eq!(pane_ids(&handler, &beta, 1).await, canonical_ids);
    assert_eq!(active_window_index(&handler, &beta).await, 0);

    run_command(&handler, "set-option -gw main-pane-width 50%").await;
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
        geometries(&handler, &beta, 1).await[0].cols(),
        60,
        "the canonical 121 - 1 halved, not beta's active 81"
    );
    assert_eq!(
        geometries(&handler, &alpha, 0).await,
        geometries(&handler, &beta, 1).await,
        "both aliases show the same canonical window"
    );
    assert_eq!(pane_ids(&handler, &alpha, 0).await, canonical_ids);
    assert_eq!(pane_ids(&handler, &beta, 1).await, canonical_ids);
    assert_eq!(geometries(&handler, &beta, 0).await, beta_active_before);
    assert_eq!(pane_ids(&handler, &beta, 0).await, beta_active_ids);
    assert_eq!(active_window_index(&handler, &beta).await, 0);
}

#[tokio::test]
async fn next_and_previous_layout_reaching_main_use_the_percentage() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 4).await;
    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 50%").await;
    select(&handler, &alpha, LayoutName::MainHorizontalMirrored).await;

    let next = handler
        .handle(Request::NextLayout(NextLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;
    assert_eq!(
        next,
        Response::NextLayout(rmux_proto::NextLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );
    assert_eq!(main_width(&handler, &alpha).await, 60);

    select(&handler, &alpha, LayoutName::MainVerticalMirrored).await;
    let previous = handler
        .handle(Request::PreviousLayout(PreviousLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;
    assert_eq!(
        previous,
        Response::PreviousLayout(rmux_proto::PreviousLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );
    assert_eq!(main_width(&handler, &alpha).await, 60);
}

/// A single pane keeps the whole window, as the source returns before reading
/// the option at all.
#[tokio::test]
async fn a_single_pane_window_is_untouched_by_a_percentage() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 1).await;
    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 50%").await;

    select(&handler, &alpha, LayoutName::MainVertical).await;

    assert_eq!(
        geometries(&handler, &alpha, 0).await,
        vec![PaneGeometry::new(0, 0, 121, 41)]
    );
}

/// A floating pane keeps its geometry and identity and is not one of the tiles
/// the percentage sizes. No floating primitive is touched here.
#[tokio::test]
async fn a_floating_pane_stays_intact_across_a_percentage_selection() {
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

    run_command(&handler, "set-window-option -t alpha:0 main-pane-width 50%").await;
    select(&handler, &alpha, LayoutName::MainVertical).await;

    let geometry = geometries(&handler, &alpha, 0).await;
    assert_eq!(
        geometry[0].cols(),
        60,
        "the tiled main pane follows the option"
    );
    assert_eq!(
        geometry[4], floating_geometry,
        "the floating pane is untouched"
    );
    assert_eq!(pane_ids(&handler, &alpha, 0).await[4], floating_id);
}

/// The lexical edges of the percentage part, on both axes, with the value the
/// registry really stored checked alongside the resolution.
///
/// `0%` and `-0%` are numeric zeroes and must resolve identically and
/// differently from the fallback; a doubled sign, an overflow and a trailing
/// blank must all keep the fallback, and a leading blank must resolve, as the
/// pinned oracle measured.
#[tokio::test]
async fn the_lexical_edges_resolve_or_fall_back_on_both_axes() {
    for (option, layout, axis) in [
        ("main-pane-width", LayoutName::MainVertical, Axis::Width),
        ("main-pane-height", LayoutName::MainHorizontal, Axis::Height),
    ] {
        let fallback = lexical_case(option, layout, axis, None).await;
        let zero = lexical_case(option, layout, axis, Some("0%")).await;
        let negative_zero = lexical_case(option, layout, axis, Some("-0%")).await;
        let leading_blank = lexical_case(option, layout, axis, Some(" 50%")).await;
        let half = lexical_case(option, layout, axis, Some("50%")).await;

        assert_eq!(zero.0, negative_zero.0, "{option}: 0% and -0% agree");
        assert_ne!(
            zero.0, fallback.0,
            "{option}: a numeric zero is not the fallback"
        );
        assert_eq!(
            leading_blank.0, half.0,
            "{option}: a leading blank still resolves"
        );
        assert_ne!(half.0, fallback.0, "{option}: 50% resolves");

        for value in [
            "++50%",
            "-+0%",
            "+-50%",
            "99999999999999%",
            "4294967296%",
            "50% ",
        ] {
            let (resolved, stored) = lexical_case(option, layout, axis, Some(value)).await;
            assert_eq!(
                resolved, fallback.0,
                "{option} {value:?} keeps the fallback"
            );
            assert_eq!(
                stored,
                format!("{option} \"{value}\""),
                "{option} {value:?} is stored unchanged"
            );
        }

        assert_eq!(zero.1, format!("{option} \"0%\""));
        assert_eq!(negative_zero.1, format!("{option} \"-0%\""));
        assert_eq!(half.1, format!("{option} \"50%\""));
    }
}

#[derive(Clone, Copy)]
enum Axis {
    Width,
    Height,
}

/// Applies one spelling on a fresh two-pane session and returns the resolved
/// main-pane extent together with the value the registry stored.
async fn lexical_case(
    option: &str,
    layout: LayoutName,
    axis: Axis,
    value: Option<&str>,
) -> (u16, String) {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    session_with_panes(&handler, &alpha, 2).await;
    let stored = if let Some(value) = value {
        run_command(
            &handler,
            &format!("set-window-option -t alpha:0 {option} '{value}'"),
        )
        .await;
        stored_option(&handler, "alpha:0", option).await
    } else {
        String::new()
    };

    select(&handler, &alpha, layout).await;

    let main = geometries(&handler, &alpha, 0).await[0];
    let resolved = match axis {
        Axis::Width => main.cols(),
        Axis::Height => main.rows(),
    };
    kill_session(&handler, &alpha).await;
    (resolved, stored)
}
