//! `tiled-layout-max-columns` resolved at the moment a layout command runs.
//!
//! The expected geometries are the cells measured on the frozen tmux 3.7b
//! oracle (`runs/tcol1/oracle/20260913T014246Z-89ac`): a 120x60 window with
//! four tiled panes gives two columns at x=0,60 and two rows at y=0,30 without
//! a bound, and one column at x=0 with four rows at y=0,15,30,45 under 1.

use super::RequestHandler;
use rmux_core::command_parser::{CommandParser, ParsedCommands};
use rmux_core::{PaneGeometry, PaneId};
use rmux_proto::{
    LayoutName, NewSessionRequest, NewWindowRequest, NextLayoutRequest, PaneTarget,
    PreviousLayoutRequest, Request, Response, SelectLayoutRequest, SelectLayoutTarget, SessionName,
    SplitWindowRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};

const WINDOW: TerminalSize = TerminalSize {
    cols: 120,
    rows: 60,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn parse_command(command: &str) -> ParsedCommands {
    CommandParser::new()
        .parse_one_group(command)
        .expect("command parses")
}

/// Two columns at x=0,60 and two rows at y=0,30.
fn oracle_two_by_two() -> Vec<PaneGeometry> {
    vec![
        PaneGeometry::new(0, 0, 59, 29),
        PaneGeometry::new(60, 0, 60, 29),
        PaneGeometry::new(0, 30, 59, 30),
        PaneGeometry::new(60, 30, 60, 30),
    ]
}

/// One column at x=0 and four rows at y=0,15,30,45.
fn oracle_one_by_four() -> Vec<PaneGeometry> {
    vec![
        PaneGeometry::new(0, 0, 120, 14),
        PaneGeometry::new(0, 15, 120, 14),
        PaneGeometry::new(0, 30, 120, 14),
        PaneGeometry::new(0, 45, 120, 15),
    ]
}

async fn run_command(handler: &RequestHandler, command: &str) {
    handler
        .execute_parsed_commands_for_test(std::process::id(), parse_command(command))
        .await
        .unwrap_or_else(|error| panic!("`{command}` succeeds: {error:?}"));
}

async fn four_pane_session(handler: &RequestHandler, name: &SessionName) {
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
    for _ in 0..3 {
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

async fn select_tiled(handler: &RequestHandler, name: &SessionName) {
    assert_eq!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(name.clone())),
                layout: LayoutName::Tiled,
            }))
            .await,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );
}

#[tokio::test]
async fn select_layout_tiled_follows_the_window_local_bound_for_each_oracle_value() {
    for (value, expected) in [
        ("0", oracle_two_by_two()),
        ("1", oracle_one_by_four()),
        ("2", oracle_two_by_two()),
        ("65535", oracle_two_by_two()),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        four_pane_session(&handler, &alpha).await;
        let identities = pane_ids(&handler, &alpha, 0).await;

        run_command(
            &handler,
            &format!("set-window-option -t alpha:0 tiled-layout-max-columns {value}"),
        )
        .await;
        select_tiled(&handler, &alpha).await;

        assert_eq!(
            geometries(&handler, &alpha, 0).await,
            expected,
            "value {value}"
        );
        assert_eq!(
            pane_ids(&handler, &alpha, 0).await,
            identities,
            "value {value} keeps the same panes"
        );
    }
}

/// tmux 3.7b accepts 65535, rejects larger values, and keeps the previous
/// value and tiled geometry for both window-option commands and scopes.
#[tokio::test]
async fn a_value_above_the_tmux_maximum_is_rejected_without_changing_the_previous_value() {
    for (set, show) in [
        (
            "set-window-option -t alpha:0",
            "show-window-options -v -t alpha:0",
        ),
        ("set-window-option -g", "show-window-options -gv"),
        ("set-option -w -t alpha:0", "show-options -wv -t alpha:0"),
        ("set-option -gw", "show-options -gwv"),
    ] {
        let handler = RequestHandler::new();
        let alpha = session_name("alpha");
        four_pane_session(&handler, &alpha).await;
        let identities = pane_ids(&handler, &alpha, 0).await;

        for (previous, expected) in [
            ("0", oracle_two_by_two()),
            ("1", oracle_one_by_four()),
            ("2", oracle_two_by_two()),
            ("65534", oracle_two_by_two()),
            ("65535", oracle_two_by_two()),
        ] {
            run_command(
                &handler,
                &format!("{set} tiled-layout-max-columns {previous}"),
            )
            .await;
            let output = handler
                .execute_parsed_commands_for_test(
                    std::process::id(),
                    parse_command(&format!("{show} tiled-layout-max-columns")),
                )
                .await
                .expect("the accepted value can be read back");
            assert_eq!(output.stdout(), format!("{previous}\n").as_bytes(), "{set}");
            select_tiled(&handler, &alpha).await;
            assert_eq!(geometries(&handler, &alpha, 0).await, expected, "{set}");

            for value in ["65536", "65537", "131071", "2147483647"] {
                let command = format!("{set} tiled-layout-max-columns {value}");
                let error = handler
                    .execute_parsed_commands_for_test(std::process::id(), parse_command(&command))
                    .await
                    .expect_err("values above the tmux maximum are rejected");
                assert!(
                    format!("{error:?}").contains("value is too large"),
                    "{command}: unexpected refusal: {error:?}"
                );
                let output = handler
                    .execute_parsed_commands_for_test(
                        std::process::id(),
                        parse_command(&format!("{show} tiled-layout-max-columns")),
                    )
                    .await
                    .expect("the previous value remains readable after rejection");
                assert_eq!(
                    output.stdout(),
                    format!("{previous}\n").as_bytes(),
                    "{command} keeps the previous value {previous}"
                );
                assert_eq!(
                    geometries(&handler, &alpha, 0).await,
                    expected,
                    "{command} keeps the geometry for {previous} before reselection"
                );
                select_tiled(&handler, &alpha).await;
                assert_eq!(
                    geometries(&handler, &alpha, 0).await,
                    expected,
                    "{command} keeps the geometry for {previous}"
                );
                assert_eq!(pane_ids(&handler, &alpha, 0).await, identities, "{command}");
            }
        }
    }
}

/// The registry's own ceiling, recorded so the saturation range above is a
/// measured fact and not an assumption. This lot does not change it.
#[tokio::test]
async fn the_registry_refuses_a_value_above_its_own_ceiling() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;

    let error = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            parse_command("set-window-option -t alpha:0 tiled-layout-max-columns 2147483648"),
        )
        .await
        .expect_err("the registry refuses a number above i32::MAX");

    assert!(
        format!("{error:?}").contains("value is too large"),
        "unexpected refusal: {error:?}"
    );
}

#[tokio::test]
async fn the_global_window_value_applies_without_a_local_one() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;

    run_command(&handler, "set-option -gw tiled-layout-max-columns 1").await;
    select_tiled(&handler, &alpha).await;

    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_one_by_four());
}

#[tokio::test]
async fn a_local_value_wins_and_its_unset_reveals_the_global_one() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;

    run_command(&handler, "set-option -gw tiled-layout-max-columns 1").await;
    run_command(
        &handler,
        "set-window-option -t alpha:0 tiled-layout-max-columns 2",
    )
    .await;
    select_tiled(&handler, &alpha).await;
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_two_by_two());

    run_command(
        &handler,
        "set-window-option -u -t alpha:0 tiled-layout-max-columns",
    )
    .await;
    select_tiled(&handler, &alpha).await;
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_one_by_four());

    run_command(&handler, "set-option -gw tiled-layout-max-columns 0").await;
    select_tiled(&handler, &alpha).await;
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_two_by_two());
}

/// `set-option` keeps its `EFFECT_NONE`: the value changes, the geometry does
/// not, and the next selection reads the new value.
#[tokio::test]
async fn a_hot_set_takes_effect_at_the_next_selection_and_not_immediately() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;
    select_tiled(&handler, &alpha).await;
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_two_by_two());

    run_command(
        &handler,
        "set-window-option -t alpha:0 tiled-layout-max-columns 1",
    )
    .await;
    assert_eq!(
        geometries(&handler, &alpha, 0).await,
        oracle_two_by_two(),
        "the hot set must not relayout on its own"
    );

    select_tiled(&handler, &alpha).await;
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_one_by_four());
}

/// Oracle cell: `next-layout` from `main-vertical-mirrored` reaches tiled and
/// uses the bound 1.
#[tokio::test]
async fn next_layout_reaching_tiled_uses_the_bound() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
                layout: LayoutName::MainVerticalMirrored,
            }))
            .await,
        Response::SelectLayout(_)
    ));
    run_command(
        &handler,
        "set-window-option -t alpha:0 tiled-layout-max-columns 1",
    )
    .await;

    let response = handler
        .handle(Request::NextLayout(NextLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;

    assert_eq!(
        response,
        Response::NextLayout(rmux_proto::NextLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_one_by_four());
}

/// Oracle cell: `previous-layout` from `even-horizontal` reaches tiled and uses
/// the bound 2.
#[tokio::test]
async fn previous_layout_reaching_tiled_uses_the_bound() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
                layout: LayoutName::EvenHorizontal,
            }))
            .await,
        Response::SelectLayout(_)
    ));
    run_command(
        &handler,
        "set-window-option -t alpha:0 tiled-layout-max-columns 2",
    )
    .await;

    let response = handler
        .handle(Request::PreviousLayout(PreviousLayoutRequest {
            target: WindowTarget::new(alpha.clone()),
        }))
        .await;

    assert_eq!(
        response,
        Response::PreviousLayout(rmux_proto::PreviousLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_two_by_two());

    // The oracle cell above is a bound of 2, which happens to be the unbounded
    // grid as well; repeat the same cycle under 1 so the witness can fail.
    run_command(
        &handler,
        "set-window-option -t alpha:0 tiled-layout-max-columns 1",
    )
    .await;
    assert!(matches!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::new(alpha.clone())),
                layout: LayoutName::EvenHorizontal,
            }))
            .await,
        Response::SelectLayout(_)
    ));
    assert_eq!(
        handler
            .handle(Request::PreviousLayout(PreviousLayoutRequest {
                target: WindowTarget::new(alpha.clone()),
            }))
            .await,
        Response::PreviousLayout(rmux_proto::PreviousLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );
    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_one_by_four());
}

#[tokio::test]
async fn a_targeted_selection_leaves_the_other_window_alone() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    four_pane_session(&handler, &alpha).await;
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
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
    for _ in 0..3 {
        assert!(matches!(
            handler
                .handle(Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 1, 0,)),
                    direction: rmux_proto::SplitDirection::Vertical,
                    before: false,
                    environment: None,
                }))
                .await,
            Response::SplitWindow(_)
        ));
    }
    let other_geometry = geometries(&handler, &alpha, 1).await;
    let other_identities = pane_ids(&handler, &alpha, 1).await;

    run_command(&handler, "set-option -gw tiled-layout-max-columns 1").await;
    assert_eq!(
        handler
            .handle(Request::SelectLayout(SelectLayoutRequest {
                target: SelectLayoutTarget::Window(WindowTarget::with_window(alpha.clone(), 0)),
                layout: LayoutName::Tiled,
            }))
            .await,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::Tiled,
        })
    );

    assert_eq!(geometries(&handler, &alpha, 0).await, oracle_one_by_four());
    assert_eq!(
        geometries(&handler, &alpha, 1).await,
        other_geometry,
        "the window that was not addressed keeps its geometry"
    );
    assert_eq!(pane_ids(&handler, &alpha, 1).await, other_identities);
}
