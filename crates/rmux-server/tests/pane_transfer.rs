#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

mod common;

use common::{session_name, start_server, ClientConnection, TestHarness, PTY_TEST_LOCK};
use rmux_proto::{
    BreakPaneRequest, JoinPaneRequest, KillPaneRequest, LastPaneRequest, ListPanesRequest,
    ListSessionsRequest, NewSessionExtRequest, NewSessionRequest, NewWindowRequest, PaneTarget,
    Request, Response, SelectPaneRequest, SendKeysRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, SwapPaneRequest, TerminalSize, WindowTarget,
};

const FILE_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::test]
async fn break_pane_last_source_window_to_other_session_removes_source_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("break-pane-last-source-to-other-session");
    let socket_path = harness.socket_path().to_path_buf();
    let _handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let source = session_name("src");
    let hidden = session_name("hidden");

    for session in [&source, &hidden] {
        assert!(matches!(
            client
                .send_request(&Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize {
                        cols: 120,
                        rows: 40
                    }),
                    environment: None,
                }))
                .await?,
            Response::NewSession(_)
        ));
    }

    assert_eq!(
        client
            .send_request(&Request::BreakPane(Box::new(BreakPaneRequest {
                source: PaneTarget::new(source.clone(), 0),
                target: Some(WindowTarget::with_window(hidden.clone(), 1)),
                name: None,
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            })))
            .await?,
        Response::BreakPane(rmux_proto::BreakPaneResponse {
            target: PaneTarget::with_window(hidden.clone(), 1, 0),
            output: None,
        })
    );

    let sessions = client
        .send_request(&Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await?;
    let Response::ListSessions(sessions) = sessions else {
        panic!("expected list-sessions response");
    };
    let sessions = String::from_utf8(sessions.output.stdout)?;
    assert!(!sessions.lines().any(|line| line == source.as_str()));
    assert!(sessions.lines().any(|line| line == hidden.as_str()));

    let panes = client
        .send_request(&Request::ListPanes(ListPanesRequest {
            target: hidden.clone(),
            target_window_index: None,
            format: Some("#{window_index}.#{pane_index}:#{pane_id}".to_owned()),
        }))
        .await?;
    let Response::ListPanes(panes) = panes else {
        panic!("expected list-panes response");
    };
    let panes = String::from_utf8(panes.output.stdout)?;
    assert!(panes.lines().any(|line| line.starts_with("0.0:%")));
    assert!(panes.lines().any(|line| line.starts_with("1.0:%")));

    Ok(())
}

#[tokio::test]
async fn break_pane_last_grouped_source_removes_entire_source_group() -> Result<(), Box<dyn Error>>
{
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("break-pane-last-grouped-source-removes-group");
    let socket_path = harness.socket_path().to_path_buf();
    let _handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let source = session_name("src");
    let grouped = session_name("src-peer");
    let hidden = session_name("hidden");

    for session in [&source, &hidden] {
        assert!(matches!(
            client
                .send_request(&Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize {
                        cols: 120,
                        rows: 40
                    }),
                    environment: None,
                }))
                .await?,
            Response::NewSession(_)
        ));
    }
    assert!(matches!(
        client
            .send_request(&Request::NewSessionExt(Box::new(NewSessionExtRequest {
                session_name: Some(grouped.clone()),
                working_directory: None,
                detached: true,
                size: Some(TerminalSize {
                    cols: 120,
                    rows: 40
                }),
                environment: None,
                group_target: Some(source.clone()),
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            })))
            .await?,
        Response::NewSession(_)
    ));

    assert!(matches!(
        client
            .send_request(&Request::BreakPane(Box::new(BreakPaneRequest {
                source: PaneTarget::new(source.clone(), 0),
                target: Some(WindowTarget::with_window(hidden.clone(), 1)),
                name: None,
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            })))
            .await?,
        Response::BreakPane(_)
    ));

    let Response::ListSessions(sessions) = client
        .send_request(&Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await?
    else {
        panic!("expected list-sessions response");
    };
    let sessions = String::from_utf8(sessions.output.stdout)?;
    assert!(!sessions.lines().any(|line| line == source.as_str()));
    assert!(!sessions.lines().any(|line| line == grouped.as_str()));
    assert!(sessions.lines().any(|line| line == hidden.as_str()));

    Ok(())
}

#[tokio::test]
async fn join_pane_last_source_window_to_other_session_removes_source_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("join-pane-last-source-to-other-session");
    let socket_path = harness.socket_path().to_path_buf();
    let _handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let source = session_name("src");
    let hidden = session_name("hidden");

    for session in [&source, &hidden] {
        assert!(matches!(
            client
                .send_request(&Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize {
                        cols: 120,
                        rows: 40
                    }),
                    environment: None,
                }))
                .await?,
            Response::NewSession(_)
        ));
    }

    assert_eq!(
        client
            .send_request(&Request::JoinPane(JoinPaneRequest {
                source: PaneTarget::new(source.clone(), 0),
                target: PaneTarget::new(hidden.clone(), 0),
                direction: SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            }))
            .await?,
        Response::JoinPane(rmux_proto::JoinPaneResponse {
            target: PaneTarget::with_window(hidden.clone(), 0, 1),
        })
    );

    let sessions = client
        .send_request(&Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await?;
    let Response::ListSessions(sessions) = sessions else {
        panic!("expected list-sessions response");
    };
    let sessions = String::from_utf8(sessions.output.stdout)?;
    assert!(!sessions.lines().any(|line| line == source.as_str()));
    assert!(sessions.lines().any(|line| line == hidden.as_str()));

    let panes = client
        .send_request(&Request::ListPanes(ListPanesRequest {
            target: hidden.clone(),
            target_window_index: None,
            format: Some("#{window_index}.#{pane_index}:#{pane_id}".to_owned()),
        }))
        .await?;
    let Response::ListPanes(panes) = panes else {
        panic!("expected list-panes response");
    };
    let panes = String::from_utf8(panes.output.stdout)?;
    assert!(panes.lines().any(|line| line.starts_with("0.0:%")));
    assert!(panes.lines().any(|line| line.starts_with("0.1:%")));

    Ok(())
}

#[tokio::test]
async fn join_pane_last_grouped_source_removes_entire_source_group() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("join-pane-last-grouped-source-removes-group");
    let socket_path = harness.socket_path().to_path_buf();
    let _handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let source = session_name("src");
    let grouped = session_name("src-peer");
    let hidden = session_name("hidden");

    for session in [&source, &hidden] {
        assert!(matches!(
            client
                .send_request(&Request::NewSession(NewSessionRequest {
                    session_name: session.clone(),
                    detached: true,
                    size: Some(TerminalSize {
                        cols: 120,
                        rows: 40
                    }),
                    environment: None,
                }))
                .await?,
            Response::NewSession(_)
        ));
    }
    assert!(matches!(
        client
            .send_request(&Request::NewSessionExt(Box::new(NewSessionExtRequest {
                session_name: Some(grouped.clone()),
                working_directory: None,
                detached: true,
                size: Some(TerminalSize {
                    cols: 120,
                    rows: 40
                }),
                environment: None,
                group_target: Some(source.clone()),
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            })))
            .await?,
        Response::NewSession(_)
    ));

    assert!(matches!(
        client
            .send_request(&Request::JoinPane(JoinPaneRequest {
                source: PaneTarget::new(source.clone(), 0),
                target: PaneTarget::new(hidden.clone(), 0),
                direction: SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            }))
            .await?,
        Response::JoinPane(_)
    ));

    let Response::ListSessions(sessions) = client
        .send_request(&Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
        .await?
    else {
        panic!("expected list-sessions response");
    };
    let sessions = String::from_utf8(sessions.output.stdout)?;
    assert!(!sessions.lines().any(|line| line == source.as_str()));
    assert!(!sessions.lines().any(|line| line == grouped.as_str()));
    assert!(sessions.lines().any(|line| line == hidden.as_str()));

    Ok(())
}

#[tokio::test]
async fn pane_transfer_commands_move_live_ptys_between_windows() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("pane-transfer-live-ptys");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let session = session_name("alpha");
    let root = socket_path
        .parent()
        .expect("socket path must have a parent");
    let join_path = root.join("join.txt");
    let break_path = root.join("break.txt");
    let swap_source_path = root.join("swap-source.txt");
    let swap_target_path = root.join("swap-target.txt");

    assert!(matches!(
        client
            .send_request(&Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize {
                    cols: 120,
                    rows: 40,
                }),
                environment: None,
            }))
            .await?,
        Response::NewSession(_)
    ));

    assert_eq!(
        client
            .send_request(&Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(session.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await?,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(session.clone(), 1),
        })
    );
    assert_eq!(
        client
            .send_request(&Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::new(session.clone(), 1),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await?,
        Response::SelectPane(rmux_proto::SelectPaneResponse {
            target: PaneTarget::new(session.clone(), 1),
        })
    );
    assert_eq!(
        client
            .send_request(&Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::new(session.clone(), 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await?,
        Response::SelectPane(rmux_proto::SelectPaneResponse {
            target: PaneTarget::new(session.clone(), 0),
        })
    );
    assert_eq!(
        client
            .send_request(&Request::LastPane(LastPaneRequest {
                target: WindowTarget::new(session.clone()),
                preserve_zoom: false,
                input_disabled: None,
            }))
            .await?,
        Response::LastPane(rmux_proto::LastPaneResponse {
            target: PaneTarget::new(session.clone(), 1),
        })
    );

    assert!(matches!(
        client
            .send_request(&Request::SendKeys(SendKeysRequest {
                target: PaneTarget::new(session.clone(), 1),
                keys: vec![
                    "export RMUX_TRANSFER_MARK=joined".to_owned(),
                    "Enter".to_owned()
                ],
            }))
            .await?,
        Response::SendKeys(_)
    ));

    assert_eq!(
        client
            .send_request(&Request::NewWindow(Box::new(NewWindowRequest {
                target: session.clone(),
                name: Some("dest".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await?,
        Response::NewWindow(rmux_proto::NewWindowResponse {
            target: WindowTarget::with_window(session.clone(), 1),
        })
    );
    assert_eq!(
        client
            .send_request(&Request::JoinPane(JoinPaneRequest {
                source: PaneTarget::new(session.clone(), 1),
                target: PaneTarget::with_window(session.clone(), 1, 0),
                direction: SplitDirection::Vertical,
                detached: true,
                before: false,
                full_size: false,
                size: None,
            }))
            .await?,
        Response::JoinPane(rmux_proto::JoinPaneResponse {
            target: PaneTarget::with_window(session.clone(), 1, 1),
        })
    );
    assert!(matches!(
        client
            .send_request(&Request::SendKeys(SendKeysRequest {
                target: PaneTarget::with_window(session.clone(), 1, 1),
                keys: vec![
                    format!("printf \"$RMUX_TRANSFER_MARK\" > {}", join_path.display()),
                    "Enter".to_owned(),
                ],
            }))
            .await?,
        Response::SendKeys(_)
    ));
    wait_for_file_contents(&join_path, "joined").await?;

    assert_eq!(
        client
            .send_request(&Request::BreakPane(Box::new(BreakPaneRequest {
                source: PaneTarget::with_window(session.clone(), 1, 1),
                target: Some(WindowTarget::with_window(session.clone(), 2)),
                name: Some("broken".to_owned()),
                detached: true,
                after: false,
                before: false,
                print_target: false,
                format: None,
            })))
            .await?,
        Response::BreakPane(rmux_proto::BreakPaneResponse {
            target: PaneTarget::with_window(session.clone(), 2, 0),
            output: None,
        })
    );
    assert!(matches!(
        client
            .send_request(&Request::SendKeys(SendKeysRequest {
                target: PaneTarget::with_window(session.clone(), 2, 0),
                keys: vec![
                    format!("printf \"$RMUX_TRANSFER_MARK\" > {}", break_path.display()),
                    "Enter".to_owned(),
                ],
            }))
            .await?,
        Response::SendKeys(_)
    ));
    wait_for_file_contents(&break_path, "joined").await?;

    assert!(matches!(
        client
            .send_request(&Request::NewWindow(Box::new(NewWindowRequest {
                target: session.clone(),
                name: Some("swap".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await?,
        Response::NewWindow(_)
    ));
    assert!(matches!(
        client
            .send_request(&Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 3, 0)),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await?,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        client
            .send_request(&Request::KillPane(KillPaneRequest {
                target: PaneTarget::with_window(session.clone(), 3, 0),
                kill_all_except: false,
            }))
            .await?,
        Response::KillPane(_)
    ));
    assert!(matches!(
        client
            .send_request(&Request::SendKeys(SendKeysRequest {
                target: PaneTarget::with_window(session.clone(), 3, 0),
                keys: vec![
                    "export RMUX_TRANSFER_MARK=swapped".to_owned(),
                    "Enter".to_owned()
                ],
            }))
            .await?,
        Response::SendKeys(_)
    ));

    assert_eq!(
        client
            .send_request(&Request::SwapPane(SwapPaneRequest {
                source: PaneTarget::with_window(session.clone(), 2, 0),
                target: PaneTarget::with_window(session.clone(), 3, 0),
                direction: None,
                detached: true,
                preserve_zoom: false,
            }))
            .await?,
        Response::SwapPane(rmux_proto::SwapPaneResponse {
            source: PaneTarget::with_window(session.clone(), 2, 0),
            target: PaneTarget::with_window(session.clone(), 3, 0),
        })
    );
    assert!(matches!(
        client
            .send_request(&Request::SendKeys(SendKeysRequest {
                target: PaneTarget::with_window(session.clone(), 2, 0),
                keys: vec![
                    format!(
                        "printf \"$RMUX_TRANSFER_MARK\" > {}",
                        swap_source_path.display()
                    ),
                    "Enter".to_owned(),
                ],
            }))
            .await?,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        client
            .send_request(&Request::SendKeys(SendKeysRequest {
                target: PaneTarget::with_window(session.clone(), 3, 0),
                keys: vec![
                    format!(
                        "printf \"$RMUX_TRANSFER_MARK\" > {}",
                        swap_target_path.display()
                    ),
                    "Enter".to_owned(),
                ],
            }))
            .await?,
        Response::SendKeys(_)
    ));
    wait_for_file_contents(&swap_source_path, "swapped").await?;
    wait_for_file_contents(&swap_target_path, "joined").await?;

    handle.shutdown().await?;
    Ok(())
}

async fn wait_for_file_contents(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + FILE_TIMEOUT;

    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) | Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }

    Err(io::Error::other(format!(
        "timed out waiting for '{}' to contain '{}'",
        path.display(),
        expected
    ))
    .into())
}
