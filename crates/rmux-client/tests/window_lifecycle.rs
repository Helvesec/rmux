#![cfg(unix)]

mod common;

use std::error::Error;

use common::{session_name, start_server, TestHarness};
use rmux_client::connect;
use rmux_proto::{
    KillPaneResponse, KillWindowResponse, MoveWindowResponse, MoveWindowTarget, NewSessionRequest,
    NewWindowRequest, NewWindowResponse, PaneTarget, RenameWindowResponse, Request, Response,
    RotateWindowDirection, RotateWindowResponse, SelectWindowResponse, SplitDirection,
    SplitWindowResponse, SplitWindowTarget, SwapWindowResponse, TerminalSize, WindowTarget,
};

#[test]
fn window_commands_round_trip_through_connection_helpers() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("window-helpers");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    let created_window = connection.new_window(session.clone(), Some("logs".to_owned()), true)?;
    assert_eq!(
        created_window,
        Response::NewWindow(NewWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let selected = connection.select_window(WindowTarget::with_window(session.clone(), 1))?;
    assert_eq!(
        selected,
        Response::SelectWindow(SelectWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let renamed = connection.rename_window(
        WindowTarget::with_window(session.clone(), 1),
        "renamed".to_owned(),
    )?;
    assert_eq!(
        renamed,
        Response::RenameWindow(RenameWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let killed = connection.kill_window(WindowTarget::with_window(session.clone(), 1), false)?;
    assert_eq!(
        killed,
        Response::KillWindow(KillWindowResponse {
            target: WindowTarget::with_window(session.clone(), 0),
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn kill_window_all_others_preserves_the_target_window_for_session_commands(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-window-all-others");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: Some(TerminalSize {
            cols: 120,
            rows: 40,
        }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    assert!(matches!(
        connection.new_window(session.clone(), None, true)?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        connection.select_window(WindowTarget::with_window(session.clone(), 1))?,
        Response::SelectWindow(_)
    ));
    assert_eq!(
        connection.split_window(SplitWindowTarget::Session(session.clone()))?,
        Response::SplitWindow(SplitWindowResponse {
            pane: PaneTarget::with_window(session.clone(), 1, 1),
        })
    );
    assert!(matches!(
        connection.new_window(session.clone(), None, true)?,
        Response::NewWindow(_)
    ));

    let killed = connection.kill_window(WindowTarget::with_window(session.clone(), 1), true)?;
    assert_eq!(
        killed,
        Response::KillWindow(KillWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let split = connection.split_window(SplitWindowTarget::Session(session.clone()))?;
    assert_eq!(
        split,
        Response::SplitWindow(SplitWindowResponse {
            pane: PaneTarget::with_window(session, 1, 2),
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn horizontal_split_and_kill_pane_round_trip_through_connection_helpers(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("horizontal-split-and-kill-pane");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: Some(TerminalSize {
            cols: 120,
            rows: 40,
        }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    assert_eq!(
        connection.split_window_with_direction(
            SplitWindowTarget::Session(session.clone()),
            SplitDirection::Horizontal,
        )?,
        Response::SplitWindow(SplitWindowResponse {
            pane: PaneTarget::new(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.kill_pane(PaneTarget::new(session.clone(), 1))?,
        Response::KillPane(KillPaneResponse {
            target: PaneTarget::new(session.clone(), 1),
            window_destroyed: false,
        })
    );
    assert_eq!(
        connection.split_window(SplitWindowTarget::Session(session.clone()))?,
        Response::SplitWindow(SplitWindowResponse {
            pane: PaneTarget::new(session, 1),
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn killing_the_last_pane_destroys_the_window_and_session_targets_fall_back(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-pane-destroys-window");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: Some(TerminalSize {
            cols: 120,
            rows: 40,
        }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    assert_eq!(
        connection.roundtrip(&Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: Some("scratch".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))?,
        Response::NewWindow(NewWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.select_window(WindowTarget::with_window(session.clone(), 1))?,
        Response::SelectWindow(SelectWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.kill_pane(PaneTarget::with_window(session.clone(), 1, 0))?,
        Response::KillPane(KillPaneResponse {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            window_destroyed: true,
        })
    );
    assert_eq!(
        connection.split_window(SplitWindowTarget::Session(session.clone()))?,
        Response::SplitWindow(SplitWindowResponse {
            pane: PaneTarget::with_window(session, 0, 1),
        })
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn window_navigation_and_listing_round_trip_through_connection_helpers(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("window-navigation-helpers");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    assert!(matches!(
        connection.new_window(session.clone(), Some("logs".to_owned()), true)?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        connection.new_window(session.clone(), Some("shell".to_owned()), true)?,
        Response::NewWindow(_)
    ));

    assert_eq!(
        connection.next_window(session.clone(), false)?,
        Response::NextWindow(rmux_proto::NextWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );
    assert_eq!(
        connection.previous_window(session.clone(), false)?,
        Response::PreviousWindow(rmux_proto::PreviousWindowResponse {
            target: WindowTarget::with_window(session.clone(), 0),
        })
    );
    assert_eq!(
        connection.last_window(session.clone())?,
        Response::LastWindow(rmux_proto::LastWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let listed = connection.list_windows(
        session.clone(),
        Some("#{window_index}:#{window_id}:#{window_active}".to_owned()),
    )?;
    let Response::ListWindows(listed) = listed else {
        panic!("expected list-windows response");
    };
    assert_eq!(
        std::str::from_utf8(listed.output.stdout())?,
        "0:@0:0\n1:@1:1\n2:@2:0\n"
    );
    assert_eq!(listed.windows.len(), 3);
    assert_eq!(listed.windows[1].name.as_deref(), Some("logs"));
    assert_eq!(
        listed.windows[1].target,
        WindowTarget::with_window(session, 1)
    );

    server.shutdown()?;
    Ok(())
}

#[test]
fn move_swap_and_rotate_window_commands_round_trip_through_connection_helpers(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("window-movement-helpers");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session in [alpha.clone(), beta.clone()] {
        let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
            session_name: session,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))?;
        assert!(matches!(created, Response::NewSession(_)));
    }

    assert!(matches!(
        connection.new_window(alpha.clone(), Some("logs".to_owned()), true)?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        connection.new_window(alpha.clone(), Some("scratch".to_owned()), true)?,
        Response::NewWindow(_)
    ));
    assert_eq!(
        connection.move_window(
            Some(WindowTarget::with_window(alpha.clone(), 1)),
            MoveWindowTarget::Window(WindowTarget::with_window(beta.clone(), 4)),
            false,
            false,
            true,
        )?,
        Response::MoveWindow(MoveWindowResponse {
            session_name: beta.clone(),
            target: Some(WindowTarget::with_window(beta.clone(), 4)),
        })
    );
    assert_eq!(
        connection.swap_window(
            WindowTarget::with_window(alpha.clone(), 2),
            WindowTarget::with_window(beta.clone(), 4),
            true,
        )?,
        Response::SwapWindow(SwapWindowResponse {
            source: WindowTarget::with_window(alpha.clone(), 2),
            target: WindowTarget::with_window(beta.clone(), 4),
        })
    );
    assert_eq!(
        connection.split_window(SplitWindowTarget::Pane(PaneTarget::with_window(
            alpha.clone(),
            2,
            0,
        )))?,
        Response::SplitWindow(SplitWindowResponse {
            pane: PaneTarget::with_window(alpha.clone(), 2, 1),
        })
    );
    assert_eq!(
        connection.rotate_window(
            WindowTarget::with_window(alpha.clone(), 2),
            RotateWindowDirection::Up,
        )?,
        Response::RotateWindow(RotateWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 2),
        })
    );

    let alpha_windows = connection.list_windows(
        alpha.clone(),
        Some("#{window_index}:#{window_panes}".to_owned()),
    )?;
    let Response::ListWindows(alpha_windows) = alpha_windows else {
        panic!("expected list-windows response");
    };
    assert_eq!(
        std::str::from_utf8(alpha_windows.output.stdout())?,
        "0:1\n2:2\n"
    );

    let beta_windows = connection.list_windows(
        beta.clone(),
        Some("#{window_index}:#{window_panes}".to_owned()),
    )?;
    let Response::ListWindows(beta_windows) = beta_windows else {
        panic!("expected list-windows response");
    };
    assert_eq!(
        std::str::from_utf8(beta_windows.output.stdout())?,
        "0:1\n4:1\n"
    );

    drop(connection);
    server.shutdown()?;
    Ok(())
}

#[test]
fn move_window_with_position_preserves_after_before_flags() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("window-move-position");
    let mut server = start_server(&harness)?;
    let mut connection = connect(harness.socket_path())?;
    let session = session_name("alpha");

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session.clone(),
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));
    assert!(matches!(
        connection.new_window(session.clone(), Some("one".to_owned()), true)?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        connection.new_window(session.clone(), Some("two".to_owned()), true)?,
        Response::NewWindow(_)
    ));

    assert_eq!(
        connection.move_window_with_position(
            Some(WindowTarget::with_window(session.clone(), 2)),
            MoveWindowTarget::Window(WindowTarget::with_window(session.clone(), 0)),
            false,
            false,
            true,
            true,
            false,
        )?,
        Response::MoveWindow(MoveWindowResponse {
            session_name: session.clone(),
            target: Some(WindowTarget::with_window(session.clone(), 1)),
        })
    );

    let listed = connection.list_windows(
        session.clone(),
        Some("#{window_index}:#{window_name}".to_owned()),
    )?;
    let Response::ListWindows(listed) = listed else {
        panic!("expected list-windows response");
    };
    let lines = std::str::from_utf8(listed.output.stdout())?
        .lines()
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1], "1:two");
    assert_eq!(lines[2], "2:one");

    drop(connection);
    server.shutdown()?;
    Ok(())
}
