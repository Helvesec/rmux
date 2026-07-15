#![cfg(unix)]

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod common;

use common::{
    session_name, start_server, tty_size, wait_for_socket_removal, ClientConnection, TestHarness,
    PTY_TEST_LOCK,
};
use rmux_proto::{
    HasSessionRequest, KillPaneRequest, LayoutName, ListPanesRequest, NewSessionExtRequest,
    NewSessionRequest, NewWindowRequest, PaneTarget, ProcessCommand, Request, ResizePaneAdjustment,
    Response, SelectLayoutRequest, SelectLayoutTarget, SelectPaneRequest, SelectWindowRequest,
    SessionName, SplitWindowRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};

#[tokio::test]
async fn pane_management_requests_round_trip_through_the_socket() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("pane-management");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let session = session_name("alpha");

    let missing_split = client
        .send_request(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name("missing")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await?;
    assert_eq!(
        missing_split,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::SessionNotFound("missing".to_owned()),
        })
    );

    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 200,
                rows: 50,
            }),
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let select_layout_window = client
        .send_request(&Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(session.clone())),
            layout: LayoutName::MainVertical,
        }))
        .await?;
    assert_eq!(
        select_layout_window,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );

    let missing_resize = client
        .send_request(&Request::ResizePane(rmux_proto::ResizePaneRequest {
            target: PaneTarget::new(session.clone(), 9),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }))
        .await?;
    assert_eq!(
        missing_resize,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::invalid_target(
                "alpha:0.9",
                "pane index does not exist in session",
            ),
        })
    );

    let first_split = client
        .send_request(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await?;
    assert_eq!(
        first_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session.clone(), 1),
        })
    );

    let second_split = client
        .send_request(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(session.clone(), 0)),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await?;
    assert_eq!(
        second_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session.clone(), 1),
        })
    );

    let resized = client
        .send_request(&Request::ResizePane(rmux_proto::ResizePaneRequest {
            target: PaneTarget::new(session.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }))
        .await?;
    assert_eq!(
        resized,
        Response::ResizePane(rmux_proto::ResizePaneResponse {
            target: PaneTarget::new(session.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        })
    );

    let select_layout_session = client
        .send_request(&Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Session(session.clone()),
            layout: LayoutName::MainVertical,
        }))
        .await?;
    assert_eq!(
        select_layout_session,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );

    let selected = client
        .send_request(&Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(session, 2),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await?;
    assert_eq!(
        selected,
        Response::SelectPane(rmux_proto::SelectPaneResponse {
            target: PaneTarget::new(session_name("alpha"), 2),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn horizontal_split_and_kill_pane_round_trip_through_the_socket() -> Result<(), Box<dyn Error>>
{
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("horizontal-split-and-kill-pane");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let session = session_name("beta");

    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let horizontal_split = client
        .send_request(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await?;
    assert_eq!(
        horizontal_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session.clone(), 1),
        })
    );

    let killed = client
        .send_request(&Request::KillPane(rmux_proto::KillPaneRequest {
            target: PaneTarget::new(session.clone(), 1),
            kill_all_except: false,
        }))
        .await?;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::new(session.clone(), 1),
            window_destroyed: false,
        })
    );

    let retried_split = client
        .send_request(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await?;
    assert_eq!(
        retried_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session, 1),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn select_layout_even_layouts_resize_panes_through_the_socket() -> Result<(), Box<dyn Error>>
{
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("select-layout-even-layouts");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let session = session_name("even");

    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 100,
                rows: 40,
            }),
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    for expected_pane in [1, 1] {
        let split = client
            .send_request(&Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::new(session.clone(), 0)),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await?;
        assert_eq!(
            split,
            Response::SplitWindow(rmux_proto::SplitWindowResponse {
                pane: PaneTarget::new(session.clone(), expected_pane),
            })
        );
    }

    let pane_ttys = wait_for_session_pane_ttys(&mut client, &session, 3).await?;
    assert_eq!(pane_ttys.len(), 3);

    let even_horizontal = client
        .send_request(&Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(session.clone())),
            layout: LayoutName::EvenHorizontal,
        }))
        .await?;
    assert_eq!(
        even_horizontal,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenHorizontal,
        })
    );

    assert_eq!(
        wait_for_tty_sizes(
            &pane_ttys,
            &[
                TerminalSize { cols: 32, rows: 40 },
                TerminalSize { cols: 32, rows: 40 },
                TerminalSize { cols: 34, rows: 40 },
            ],
        )?,
        vec![
            TerminalSize { cols: 32, rows: 40 },
            TerminalSize { cols: 32, rows: 40 },
            TerminalSize { cols: 34, rows: 40 },
        ]
    );

    let even_vertical = client
        .send_request(&Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(WindowTarget::new(session.clone())),
            layout: LayoutName::EvenVertical,
        }))
        .await?;
    assert_eq!(
        even_vertical,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::EvenVertical,
        })
    );

    assert_eq!(
        wait_for_tty_sizes(
            &pane_ttys,
            &[
                TerminalSize {
                    cols: 100,
                    rows: 12,
                },
                TerminalSize {
                    cols: 100,
                    rows: 12,
                },
                TerminalSize {
                    cols: 100,
                    rows: 14,
                },
            ],
        )?,
        vec![
            TerminalSize {
                cols: 100,
                rows: 12
            },
            TerminalSize {
                cols: 100,
                rows: 12
            },
            TerminalSize {
                cols: 100,
                rows: 14
            },
        ]
    );

    handle.shutdown().await?;
    Ok(())
}

async fn wait_for_session_pane_ttys(
    client: &mut ClientConnection,
    session: &SessionName,
    expected_count: usize,
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);

    while Instant::now() < deadline {
        let listed = client
            .send_request(&Request::ListPanes(Box::new(ListPanesRequest {
                target: session.clone(),
                format: Some("#{pane_index}:#{pane_tty}".to_owned()),
                filter: None,
                sort_order: None,
                reversed: false,
                target_window_index: None,
            })))
            .await?;
        let output = listed
            .command_output()
            .ok_or("list-panes response did not include command output")?;
        let mut pane_ttys = std::str::from_utf8(output.stdout())?
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(index, tty)| {
                index
                    .parse::<u32>()
                    .map(|index| (index, PathBuf::from(tty)))
                    .map_err(|error| format!("invalid pane index '{index}': {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        pane_ttys.sort_by_key(|(index, _)| *index);
        if pane_ttys.len() == expected_count
            && pane_ttys.iter().all(|(_, tty)| !tty.as_os_str().is_empty())
        {
            return Ok(pane_ttys.into_iter().map(|(_, tty)| tty).collect());
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    Err(
        format!("timed out waiting for {expected_count} pane tty paths in session {session}")
            .into(),
    )
}

fn wait_for_tty_sizes(
    paths: &[PathBuf],
    expected: &[TerminalSize],
) -> Result<Vec<TerminalSize>, Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut expected_sizes = expected.to_vec();
    expected_sizes.sort_by_key(|size| (size.cols, size.rows));

    while Instant::now() < deadline {
        let mut current_sizes = paths
            .iter()
            .map(|path| tty_size(path))
            .collect::<Result<Vec<_>, _>>()?;
        current_sizes.sort_by_key(|size| (size.cols, size.rows));
        if current_sizes == expected_sizes {
            return Ok(current_sizes);
        }

        std::thread::sleep(Duration::from_millis(25));
    }

    let mut current_sizes = paths
        .iter()
        .map(|path| tty_size(path))
        .collect::<Result<Vec<_>, _>>()?;
    current_sizes.sort_by_key(|size| (size.cols, size.rows));
    Err(format!(
        "timed out waiting for tty sizes {:?}, found {:?}",
        expected_sizes, current_sizes
    )
    .into())
}

#[tokio::test]
async fn killing_the_last_pane_destroys_the_window_and_session_targets_fall_back(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("kill-pane-destroys-window");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let session = session_name("gamma");

    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let created_window = client
        .send_request(&Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: Some("scratch".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await?;
    assert_eq!(
        created_window,
        Response::NewWindow(rmux_proto::NewWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let selected_window = client
        .send_request(&Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(session.clone(), 1),
        }))
        .await?;
    assert_eq!(
        selected_window,
        Response::SelectWindow(rmux_proto::SelectWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );

    let killed = client
        .send_request(&Request::KillPane(rmux_proto::KillPaneRequest {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            kill_all_except: false,
        }))
        .await?;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::with_window(session.clone(), 1, 0),
            window_destroyed: true,
        })
    );

    let split = client
        .send_request(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await?;
    assert_eq!(
        split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::with_window(session, 0, 1),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn killing_the_last_pane_in_the_only_window_removes_the_session_over_the_socket(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("kill-last-pane-removes-session");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let session = session_name("alpha");

    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let killed = client
        .send_request(&Request::KillPane(KillPaneRequest {
            target: PaneTarget::with_window(session.clone(), 0, 0),
            kill_all_except: false,
        }))
        .await?;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::with_window(session.clone(), 0, 0),
            window_destroyed: true,
        })
    );

    drop(client);
    wait_for_socket_removal(&socket_path).await?;
    drop(handle);
    Ok(())
}

#[tokio::test]
async fn pty_eof_before_child_exit_eventually_removes_the_session() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("pty-eof-before-child-exit");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;

    let keeper = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session_name("keeper"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await?;
    assert!(matches!(keeper, Response::NewSession(_)));

    let delayed = session_name("delayed-exit");
    let created = client
        .send_request(&Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(delayed.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: Some(ProcessCommand::Argv(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "printf x; exec sleep 1 </dev/null >/dev/null 2>&1".to_owned(),
            ])),
            client_environment: None,
            skip_environment_update: false,
        })))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let response = client
            .send_request(&Request::HasSession(HasSessionRequest {
                target: delayed.clone(),
            }))
            .await?;
        if response == Response::HasSession(rmux_proto::HasSessionResponse { exists: false }) {
            break;
        }
        if Instant::now() >= deadline {
            return Err("session survived after its PTY closed and child exited".into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    handle.shutdown().await?;
    Ok(())
}
