#![cfg(unix)]

mod common;

use std::error::Error;

use common::{session_name, start_server, TestHarness};
use rmux_client::connect;
use rmux_proto::{
    BreakPaneRequest, JoinPaneRequest, NewSessionRequest, NewWindowResponse, PaneTarget, Request,
    Response, SelectPaneResponse, SplitDirection, SplitWindowTarget, TerminalSize, WindowTarget,
};

#[test]
fn pane_transfer_commands_round_trip_through_connection_helpers() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("pane-transfer-helpers");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    assert!(matches!(
        connection.roundtrip(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
        }))?,
        Response::NewSession(_)
    ));
    assert_eq!(
        connection.split_window(SplitWindowTarget::Session(session.clone()))?,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.select_pane(PaneTarget::new(session.clone(), 1))?,
        Response::SelectPane(SelectPaneResponse {
            target: PaneTarget::new(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.select_pane(PaneTarget::new(session.clone(), 0))?,
        Response::SelectPane(SelectPaneResponse {
            target: PaneTarget::new(session.clone(), 0),
        })
    );
    assert_eq!(
        connection.last_pane(WindowTarget::new(session.clone()))?,
        Response::LastPane(rmux_proto::LastPaneResponse {
            target: PaneTarget::new(session.clone(), 1),
        })
    );

    assert_eq!(
        connection.new_window(session.clone(), Some("dest".to_owned()), true)?,
        Response::NewWindow(NewWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.join_pane(JoinPaneRequest {
            source: PaneTarget::new(session.clone(), 1),
            target: PaneTarget::with_window(session.clone(), 1, 0),
            direction: SplitDirection::Vertical,
            detached: true,
            before: false,
            full_size: false,
            size: None,
        })?,
        Response::JoinPane(rmux_proto::JoinPaneResponse {
            target: PaneTarget::with_window(session.clone(), 1, 1),
        })
    );
    assert_eq!(
        connection.break_pane(BreakPaneRequest {
            source: PaneTarget::with_window(session.clone(), 1, 1),
            target: Some(WindowTarget::with_window(session.clone(), 2)),
            name: Some("broken".to_owned()),
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })?,
        Response::BreakPane(rmux_proto::BreakPaneResponse {
            target: PaneTarget::with_window(session.clone(), 2, 0),
            output: None,
        })
    );

    assert!(matches!(
        connection.new_window(session.clone(), Some("swap".to_owned()), true)?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        connection.split_window(SplitWindowTarget::Pane(PaneTarget::with_window(
            session.clone(),
            3,
            0,
        )))?,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        connection.kill_pane(PaneTarget::with_window(session.clone(), 3, 0))?,
        Response::KillPane(_)
    ));
    assert_eq!(
        connection.swap_pane(
            PaneTarget::with_window(session.clone(), 2, 0),
            PaneTarget::with_window(session.clone(), 3, 0),
            true,
            false,
        )?,
        Response::SwapPane(rmux_proto::SwapPaneResponse {
            source: PaneTarget::with_window(session.clone(), 2, 0),
            target: PaneTarget::with_window(session.clone(), 3, 0),
        })
    );

    assert!(matches!(
        connection.new_window(session.clone(), Some("relative".to_owned()), true)?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        connection.split_window(SplitWindowTarget::Pane(PaneTarget::with_window(
            session.clone(),
            4,
            0,
        )))?,
        Response::SplitWindow(_)
    ));
    assert_eq!(
        connection.swap_pane_with_previous(
            PaneTarget::with_window(session.clone(), 4, 1),
            true,
            false,
        )?,
        Response::SwapPane(rmux_proto::SwapPaneResponse {
            source: PaneTarget::with_window(session.clone(), 4, 0),
            target: PaneTarget::with_window(session.clone(), 4, 1),
        })
    );

    server.shutdown()?;
    Ok(())
}
