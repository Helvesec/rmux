#![cfg(unix)]
//! An idle daemon releases its endpoint when it stops accepting, not when its
//! drain ends; kill-server keeps it until then.

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_bootstrap::startup_unix::EndpointReservation;
use rmux_proto::{
    HookLifecycle, HookName, KillServerRequest, KillSessionRequest, NewSessionRequest, Request,
    Response, ScopeSelector, SetHookRequest, TerminalSize,
};
use tokio::time::{sleep, Instant};

/// Long enough for every assertion below while the hook keeps the drain open.
const HOOK_SECONDS: u64 = 2;
const MARKER_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts a server with `sessions` and a `session-closed` hook that keeps the
/// shutdown drain open for [`HOOK_SECONDS`] after touching a marker.
async fn server_with_slow_session_closed_hook(
    harness: &TestHarness,
    sessions: &[&str],
) -> Result<(rmux_server::ServerHandle, PathBuf), Box<dyn Error>> {
    let socket_path = harness.socket_path();
    let handle = start_server(harness).await?;
    let marker = socket_path.with_extension("hook-ran");
    let response = send_request(
        socket_path,
        &Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::SessionClosed,
            command: format!(
                "run-shell \"touch '{}'; sleep {HOOK_SECONDS}\"",
                marker.display()
            ),
            lifecycle: HookLifecycle::Persistent,
        }),
    )
    .await?;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
    for name in sessions {
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
        common::assert_new_session(&response);
    }
    Ok((handle, marker))
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
    assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    Ok(())
}

async fn wait_for_marker(marker: &Path) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + MARKER_TIMEOUT;
    while !marker.exists() {
        if Instant::now() >= deadline {
            return Err(io::Error::other("session-closed hook never ran").into());
        }
        sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// S1987: killing the last session made the exit-empty daemon stop accepting
/// but keep its socket bound through the drain, so a starting client was
/// queued there and then reset (Linux) or closed (macOS) unanswered.
#[tokio::test]
async fn exit_empty_releases_its_endpoint_but_holds_the_startup_lock_while_draining(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("idle-exit-endpoint");
    let socket_path = harness.socket_path().to_path_buf();
    let (handle, marker) = server_with_slow_session_closed_hook(&harness, &["alpha"]).await?;

    kill_session(&socket_path, "alpha").await?;
    wait_for_marker(&marker).await?;
    // Let the daemon pass its accept loop even on a loaded host.
    sleep(Duration::from_millis(300)).await;

    let connect = std::os::unix::net::UnixStream::connect(&socket_path);
    assert!(
        matches!(&connect, Err(error) if error.kind() == io::ErrorKind::NotFound),
        "the draining daemon still owns its endpoint: {connect:?}"
    );
    let reservation = EndpointReservation::acquire(&socket_path, Duration::from_millis(50)).await;
    assert!(
        reservation.is_err(),
        "no next daemon may start before the draining one has finished"
    );

    tokio::time::timeout(Duration::from_secs(HOOK_SECONDS + 10), handle.wait()).await??;
    EndpointReservation::acquire(&socket_path, Duration::from_millis(50)).await?;
    Ok(())
}

/// kill-server reports completion when the endpoint disappears, so it keeps
/// its endpoint until its panes and hooks are gone.
#[tokio::test]
async fn kill_server_keeps_its_endpoint_until_the_drain_ends() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-server-endpoint");
    let socket_path = harness.socket_path().to_path_buf();
    let (handle, marker) =
        server_with_slow_session_closed_hook(&harness, &["alpha", "beta"]).await?;

    // Closing one of two sessions runs the hook without an idle exit; the
    // kill-server that follows must then drain it.
    kill_session(&socket_path, "alpha").await?;
    wait_for_marker(&marker).await?;
    // kill-server may close the connection before it answers.
    let _ = send_request(&socket_path, &Request::KillServer(KillServerRequest)).await;
    sleep(Duration::from_millis(300)).await;

    assert!(
        socket_path.exists(),
        "kill-server released its endpoint before its drain ended"
    );

    tokio::time::timeout(Duration::from_secs(HOOK_SECONDS + 10), handle.wait()).await??;
    assert!(!socket_path.exists());
    Ok(())
}
