#![cfg(unix)]

mod common;

use std::error::Error;

use common::{send_request, start_server, TestHarness};
use rmux_proto::{
    IfShellRequest, Request, Response, RunShellRequest, SetBufferRequest, WaitForMode,
    WaitForRequest,
};

#[tokio::test]
async fn run_shell_foreground_returns_status_without_stdout() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("run-shell-foreground");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let response = send_request(
        &socket_path,
        &Request::RunShell(Box::new(RunShellRequest {
            command: "printf server".to_owned(),
            background: false,
            as_commands: false,
            show_stderr: false,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })),
    )
    .await?;

    let Response::RunShell(response) = response else {
        panic!("unexpected response: {response:?}");
    };
    assert_eq!(response.exit_status(), Some(0));
    assert!(response.command_output().is_none());
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn if_shell_rejects_unsupported_nested_command() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("if-shell-unsupported");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let response = send_request(
        &socket_path,
        &Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "unsupported-command".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })),
    )
    .await?;

    assert!(matches!(response, Response::Error(_)));
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn if_shell_returns_nested_command_output() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("if-shell-output");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let set_buffer = send_request(
        &socket_path,
        &Request::SetBuffer(SetBufferRequest {
            name: Some("selected".to_owned()),
            content: b"yes".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: false,
        }),
    )
    .await?;
    assert!(matches!(set_buffer, Response::SetBuffer(_)));

    let response = send_request(
        &socket_path,
        &Request::IfShell(Box::new(IfShellRequest {
            condition: "1".to_owned(),
            format_mode: true,
            then_command: "show-buffer -b selected".to_owned(),
            else_command: None,
            target: None,
            caller_cwd: None,
            background: false,
        })),
    )
    .await?;

    assert!(matches!(response, Response::IfShell(_)));
    assert_eq!(
        response.command_output().expect("if-shell output").stdout(),
        b"yes"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn wait_for_signal_without_waiters_is_not_latched() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("wait-for-signal-no-waiters");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let response = send_request(
        &socket_path,
        &Request::WaitFor(WaitForRequest {
            channel: "empty".to_owned(),
            mode: WaitForMode::Signal,
        }),
    )
    .await?;

    assert!(matches!(response, Response::WaitFor(_)));
    handle.shutdown().await?;
    Ok(())
}
