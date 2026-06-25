#![cfg(unix)]

mod common;
#[path = "stress/support.rs"]
mod support;

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::Duration;

use common::{
    assert_socket_directory_empty, assert_success, default_socket_path_in, pane_tty_paths, stderr,
    stdout, tty_has_input, unique_tmpdir, wait_for_no_child_processes, CliHarness,
    BINARY_OVERRIDE_ENV,
};
use rmux_client::{connect, ClientError};
use rmux_core::Session;
use rmux_proto::{
    CapturePaneRequest, KillSessionRequest, KillSessionResponse, LayoutName, NewSessionExtRequest,
    NewSessionRequest, PaneTarget, ProcessCommand, Request, ResizePaneAdjustment,
    ResizePaneRequest, Response, SelectLayoutRequest, SelectLayoutResponse, SelectLayoutTarget,
    SendKeysRequest, SendKeysResponse, SplitWindowExtRequest, SplitWindowRequest,
    SplitWindowResponse, SplitWindowTarget, Target, TerminalSize,
};
use rmux_server::{DaemonConfig, ServerDaemon, ServerHandle};
use support::{
    assert_valid_non_overlapping_geometry, runtime, serialize_test_execution, single_new_tty,
    tty_sizes_by_index, unique_session_name, wait_for_tty_size,
};

const TERMINAL_COLS: u16 = 200;
const TERMINAL_ROWS: u16 = 50;
const PANE_COUNT: usize = 20;
const SECONDARY_PANE_COUNT: u32 = (PANE_COUNT - 1) as u32;
const MAIN_PANE_WIDTH: u16 = 34;
const REAP_TIMEOUT: Duration = Duration::from_secs(2);
const DAEMON_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const STRESS_PANE_COMMAND: &str = "/bin/cat";

fn wait_for_socket_directory_empty(
    socket_path: &Path,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if assert_socket_directory_empty(socket_path).is_ok() {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            assert_socket_directory_empty(socket_path)?;
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}

fn shutdown_daemon(
    runtime: &tokio::runtime::Runtime,
    handle: ServerHandle,
) -> Result<(), Box<dyn Error>> {
    runtime.block_on(async {
        tokio::time::timeout(DAEMON_SHUTDOWN_TIMEOUT, handle.shutdown())
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "daemon shutdown timed out")
            })??;
        Ok::<(), Box<dyn Error>>(())
    })
}

#[test]
fn twenty_pane_layout_produces_valid_geometry_and_resizes_all_ptys() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();
    let runtime = runtime()?;
    let tmpdir = unique_tmpdir("stress-20pane");
    fs::create_dir_all(&tmpdir)?;
    let socket_path = default_socket_path_in(&tmpdir)?;
    let handle =
        runtime.block_on(ServerDaemon::new(DaemonConfig::new(socket_path.clone())).bind())?;
    let session_name = unique_session_name("stress-20pane");
    let mut connection = connect(&socket_path)?;
    let mut tty_paths = HashMap::new();
    let mut expected_session = Session::new(
        session_name.clone(),
        TerminalSize {
            cols: TERMINAL_COLS,
            rows: TERMINAL_ROWS,
        },
    );

    let created =
        connection.roundtrip(&Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: TERMINAL_COLS,
                rows: TERMINAL_ROWS,
            }),
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
            process_command: Some(ProcessCommand::Argv(vec![STRESS_PANE_COMMAND.to_owned()])),
            client_environment: None,
            skip_environment_update: false,
        })))?;
    assert!(matches!(created, Response::NewSession(_)));

    tty_paths.insert(
        0,
        pane_tty_path(&mut connection, PaneTarget::new(session_name.clone(), 0))?,
    );

    for expected_index in 1..PANE_COUNT as u32 {
        let split =
            connection.roundtrip(&Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                target: SplitWindowTarget::Session(session_name.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: Some(ProcessCommand::Argv(vec![STRESS_PANE_COMMAND.to_owned()])),
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            })))?;
        assert_eq!(
            split,
            Response::SplitWindow(SplitWindowResponse {
                pane: PaneTarget::new(session_name.clone(), expected_index),
            })
        );
        assert_eq!(
            expected_session
                .split_active_pane_with_direction(rmux_proto::SplitDirection::Vertical)?,
            expected_index
        );

        let relayout = connection.roundtrip(&Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Session(session_name.clone()),
            layout: LayoutName::EvenHorizontal,
        }))?;
        assert_eq!(
            relayout,
            Response::SelectLayout(SelectLayoutResponse {
                layout: LayoutName::EvenHorizontal,
            })
        );
        expected_session.select_layout(LayoutName::EvenHorizontal);

        tty_paths.insert(
            expected_index,
            pane_tty_path(
                &mut connection,
                PaneTarget::new(session_name.clone(), expected_index),
            )?,
        );
    }

    assert_eq!(tty_paths.len(), PANE_COUNT);

    let pty_sizes_before_layout = tty_sizes_by_index(&tty_paths)?;

    let selected = connection.roundtrip(&Request::SelectLayout(SelectLayoutRequest {
        target: SelectLayoutTarget::Session(session_name.clone()),
        layout: LayoutName::MainVertical,
    }))?;
    assert_eq!(
        selected,
        Response::SelectLayout(SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );

    let resized = connection.roundtrip(&Request::ResizePane(ResizePaneRequest {
        target: PaneTarget::new(session_name.clone(), 0),
        adjustment: ResizePaneAdjustment::AbsoluteWidth {
            columns: MAIN_PANE_WIDTH,
        },
    }))?;
    assert_eq!(
        resized,
        Response::ResizePane(rmux_proto::ResizePaneResponse {
            target: PaneTarget::new(session_name.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth {
                columns: MAIN_PANE_WIDTH
            },
        })
    );

    expected_session.select_layout(LayoutName::MainVertical);
    expected_session.resize_pane(
        0,
        ResizePaneAdjustment::AbsoluteWidth {
            columns: MAIN_PANE_WIDTH,
        },
    )?;

    let panes = expected_session.window().panes();
    assert_eq!(panes.len(), PANE_COUNT);
    assert_valid_non_overlapping_geometry(
        panes,
        TerminalSize {
            cols: TERMINAL_COLS,
            rows: TERMINAL_ROWS,
        },
    );

    let secondary_panes: Vec<_> = panes.iter().skip(1).collect();
    let separators = SECONDARY_PANE_COUNT.saturating_sub(1);
    let usable_rows = u32::from(TERMINAL_ROWS).saturating_sub(separators);
    let secondary_rows = secondary_panes
        .iter()
        .map(|pane| u32::from(pane.geometry().rows()))
        .collect::<Vec<_>>();
    assert!(secondary_rows.iter().all(|rows| *rows > 0));
    assert_eq!(secondary_rows.iter().sum::<u32>(), usable_rows);

    for pane in panes {
        let original_size = pty_sizes_before_layout
            .get(&pane.index())
            .expect("all pane sizes should be captured before the layout change");
        let expected_size = TerminalSize {
            cols: pane.geometry().cols(),
            rows: pane.geometry().rows(),
        };
        let current_size = wait_for_tty_size(
            tty_paths
                .get(&pane.index())
                .expect("all pane tty paths should remain addressable"),
            expected_size,
        )?;

        assert_ne!(
            *original_size,
            current_size,
            "pane {} PTY size should change after the layout cycle",
            pane.index()
        );
        assert_eq!(
            current_size,
            expected_size,
            "pane {} PTY size should match the computed geometry",
            pane.index()
        );
    }

    let last_pane_index = (PANE_COUNT - 1) as u32;

    let first_marker = "stress_edge_zero_marker";
    let first_pane_write = connection.roundtrip(&Request::SendKeys(SendKeysRequest {
        target: PaneTarget::new(session_name.clone(), 0),
        keys: vec![format!("printf '{first_marker}\\n'"), "Enter".to_owned()],
    }))?;
    assert_eq!(
        first_pane_write,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );
    let first_capture = wait_for_pane_capture(
        &mut connection,
        PaneTarget::new(session_name.clone(), 0),
        first_marker,
    )?;
    assert!(first_capture.contains(first_marker));

    let last_marker = "stress_edge_last_marker";
    let last_pane_write = connection.roundtrip(&Request::SendKeys(SendKeysRequest {
        target: PaneTarget::new(session_name.clone(), last_pane_index),
        keys: vec![format!("printf '{last_marker}\\n'"), "Enter".to_owned()],
    }))?;
    assert_eq!(
        last_pane_write,
        Response::SendKeys(SendKeysResponse { key_count: 2 })
    );
    let last_capture = wait_for_pane_capture(
        &mut connection,
        PaneTarget::new(session_name.clone(), last_pane_index),
        last_marker,
    )?;
    assert!(last_capture.contains(last_marker));

    drop(connection);
    shutdown_daemon(&runtime, handle)?;
    fs::remove_dir_all(tmpdir)?;
    Ok(())
}

#[test]
fn session_name_rewrites_preserve_server_state() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();
    let cases = [
        ("stress-names-empty", "", None, false),
        ("stress-names-colon", "a:b", Some("a_b"), true),
        ("stress-names-dot", "a..b", Some("a__b"), true),
    ];

    for (label, source_name, expected_name, should_succeed) in cases {
        let harness = CliHarness::new(label)?;
        let _cleanup = harness.auto_start_cleanup()?;
        let output = harness.run_with(&["new-session", "-d", "-s", source_name], |command| {
            command.env(BINARY_OVERRIDE_ENV, harness.launcher_path());
        })?;

        if should_succeed {
            assert_success(&output);
            assert!(
                stdout(&output).is_empty(),
                "stdout should stay empty for {label}"
            );
            assert!(
                stderr(&output).is_empty(),
                "stderr should stay empty for {label}"
            );
            assert!(
                harness.pid_path().exists(),
                "sanitized session names must launch the auto-start daemon for {label}"
            );
            assert!(
                harness.socket_path().exists(),
                "sanitized session names must create a socket for {label}"
            );
            assert_success(&harness.run(&["has-session", "-t", expected_name.expect("name")])?);
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert!(
                stdout(&output).is_empty(),
                "stdout should stay empty for {label}"
            );
            assert!(
                stderr(&output).contains("invalid session"),
                "stderr should report an invalid session for {label}, got: {}",
                stderr(&output)
            );
            assert!(
                !harness.pid_path().exists(),
                "invalid session names must not launch the auto-start daemon for {label}"
            );
            assert!(
                !harness.socket_path().exists(),
                "invalid session names must not create a socket for {label}"
            );
        }
    }

    let live_harness = CliHarness::new("stress-names-live")?;
    let mut live_daemon = live_harness.start_hidden_daemon()?;
    let stable_session = unique_session_name("stress-names-live");

    assert_success(&live_harness.run(&["new-session", "-d", "-s", stable_session.as_str()])?);

    for (_label, source_name, expected_name, should_succeed) in cases {
        let live_output = live_harness.run(&["new-session", "-d", "-s", source_name])?;
        if should_succeed {
            assert_success(&live_output);
            assert_success(&live_harness.run(&[
                "has-session",
                "-t",
                expected_name.expect("name"),
            ])?);
        } else {
            assert_eq!(live_output.status.code(), Some(1));
            assert!(stderr(&live_output).contains("invalid session"));
        }
        assert_success(&live_harness.run(&["has-session", "-t", stable_session.as_str()])?);
    }

    common::terminate_child(live_daemon.child_mut())?;
    Ok(())
}

fn pane_tty_path(
    connection: &mut rmux_client::Connection,
    target: PaneTarget,
) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let response = connection.roundtrip(&Request::DisplayMessage(
        rmux_proto::DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some("#{pane_tty}".to_owned()),
            empty_target_context: false,
        },
    ))?;
    let output = response
        .command_output()
        .ok_or("display-message -p #{pane_tty} did not return output")?;
    let path = std::str::from_utf8(output.stdout())?.trim();
    if path.is_empty() {
        return Err("display-message returned an empty pane_tty".into());
    }
    Ok(std::path::PathBuf::from(path))
}

fn wait_for_pane_capture(
    connection: &mut rmux_client::Connection,
    target: PaneTarget,
    marker: &str,
) -> Result<String, Box<dyn Error>> {
    for _ in 0..100 {
        // The edge panes in this stress layout can be one row tall, so the
        // visible screen may roll past the marker immediately after the shell
        // repaints. Capture the full transcript to verify delivery instead.
        let response =
            connection.roundtrip(&Request::CapturePane(Box::new(CapturePaneRequest {
                target: target.clone(),
                start: None,
                end: None,
                print: true,
                buffer_name: None,
                alternate: false,
                escape_ansi: false,
                escape_sequences: false,
                join_wrapped: false,
                use_mode_screen: false,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: true,
                end_is_absolute: true,
            })))?;
        let captured = std::str::from_utf8(
            response
                .command_output()
                .expect("capture-pane -p should return command output")
                .stdout(),
        )?
        .to_owned();
        if captured.contains(marker) {
            return Ok(captured);
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    Err(format!("capture-pane -p never surfaced marker {marker}").into())
}

#[test]
fn empty_send_keys_succeeds_through_the_cli() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();
    let harness = CliHarness::new("stress-sendkeys-empty")?;
    let runtime = runtime()?;
    let baseline_ttys = pane_tty_paths()?;
    let handle = runtime.block_on(
        ServerDaemon::new(DaemonConfig::new(harness.socket_path().to_path_buf())).bind(),
    )?;
    let session_name = unique_session_name("stress-sendkeys");
    let target = format!("{session_name}:0.0");

    assert_success(&harness.run(&["new-session", "-d", "-s", session_name.as_str()])?);
    let pane_zero_tty = single_new_tty(&baseline_ttys, &pane_tty_paths()?)?;

    let output = harness.run(&["send-keys", "-t", target.as_str()])?;
    assert_success(&output);
    assert!(
        !tty_has_input(&pane_zero_tty, Duration::from_millis(150))?,
        "empty send-keys must not write bytes into pane 0"
    );
    assert_success(&harness.run(&["has-session", "-t", session_name.as_str()])?);

    runtime.block_on(handle.shutdown())?;
    Ok(())
}

#[test]
fn runtime_resolved_targets_report_absent_server_without_autostart() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();

    for (label, target) in [
        ("stress-target-pane-window", "sess:x.0"),
        ("stress-target-trailing-colon", "sess:"),
        ("stress-target-trailing-dot", "sess:0."),
        ("stress-target-overflow-pane", "sess:0.999999999999"),
        ("stress-target-negative-pane", "sess:0.-1"),
        ("stress-target-empty-session", ":0"),
    ] {
        let harness = CliHarness::new(label)?;
        let output = harness.run(&["select-pane", "-t", target])?;

        assert_eq!(output.status.code(), Some(1), "{label} should exit 1");
        assert!(
            stdout(&output).is_empty(),
            "{label} should not print stdout"
        );
        assert!(stderr(&output).contains("no server running on"));
        assert!(
            !harness.socket_path().exists(),
            "{label} must fail without auto-starting a server"
        );
    }

    Ok(())
}

#[test]
fn missing_window_targets_report_explicit_errors() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();
    let harness = CliHarness::new("stress-missing-window")?;
    let session_name = unique_session_name("stress-missing-window");

    assert_success(&harness.run(&["new-session", "-d", "-s", session_name.as_str()])?);
    let output = harness.run(&["select-pane", "-t", &format!("{session_name}:5.0")])?;

    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).is_empty(), "stdout should be empty");
    assert!(
        stderr(&output).contains("can't find window: 5"),
        "stderr should report the missing window, got: {}",
        stderr(&output)
    );

    Ok(())
}

#[test]
fn multi_pane_kill_reaps_children_under_the_hidden_daemon() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();
    let harness = CliHarness::new("stress-zombies")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let session_name = unique_session_name("stress-zombies");

    assert_success(&harness.run(&["new-session", "-d", "-s", session_name.as_str()])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", session_name.as_str()])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", session_name.as_str()])?);
    assert_success(&harness.run(&["kill-session", "-t", session_name.as_str()])?);

    wait_for_no_child_processes(daemon.pid(), REAP_TIMEOUT)?;
    wait_for_socket_directory_empty(harness.socket_path(), REAP_TIMEOUT)?;

    common::terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn shutdown_removes_socket_files_after_all_sessions_are_killed() -> Result<(), Box<dyn Error>> {
    let _guard = serialize_test_execution();
    let runtime = runtime()?;
    let tmpdir = unique_tmpdir("stress-cleanup");
    fs::create_dir_all(&tmpdir)?;
    let socket_path = default_socket_path_in(&tmpdir)?;
    let handle =
        runtime.block_on(ServerDaemon::new(DaemonConfig::new(socket_path.clone())).bind())?;
    let session_name = unique_session_name("stress-cleanup");
    let mut connection = connect(&socket_path)?;

    let created = connection.roundtrip(&Request::NewSession(NewSessionRequest {
        session_name: session_name.clone(),
        detached: true,
        size: Some(TerminalSize { cols: 80, rows: 24 }),
        environment: None,
    }))?;
    assert!(matches!(created, Response::NewSession(_)));

    for _ in 0..2 {
        let split = connection.roundtrip(&Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session_name.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))?;
        assert!(matches!(split, Response::SplitWindow(_)));
    }

    let killed = connection.roundtrip(&Request::KillSession(KillSessionRequest {
        target: session_name.clone(),
        kill_all_except_target: false,
        clear_alerts: false,
    }));
    match killed {
        Ok(killed) => assert_eq!(
            killed,
            Response::KillSession(KillSessionResponse { existed: true })
        ),
        Err(ClientError::UnexpectedEof) => {}
        Err(error) => return Err(Box::new(error)),
    }
    drop(connection);

    runtime.block_on(handle.shutdown())?;
    assert_socket_directory_empty(&socket_path)?;
    fs::remove_dir_all(tmpdir)?;
    Ok(())
}
