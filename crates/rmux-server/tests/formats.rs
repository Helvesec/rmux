#![cfg(unix)]

use std::error::Error;
mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_proto::{
    DisplayMessageRequest, ListWindowsRequest, NewSessionRequest, NewWindowRequest, Request,
    Response, Target, TerminalSize,
};

const FORMAT_FIELD_SEPARATOR: char = '\x1f';

fn default_shell_window_name() -> String {
    "bash".to_owned()
}

fn assert_window_format_line(
    line: &str,
    window_index: &str,
    window_name: &str,
    raw_flags: &str,
    active: &str,
    last: &str,
    conditional: &str,
) {
    let fields: Vec<&str> = line.split(FORMAT_FIELD_SEPARATOR).collect();
    assert_eq!(fields.len(), 14, "unexpected format fields: {fields:?}");
    assert_eq!(fields[0], "alpha");
    assert_eq!(fields[1], "2");
    assert_eq!(fields[2], "0");
    assert_eq!(fields[3], "x");
    assert_eq!(fields[4], window_index);
    assert_eq!(fields[5], window_name);
    assert_eq!(fields[6], raw_flags);
    assert_eq!(fields[7], active);
    assert_eq!(fields[8], last);
    assert_eq!(fields[9], format!("@{window_index}"));
    assert_eq!(fields[10], "");
    assert_eq!(fields[11], format!("{window_index}{window_name}alpha"));
    assert!(
        !fields[12].contains("#{"),
        "pane title should be resolved, not leaked as a format token"
    );
    assert_eq!(fields[13], conditional);
}

fn is_unix_pty_path(path: &str) -> bool {
    path.starts_with("/dev/pts/") || path.starts_with("/dev/ttys")
}

#[tokio::test]
async fn list_windows_uses_shared_formatter_through_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("formats-list-windows");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: Some(vec![
                "SHELL=/bin/bash".to_owned(),
                "TERM_PROGRAM=tmux".to_owned(),
            ]),
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let new_window = send_request(
        harness.socket_path(),
        &Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("logs".to_owned()),
            detached: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })),
    )
    .await?;
    assert!(matches!(new_window, Response::NewWindow(_)));

    let listed = send_request(
        harness.socket_path(),
        &Request::ListWindows(ListWindowsRequest {
            target: alpha,
            format: Some(
                "#{session_name}\x1f#{session_windows}\x1f#{session_attached}\x1f#{session_width}x#{session_height}\x1f#{window_index}\x1f#{window_name}\x1f#{window_raw_flags}\x1f#{window_active}\x1f#{window_last_flag}\x1f#{window_id}\x1f#{missing}\x1f#I#W#S\x1f#{=21:pane_title}\x1f#{?window_active,yes,no}"
                    .to_owned(),
            ),
            }),
    )
    .await?;

    let output = listed
        .command_output()
        .expect("list-windows returns command output");
    let stdout = std::str::from_utf8(output.stdout()).expect("list-windows output is utf-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "unexpected list-windows output: {stdout:?}");
    assert_window_format_line(
        lines[0].trim_end(),
        "0",
        &default_shell_window_name(),
        "-",
        "0",
        "1",
        "no",
    );
    assert_window_format_line(lines[1].trim_end(), "1", "logs", "*", "1", "0", "yes");

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn nested_conditionals_expand_inner_templates_through_real_socket(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("formats-nested-conditionals");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let new_window = send_request(
        harness.socket_path(),
        &Request::NewWindow(Box::new(NewWindowRequest {
            target: alpha.clone(),
            name: Some("logs".to_owned()),
            detached: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })),
    )
    .await?;
    assert!(matches!(new_window, Response::NewWindow(_)));

    let listed = send_request(
        harness.socket_path(),
        &Request::ListWindows(ListWindowsRequest {
            target: alpha,
            format: Some(
                "#{?window_active,#{window_name},#{?window_last_flag,last,#{session_name}}}"
                    .to_owned(),
            ),
        }),
    )
    .await?;

    let output = listed
        .command_output()
        .expect("list-windows returns command output");
    assert_eq!(
        std::str::from_utf8(output.stdout()).expect("list-windows output is utf-8"),
        "last\nlogs\n"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn display_message_session_target_includes_active_pane_runtime_context(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("formats-display-session-pane-context");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");

    let created = send_request(
        harness.socket_path(),
        &Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let displayed = send_request(
        harness.socket_path(),
        &Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Session(alpha)),
            print: true,
            message: Some(
                "#{session_name}|#{window_index}|#{pane_index}|#{pane_current_path}|#{pane_pid}|#{pane_tty}|#{socket_path}"
                    .to_owned(),
            ),
            empty_target_context: false,
            }),
    )
    .await?;

    let output = displayed
        .command_output()
        .expect("display-message -p returns command output");
    let stdout = std::str::from_utf8(output.stdout()).expect("display-message output is utf-8");
    let fields: Vec<&str> = stdout.trim_end().split('|').collect();
    assert_eq!(fields.len(), 7);
    assert_eq!(fields[0], "alpha");
    assert_eq!(fields[1], "0");
    assert_eq!(fields[2], "0");
    assert!(!fields[3].is_empty(), "pane_current_path must be populated");
    assert!(fields[4].parse::<u32>().is_ok(), "pane_pid must be numeric");
    assert!(is_unix_pty_path(fields[5]), "pane_tty must be a pty");
    assert_eq!(fields[6], harness.socket_path().to_string_lossy());

    handle.shutdown().await?;
    Ok(())
}
