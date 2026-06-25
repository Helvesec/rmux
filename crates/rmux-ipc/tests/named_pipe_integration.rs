#![cfg(windows)]

use std::io::ErrorKind;
use std::io::{Read, Write};
use std::time::Duration;

use rmux_ipc::{connect_blocking, endpoint_for_label, wait_for_peer_close, LocalListener};
use rmux_os::identity::{IdentityResolver, UserIdentity};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::net::windows::named_pipe::ServerOptions;
use tokio::time::timeout;

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
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            connect_blocking(&endpoint_for_client, Duration::from_secs(2))
        }),
    )
    .await
    .expect("client connect timed out")
    .expect("client connect task")?;

    timeout(
        Duration::from_secs(2),
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

    timeout(Duration::from_secs(2), accept)
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
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            let mut client = connect_blocking(&endpoint_for_client, Duration::from_secs(2))?;
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

    timeout(Duration::from_secs(2), accept)
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
        timeout(Duration::from_secs(2), wait_for_peer_close(&stream))
            .await
            .expect("peer close wait timed out")?;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;

    let endpoint_for_client = endpoint.clone();
    timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            let client = connect_blocking(&endpoint_for_client, Duration::from_secs(2))?;
            drop(client);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("client connect/drop timed out")
    .expect("client connect/drop task")?;

    timeout(Duration::from_secs(2), accept)
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
        timeout(Duration::from_secs(2), wait_for_peer_close(&stream))
            .await
            .expect("peer close wait timed out after buffered bytes")?;
        std::io::Result::Ok(())
    });

    tokio::task::yield_now().await;

    let endpoint_for_client = endpoint.clone();
    timeout(
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            let mut client = connect_blocking(&endpoint_for_client, Duration::from_secs(2))?;
            client.write_all(b"buffered")?;
            std::thread::sleep(Duration::from_millis(100));
            drop(client);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("client write/drop timed out")
    .expect("client write/drop task")?;

    timeout(Duration::from_secs(2), accept)
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
        Duration::from_secs(2),
        tokio::task::spawn_blocking(move || {
            let client = ClientOptions::new().open(endpoint_for_abandoned.as_pipe_name())?;
            drop(client);
            std::io::Result::Ok(())
        }),
    )
    .await
    .expect("abandoned client open timed out")
    .expect("abandoned client task")?;

    let _ = timeout(Duration::from_secs(2), listener.accept())
        .await
        .expect("first accept should observe the abandoned instance");

    let endpoint_for_client = endpoint.clone();
    let client = tokio::task::spawn_blocking(move || {
        let mut client = connect_blocking(&endpoint_for_client, Duration::from_secs(2))?;
        client.write_all(b"ok")?;
        std::io::Result::Ok(())
    });

    let (mut stream, _peer) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .expect("listener should accept a later healthy client")?;
    let mut bytes = [0_u8; 2];
    stream.read_exact(&mut bytes).await?;
    assert_eq!(&bytes, b"ok");

    timeout(Duration::from_secs(2), client)
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

fn assert_bind_conflict(error: std::io::Error) {
    assert!(
        matches!(
            error.kind(),
            ErrorKind::PermissionDenied | ErrorKind::AlreadyExists
        ) || matches!(error.raw_os_error(), Some(5) | Some(231)),
        "unexpected bind error: {error:?}"
    );
}
