#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

mod common;

use common::{
    read_response_exact, send_request, session_name, start_server, ClientConnection, TestHarness,
};
use rmux_proto::{
    encode_frame, AttachSessionRequest, ErrorResponse, HookLifecycle, HookName, KillSessionRequest,
    NewSessionRequest, Request, Response, RmuxError, ScopeSelector, SetHookRequest, TerminalSize,
};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

const ATTACH_SETTLE_DELAY: Duration = Duration::from_millis(50);
const STEP_TIMEOUT: Duration = Duration::from_millis(150);
const WAIT_TIMEOUT: Duration = Duration::from_secs(2);

#[tokio::test]
async fn persistent_client_attached_hooks_run_on_every_attach() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("persistent-client-attached-hook");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let output_path = hook_output_path(&socket_path);

    create_session(&socket_path, "alpha").await?;
    register_hook(
        &socket_path,
        ScopeSelector::Global,
        "printf ab >> {path} && printf cd >> {path}",
        &output_path,
        HookLifecycle::Persistent,
    )
    .await?;

    attach_once(&socket_path, "alpha").await?;
    attach_once(&socket_path, "alpha").await?;
    wait_for_file_contents(&output_path, "abcdabcd").await?;

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn one_shot_client_attached_hooks_are_removed_after_dispatch() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("oneshot-client-attached-hook");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let output_path = hook_output_path(&socket_path);

    create_session(&socket_path, "keepalive").await?;
    create_session(&socket_path, "alpha").await?;
    register_hook(
        &socket_path,
        ScopeSelector::Session(session_name("alpha")),
        "printf once >> {path}",
        &output_path,
        HookLifecycle::OneShot,
    )
    .await?;

    attach_once(&socket_path, "alpha").await?;
    attach_once(&socket_path, "alpha").await?;
    wait_for_file_contents(&output_path, "once").await?;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(fs::read_to_string(&output_path)?, "once");

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn session_scoped_hooks_are_cleared_when_sessions_are_killed() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("session-hook-cleanup-on-kill");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let output_path = hook_output_path(&socket_path);

    create_session(&socket_path, "keepalive")
        .await
        .map_err(|error| format!("create keepalive: {error}"))?;
    create_session(&socket_path, "alpha")
        .await
        .map_err(|error| format!("create alpha before hook: {error}"))?;
    register_hook(
        &socket_path,
        ScopeSelector::Session(session_name("alpha")),
        "printf stale >> {path}",
        &output_path,
        HookLifecycle::Persistent,
    )
    .await
    .map_err(|error| format!("register alpha hook: {error}"))?;
    kill_session(&socket_path, "alpha")
        .await
        .map_err(|error| format!("kill alpha: {error}"))?;
    create_session(&socket_path, "alpha")
        .await
        .map_err(|error| format!("recreate alpha: {error}"))?;

    attach_once(&socket_path, "alpha")
        .await
        .map_err(|error| format!("attach recreated alpha: {error}"))?;
    sleep(Duration::from_millis(100)).await;
    assert!(
        !output_path.exists(),
        "recreated sessions must not inherit prior session-scoped hooks"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn session_created_hooks_run_only_after_successful_creates() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("session-created-hook");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let output_path = hook_output_path(&socket_path);

    register_hook_for(
        &socket_path,
        ScopeSelector::Global,
        HookName::SessionCreated,
        "printf created > {path}",
        &output_path,
        HookLifecycle::Persistent,
    )
    .await?;

    create_session(&socket_path, "alpha").await?;
    wait_for_file_contents(&output_path, "created").await?;
    fs::remove_file(&output_path)?;

    let duplicate = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(duplicate, Response::Error(_)));
    sleep(Duration::from_millis(100)).await;
    assert!(
        !output_path.exists(),
        "failed session creates must not run session-created hooks"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn slow_hooks_do_not_block_attach_completion() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("nonblocking-client-attached-hook");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let output_path = hook_output_path(&socket_path);

    create_session(&socket_path, "alpha").await?;
    register_hook(
        &socket_path,
        ScopeSelector::Global,
        "sleep 0.3; printf ready > {path}",
        &output_path,
        HookLifecycle::Persistent,
    )
    .await?;

    let (_, attach_stream) = timeout(STEP_TIMEOUT, async {
        ClientConnection::connect(&socket_path)
            .await?
            .begin_attach(AttachSessionRequest {
                target: session_name("alpha"),
            })
            .await
    })
    .await??;

    assert!(
        !output_path.exists(),
        "slow hook should not complete before attach returns"
    );
    wait_for_file_contents(&output_path, "ready").await?;

    drop(attach_stream);
    sleep(ATTACH_SETTLE_DELAY).await;
    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn invalid_hook_event_wire_values_are_rejected() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("invalid-hook-event");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut stream = UnixStream::connect(&socket_path).await?;
    let mut frame = encode_frame(&Request::SetHook(SetHookRequest {
        scope: ScopeSelector::Global,
        hook: HookName::ClientAttached,
        command: "true".to_owned(),
        lifecycle: HookLifecycle::Persistent,
    }))?;

    assert_eq!(&frame[12..16], &[0, 0, 0, 0]);
    frame[12..16].copy_from_slice(&70_u32.to_le_bytes());
    stream.write_all(&frame).await?;

    match read_response_exact(&mut stream).await? {
        Response::Error(ErrorResponse {
            error: RmuxError::Decode(message),
        }) => assert!(
            message.contains("variant"),
            "expected enum-variant decode failure, received: {message}"
        ),
        other => panic!("unexpected response for invalid hook event: {other:?}"),
    }

    create_session(&socket_path, "alpha").await?;
    handle.shutdown().await?;
    Ok(())
}

async fn create_session(socket_path: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let response = send_request(
        socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;

    assert!(matches!(response, Response::NewSession(_)));
    Ok(())
}

async fn kill_session(socket_path: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let response = send_request(
        socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name(name),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }),
    )
    .await?;

    assert_eq!(
        response,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );
    Ok(())
}

async fn register_hook(
    socket_path: &Path,
    scope: ScopeSelector,
    command_template: &str,
    output_path: &Path,
    lifecycle: HookLifecycle,
) -> Result<(), Box<dyn Error>> {
    register_hook_for(
        socket_path,
        scope,
        HookName::ClientAttached,
        command_template,
        output_path,
        lifecycle,
    )
    .await
}

async fn register_hook_for(
    socket_path: &Path,
    scope: ScopeSelector,
    hook: HookName,
    command_template: &str,
    output_path: &Path,
    lifecycle: HookLifecycle,
) -> Result<(), Box<dyn Error>> {
    let shell_command = command_template.replace("{path}", &shell_quote(output_path));
    let command = format!("run-shell {}", shell_quote_str(&shell_command));
    let response = send_request(
        socket_path,
        &Request::SetHook(SetHookRequest {
            scope: scope.clone(),
            hook,
            command,
            lifecycle,
        }),
    )
    .await?;

    assert_eq!(
        response,
        Response::SetHook(rmux_proto::SetHookResponse {
            scope,
            hook,
            lifecycle,
        })
    );
    Ok(())
}

async fn attach_once(socket_path: &Path, name: &str) -> Result<(), Box<dyn Error>> {
    let (_, attach_stream) = ClientConnection::connect(socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: session_name(name),
        })
        .await?;

    drop(attach_stream);
    sleep(ATTACH_SETTLE_DELAY).await;
    Ok(())
}

fn hook_output_path(socket_path: &Path) -> std::path::PathBuf {
    socket_path
        .parent()
        .expect("test harness socket path has a parent directory")
        .join("hook-output.txt")
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn shell_quote_str(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn wait_for_file_contents(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    for _ in 0..100 {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) | Err(_) => sleep(Duration::from_millis(20)).await,
        }
    }

    Err(io::Error::other(format!(
        "file '{}' never reached expected contents '{expected}' within {WAIT_TIMEOUT:?}",
        path.display()
    ))
    .into())
}
