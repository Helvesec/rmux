#![cfg(windows)]

use std::io::ErrorKind;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use rmux_ipc::{
    connect_blocking, connect_windows_pipe, endpoint_for_label, reserve_managed_endpoint_start,
    wait_for_peer_close, LocalListener,
};
use rmux_os::identity::{IdentityResolver, UserIdentity};
use rmux_os::path::socket_paths_match;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::time::timeout;

const WINDOWS_IPC_TEST_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_GENERATION_HELPER_ENV: &str = "RMUX_TEST_STALE_GENERATION_LABEL";
const STALE_GENERATION_OUTPUT_ENV: &str = "RMUX_TEST_STALE_GENERATION_OUTPUT";
const DEAD_RUNNING_HELPER_ENV: &str = "RMUX_TEST_DEAD_RUNNING_GENERATION_LABEL";
const DEAD_RUNNING_OUTPUT_ENV: &str = "RMUX_TEST_DEAD_RUNNING_GENERATION_OUTPUT";
const RESOLVE_ONLY_HELPER_ENV: &str = "RMUX_TEST_RESOLVE_ONLY_LABEL";
const RESOLVE_ONLY_READY_ENV: &str = "RMUX_TEST_RESOLVE_ONLY_READY";
const RESOLVE_ONLY_RELEASE_ENV: &str = "RMUX_TEST_RESOLVE_ONLY_RELEASE";

#[test]
fn blocking_connect_to_missing_pipe_reports_not_found() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("missing-{}", std::process::id()))?;

    let error = connect_blocking(&endpoint, Duration::from_millis(100))
        .expect_err("missing pipe should not connect");

    assert!(
        error.kind() == ErrorKind::NotFound || matches!(error.raw_os_error(), Some(2)),
        "unexpected missing-pipe error: {error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn named_pipe_roundtrip_uses_bound_endpoint() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("integration-{}", std::process::id()))?;
    let current_user = IdentityResolver::current()?;
    let listener = LocalListener::bind(&endpoint)?;

    let accept = tokio::spawn(async move {
        let (mut stream, peer) = listener.accept().await?;
        assert_eq!(peer.pid, std::process::id());
        assert_eq!(peer.user, current_user);
        assert!(matches!(peer.user, UserIdentity::Sid(ref sid) if sid.starts_with("S-")));

        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).await?;
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").await?;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;

    let endpoint_for_client = endpoint.clone();
    let client = timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            connect_blocking(&endpoint_for_client, WINDOWS_IPC_TEST_TIMEOUT)
        }),
    )
    .await
    .expect("client connect timed out")
    .expect("client connect task")?;

    timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let mut client = client;
            client.write_all(b"ping")?;
            let mut response = [0_u8; 4];
            client.read_exact(&mut response)?;
            assert_eq!(&response, b"pong");
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("client roundtrip timed out")
    .expect("client roundtrip task")?;

    timeout(WINDOWS_IPC_TEST_TIMEOUT, accept)
        .await
        .expect("accept task timed out")
        .expect("accept task")?;
    Ok(())
}

#[tokio::test]
async fn socket_path_comparison_does_not_consume_a_pipe_instance() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("path-compare-{}", std::process::id()))?;
    let mut server = ServerOptions::new().create(endpoint.as_pipe_name())?;
    let pipe_path = Path::new(endpoint.as_pipe_name());

    assert!(socket_paths_match(pipe_path, pipe_path));

    let mut client = ClientOptions::new().open(endpoint.as_pipe_name())?;
    timeout(WINDOWS_IPC_TEST_TIMEOUT, server.connect())
        .await
        .expect("real client connect timed out")?;
    client.write_all(b"real").await?;
    let mut request = [0_u8; 4];
    server.read_exact(&mut request).await?;
    assert_eq!(&request, b"real");
    Ok(())
}

#[tokio::test]
async fn async_named_pipe_connect_uses_the_verified_client_boundary() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("async-verified-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;

    let accept = tokio::spawn(async move {
        let (mut stream, _peer) = listener.accept().await?;
        let mut request = [0_u8; 4];
        stream.read_exact(&mut request).await?;
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").await?;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;
    let mut client = timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        connect_windows_pipe(endpoint.as_pipe_name()),
    )
    .await
    .expect("verified async client connect timed out")?;
    client.write_all(b"ping").await?;
    let mut response = [0_u8; 4];
    client.read_exact(&mut response).await?;
    assert_eq!(&response, b"pong");

    timeout(WINDOWS_IPC_TEST_TIMEOUT, accept)
        .await
        .expect("accept task timed out")
        .expect("accept task")?;
    Ok(())
}

#[tokio::test]
async fn named_pipe_read_timeout_bounds_silent_server() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("read-timeout-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;

    let accept = tokio::spawn(async move {
        let (_stream, _peer) = listener.accept().await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;

    let endpoint_for_client = endpoint.clone();
    timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let mut client = connect_blocking(&endpoint_for_client, WINDOWS_IPC_TEST_TIMEOUT)?;
            client.set_read_timeout(Some(Duration::from_millis(100)))?;
            let mut byte = [0_u8; 1];
            let error = client
                .read_exact(&mut byte)
                .expect_err("silent server should hit the read timeout");
            assert_eq!(error.kind(), ErrorKind::TimedOut);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("client read task timed out")
    .expect("client read task")?;

    timeout(WINDOWS_IPC_TEST_TIMEOUT, accept)
        .await
        .expect("accept task timed out")
        .expect("accept task")?;
    Ok(())
}

#[tokio::test]
async fn wait_for_peer_close_resolves_when_named_pipe_client_disconnects() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("peer-close-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;

    let accept = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await?;
        timeout(WINDOWS_IPC_TEST_TIMEOUT, wait_for_peer_close(&stream))
            .await
            .expect("peer close wait timed out")?;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;

    let endpoint_for_client = endpoint.clone();
    timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let client = connect_blocking(&endpoint_for_client, WINDOWS_IPC_TEST_TIMEOUT)?;
            drop(client);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("client connect/drop timed out")
    .expect("client connect/drop task")?;

    timeout(WINDOWS_IPC_TEST_TIMEOUT, accept)
        .await
        .expect("accept task timed out")
        .expect("accept task")?;
    Ok(())
}

#[tokio::test]
async fn wait_for_peer_close_keeps_polling_after_buffered_bytes() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("peer-close-buffered-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;

    let accept = tokio::spawn(async move {
        let (stream, _peer) = listener.accept().await?;
        timeout(WINDOWS_IPC_TEST_TIMEOUT, wait_for_peer_close(&stream))
            .await
            .expect("peer close wait timed out after buffered bytes")?;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;

    let endpoint_for_client = endpoint.clone();
    timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let mut client = connect_blocking(&endpoint_for_client, WINDOWS_IPC_TEST_TIMEOUT)?;
            client.write_all(b"buffered")?;
            std::thread::sleep(Duration::from_millis(100));
            drop(client);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("client write/drop timed out")
    .expect("client write/drop task")?;

    timeout(WINDOWS_IPC_TEST_TIMEOUT, accept)
        .await
        .expect("accept task timed out")
        .expect("accept task")?;
    Ok(())
}

#[tokio::test]
async fn first_pipe_instance_rejects_second_listener() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("squat-{}", std::process::id()))?;
    let _first = LocalListener::bind(&endpoint)?;
    let second = LocalListener::bind(&endpoint).expect_err("second listener should fail");

    assert_bind_conflict(second);
    Ok(())
}

#[tokio::test]
async fn listener_exposes_a_bounded_pending_pipe_backlog() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("bounded-backlog-{}", std::process::id()))?;
    let _listener = LocalListener::bind(&endpoint)?;

    let mut clients = Vec::new();
    for _ in 0..4 {
        clients.push(ClientOptions::new().open(endpoint.as_pipe_name())?);
    }

    let fifth = ClientOptions::new()
        .open(endpoint.as_pipe_name())
        .expect_err("fifth client should wait for the bounded backlog to replenish");

    assert!(
        matches!(fifth.raw_os_error(), Some(231) | Some(233) | Some(2)),
        "unexpected fifth-client error while backlog is occupied: {fifth:?}"
    );
    drop(clients);
    Ok(())
}

#[tokio::test]
async fn listener_accepts_next_client_after_abandoned_instance() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("abandoned-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;

    let endpoint_for_abandoned = endpoint.clone();
    timeout(
        WINDOWS_IPC_TEST_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            let client = ClientOptions::new().open(endpoint_for_abandoned.as_pipe_name())?;
            drop(client);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("abandoned client open timed out")
    .expect("abandoned client task")?;

    let _ = timeout(WINDOWS_IPC_TEST_TIMEOUT, listener.accept())
        .await
        .expect("first accept should observe the abandoned instance");

    let endpoint_for_client = endpoint.clone();
    let client = tokio::task::spawn_blocking(move || {
        let mut client = connect_blocking(&endpoint_for_client, WINDOWS_IPC_TEST_TIMEOUT)?;
        client.write_all(b"ok")?;
        std::io::Result::Ok(())
    });

    let (mut stream, _peer) = timeout(WINDOWS_IPC_TEST_TIMEOUT, listener.accept())
        .await
        .expect("listener should accept a later healthy client")?;
    let mut bytes = [0_u8; 2];
    stream.read_exact(&mut bytes).await?;
    assert_eq!(&bytes, b"ok");

    timeout(WINDOWS_IPC_TEST_TIMEOUT, client)
        .await
        .expect("healthy client timed out")
        .expect("healthy client task")?;
    Ok(())
}

#[tokio::test]
async fn first_pipe_instance_rejects_preexisting_raw_server() -> std::io::Result<()> {
    let endpoint = endpoint_for_label(format!("raw-squat-{}", std::process::id()))?;
    let _squatter = ServerOptions::new().create(endpoint.as_pipe_name())?;
    let error = LocalListener::bind(&endpoint).expect_err("rmux bind should reject occupied pipe");

    assert_bind_conflict(error);
    Ok(())
}

#[test]
fn concurrent_label_resolution_defers_ownership_until_reservation() -> std::io::Result<()> {
    let label = format!("generation-concurrency-{}", std::process::id());
    let barrier = Arc::new(Barrier::new(12));
    let threads = (0..12)
        .map(|_| {
            let label = label.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                endpoint_for_label(label).map(|endpoint| endpoint.into_path())
            })
        })
        .collect::<Vec<_>>();

    let paths = threads
        .into_iter()
        .map(|thread| thread.join().expect("resolver thread"))
        .collect::<std::io::Result<Vec<_>>>()?;
    let winner = reserve_managed_endpoint_start(&paths[0])?;
    assert!(winner.is_owner());
    for candidate in paths.iter().skip(1) {
        let follower = reserve_managed_endpoint_start(candidate)?;
        assert!(!follower.is_owner());
        assert_eq!(follower.endpoint(), winner.endpoint());
    }
    Ok(())
}

#[tokio::test]
async fn resolve_only_process_cannot_publish_a_starting_owner() -> std::io::Result<()> {
    let label = format!("generation-resolve-only-{}", std::process::id());
    let ready = std::env::temp_dir().join(format!("{label}-ready.txt"));
    let release = std::env::temp_dir().join(format!("{label}-release.txt"));
    let mut child = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("resolve_only_helper")
        .arg("--nocapture")
        .env(RESOLVE_ONLY_HELPER_ENV, &label)
        .env(RESOLVE_ONLY_READY_ENV, &ready)
        .env(RESOLVE_ONLY_RELEASE_ENV, &release)
        .spawn()?;

    wait_for_file(&ready)?;
    let candidate = endpoint_for_label(&label)?;
    let reservation = reserve_managed_endpoint_start(candidate.as_path());
    std::fs::write(&release, b"continue")?;
    let status = child.wait()?;
    let _ = std::fs::remove_file(&ready);
    let _ = std::fs::remove_file(&release);
    assert!(status.success(), "resolve-only helper failed");

    let reservation = reservation?;
    assert!(
        reservation.is_owner(),
        "a resolver that never starts a daemon must not own discovery state"
    );
    let listener = LocalListener::bind(reservation.endpoint())?;
    drop(listener);
    Ok(())
}

#[tokio::test]
async fn stopped_listener_rotates_away_from_preclaimed_old_generation() -> std::io::Result<()> {
    let label = format!("generation-restart-squat-{}", std::process::id());
    let first = endpoint_for_label(&label)?;
    let first_path = first.clone().into_path();
    let listener = LocalListener::bind(&first)?;
    drop(listener);

    let _old_generation_squatter = ServerOptions::new()
        .first_pipe_instance(true)
        .create(first.as_pipe_name())?;
    let replacement = endpoint_for_label(&label)?;

    assert_ne!(
        replacement.as_path(),
        first_path,
        "restart must never reuse the publicly observed stopped generation"
    );
    let replacement_listener = LocalListener::bind(&replacement)?;
    drop(replacement_listener);
    Ok(())
}

#[tokio::test]
async fn concurrent_restart_reservation_rejects_the_abandoned_child_generation(
) -> std::io::Result<()> {
    let label = format!("generation-restart-reservation-{}", std::process::id());
    let abandoned_endpoint = endpoint_for_label(&label)?;
    let abandoned_path = abandoned_endpoint.as_path().to_path_buf();

    let abandoned_claim = reserve_managed_endpoint_start(&abandoned_path)?;
    assert!(abandoned_claim.is_owner());
    drop(abandoned_claim);

    let winner = reserve_managed_endpoint_start(&abandoned_path)?;
    assert!(winner.is_owner());
    assert_ne!(winner.endpoint().as_path(), abandoned_path);

    let loser = reserve_managed_endpoint_start(&abandoned_path)?;
    assert!(!loser.is_owner());
    assert_eq!(loser.endpoint(), winner.endpoint());

    let stale_error = LocalListener::bind(&abandoned_endpoint)
        .expect_err("an abandoned child must not bind after generation rotation");
    assert_eq!(stale_error.kind(), ErrorKind::PermissionDenied);

    let replacement_listener = LocalListener::bind(winner.endpoint())?;
    drop(replacement_listener);
    Ok(())
}

#[test]
fn dead_starting_process_cannot_pin_a_generation() -> std::io::Result<()> {
    let label = format!("generation-dead-owner-{}", std::process::id());
    let output = std::env::temp_dir().join(format!(
        "rmux-generation-dead-owner-{}.txt",
        std::process::id()
    ));
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("stale_generation_helper")
        .arg("--nocapture")
        .env(STALE_GENERATION_HELPER_ENV, &label)
        .env(STALE_GENERATION_OUTPUT_ENV, &output)
        .status()?;
    assert!(status.success(), "stale-generation helper failed");

    let stale = std::fs::read_to_string(&output)?;
    let replacement = endpoint_for_label(&label)?;
    assert_ne!(
        replacement.as_path().to_string_lossy(),
        stale,
        "a dead resolver process must force generation rotation"
    );
    let _ = std::fs::remove_file(output);
    Ok(())
}

#[test]
fn daemon_death_between_resolve_and_reservation_rotates_the_generation() -> std::io::Result<()> {
    let label = format!("generation-dead-running-{}", std::process::id());
    let output = std::env::temp_dir().join(format!(
        "rmux-generation-dead-running-{}.txt",
        std::process::id()
    ));
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("dead_running_generation_helper")
        .arg("--nocapture")
        .env(DEAD_RUNNING_HELPER_ENV, &label)
        .env(DEAD_RUNNING_OUTPUT_ENV, &output)
        .status()?;
    assert!(status.success(), "dead-running helper failed");

    let stale = std::fs::read_to_string(&output)?;
    let reservation = reserve_managed_endpoint_start(Path::new(stale.trim()))?;
    assert!(reservation.is_owner());
    assert_ne!(
        reservation.endpoint().as_path().to_string_lossy(),
        stale.trim(),
        "a dead Running owner must rotate during the atomic reservation"
    );
    let _ = std::fs::remove_file(output);
    Ok(())
}

#[test]
fn stale_generation_helper() -> std::io::Result<()> {
    let Some(label) = std::env::var_os(STALE_GENERATION_HELPER_ENV) else {
        return Ok(());
    };
    let output = std::env::var_os(STALE_GENERATION_OUTPUT_ENV)
        .ok_or_else(|| std::io::Error::other("missing stale-generation output path"))?;
    let endpoint = endpoint_for_label(label)?;
    std::fs::write(output, endpoint.as_path().to_string_lossy().as_bytes())
}

#[tokio::test]
async fn dead_running_generation_helper() -> std::io::Result<()> {
    let Some(label) = std::env::var_os(DEAD_RUNNING_HELPER_ENV) else {
        return Ok(());
    };
    let output = std::env::var_os(DEAD_RUNNING_OUTPUT_ENV)
        .ok_or_else(|| std::io::Error::other("missing dead-running output path"))?;
    let endpoint = endpoint_for_label(label)?;
    let listener = LocalListener::bind(&endpoint)?;
    std::fs::write(output, endpoint.as_path().to_string_lossy().as_bytes())?;
    std::mem::forget(listener);
    std::process::exit(0);
}

#[test]
fn resolve_only_helper() -> std::io::Result<()> {
    let Some(label) = std::env::var_os(RESOLVE_ONLY_HELPER_ENV) else {
        return Ok(());
    };
    let ready = std::env::var_os(RESOLVE_ONLY_READY_ENV)
        .ok_or_else(|| std::io::Error::other("missing resolve-only ready path"))?;
    let release = std::env::var_os(RESOLVE_ONLY_RELEASE_ENV)
        .ok_or_else(|| std::io::Error::other("missing resolve-only release path"))?;
    let endpoint = endpoint_for_label(label)?;
    std::fs::write(ready, endpoint.as_path().to_string_lossy().as_bytes())?;
    wait_for_file(Path::new(&release))
}

fn wait_for_file(path: &Path) -> std::io::Result<()> {
    for _ in 0..200 {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(std::io::Error::new(
        ErrorKind::TimedOut,
        format!("timed out waiting for {}", path.display()),
    ))
}

fn assert_bind_conflict(error: std::io::Error) {
    assert!(
        matches!(
            error.kind(),
            ErrorKind::PermissionDenied | ErrorKind::AlreadyExists
        ) || matches!(error.raw_os_error(), Some(5) | Some(231)),
        "unexpected bind error: {error:?}"
    );
}
