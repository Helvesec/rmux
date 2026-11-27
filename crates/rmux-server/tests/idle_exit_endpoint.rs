#![cfg(unix)]
//! An idle daemon keeps serving while a hook its last session fired still
//! runs, releases its endpoint when it stops accepting, not when its drain
//! ends, and kill-server keeps it until then.

use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_bootstrap::startup_unix::EndpointReservation;
use rmux_proto::{
    HookLifecycle, HookName, KillServerRequest, KillSessionRequest, ListSessionsRequest,
    NewSessionRequest, Request, Response, ScopeSelector, SetHookRequest, TerminalSize,
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

/// tmux 3.7b, measured with this hook and exit-empty: kill-session of the last
/// session does not stop the server while the hook runs as a waited job
/// (`job_still_running`). It keeps its socket, a new client is served
/// (`ls` rc=0), and the server exits once the hook has ended. S1987 asserted
/// the endpoint absent during this hook, the inverse of tmux: the hook ran in
/// the drain because exit-empty was committed before it. A client must still
/// never meet a bound endpoint that no longer answers, and the endpoint must
/// be free once the daemon has gone.
#[tokio::test]
async fn exit_empty_keeps_serving_while_its_session_closed_hook_runs() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("idle-exit-endpoint");
    let socket_path = harness.socket_path().to_path_buf();
    let (handle, marker) = server_with_slow_session_closed_hook(&harness, &["alpha"]).await?;

    kill_session(&socket_path, "alpha").await?;
    wait_for_marker(&marker).await?;
    // Let the daemon pass its accept loop even on a loaded host.
    sleep(Duration::from_millis(300)).await;

    let connect = std::os::unix::net::UnixStream::connect(&socket_path);
    assert!(
        connect.is_ok(),
        "the daemon stopped serving while its session-closed hook still runs: {connect:?}"
    );
    drop(connect);
    let response = send_request(
        &socket_path,
        &Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }),
    )
    .await?;
    assert!(
        matches!(response, Response::ListSessions(_)),
        "a new client is served while the hook runs: {response:?}"
    );

    tokio::time::timeout(Duration::from_secs(HOOK_SECONDS + 10), handle.wait()).await??;
    assert!(!socket_path.exists(), "the exited daemon left its endpoint");
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
