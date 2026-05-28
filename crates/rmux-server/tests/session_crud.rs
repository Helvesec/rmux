#![cfg(unix)]

use std::error::Error;
use std::io;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::Arc;

mod common;

use common::{
    create_stale_socket, send_request, session_name, start_server, wait_for_socket_removal,
    ClientConnection, TestHarness,
};
use rmux_proto::{
    AttachSessionRequest, HasSessionRequest, KillSessionRequest, NewSessionRequest,
    RenameSessionRequest, Request, Response, RmuxError, TerminalSize,
};
use rmux_server::{DaemonConfig, ServerDaemon};
use tokio::net::UnixStream;
use tokio::sync::Barrier;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn socket_file_is_created_connectable_and_removed_on_shutdown() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("socket-lifecycle");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    assert_eq!(handle.socket_path(), socket_path.as_path());
    assert!(socket_path.exists());

    let _connection = UnixStream::connect(&socket_path).await?;

    handle.shutdown().await?;

    assert!(!socket_path.exists());
    Ok(())
}

#[tokio::test]
async fn new_session_round_trips_through_the_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("new-session");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let response = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: None,
        }),
    )
    .await?;

    assert_eq!(
        response,
        Response::NewSession(rmux_proto::NewSessionResponse {
            session_name: session_name("alpha"),
            detached: true,
            output: None,
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn duplicate_new_session_returns_duplicate_session() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("duplicate-session");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let request = Request::NewSession(NewSessionRequest {
        session_name: session_name("alpha"),
        detached: false,
        size: None,
        environment: None,
    });

    let first = send_request(&socket_path, &request).await?;
    let duplicate = send_request(&socket_path, &request).await?;

    assert!(matches!(first, Response::NewSession(_)));
    assert_eq!(
        duplicate,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::DuplicateSession("alpha".to_owned()),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn has_session_reports_live_and_missing_sessions() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("has-session");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let missing = send_request(
        &socket_path,
        &Request::HasSession(HasSessionRequest {
            target: session_name("alpha"),
        }),
    )
    .await?;
    assert_eq!(
        missing,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );

    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let present = send_request(
        &socket_path,
        &Request::HasSession(HasSessionRequest {
            target: session_name("alpha"),
        }),
    )
    .await?;
    assert_eq!(
        present,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn kill_session_is_live_then_idempotent() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("kill-session");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let keepalive = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("keepalive"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(keepalive, Response::NewSession(_)));

    let removed = send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let idempotent = send_request(
        &socket_path,
        &Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await?;
    assert_eq!(
        idempotent,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::SessionNotFound("alpha".to_owned()),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn rename_session_round_trips_through_the_real_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("rename-session");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let renamed = send_request(
        &socket_path,
        &Request::RenameSession(RenameSessionRequest {
            target: session_name("alpha"),
            new_name: session_name("gamma"),
        }),
    )
    .await?;
    assert_eq!(
        renamed,
        Response::RenameSession(rmux_proto::RenameSessionResponse {
            session_name: session_name("gamma"),
        })
    );

    assert_eq!(
        send_request(
            &socket_path,
            &Request::HasSession(HasSessionRequest {
                target: session_name("alpha"),
            }),
        )
        .await?,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );
    assert_eq!(
        send_request(
            &socket_path,
            &Request::HasSession(HasSessionRequest {
                target: session_name("gamma"),
            }),
        )
        .await?,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn stale_socket_removal_allows_rebind() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("stale-rebind");
    let socket_path = harness.socket_path().to_path_buf();
    let listener = create_stale_socket(&socket_path)?;
    drop(listener);
    wait_for_stale_socket(&socket_path).await?;

    let handle = start_server(&harness).await?;

    let _connection = UnixStream::connect(&socket_path).await?;
    handle.shutdown().await?;
    Ok(())
}

async fn wait_for_stale_socket(socket_path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    for _ in 0..200 {
        match StdUnixStream::connect(socket_path) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                return Ok(());
            }
            Ok(stream) => drop(stream),
            Err(error) => return Err(error.into()),
        }

        sleep(Duration::from_millis(10)).await;
    }

    Err(io::Error::other(format!(
        "socket '{}' never transitioned into a stale state",
        socket_path.display()
    ))
    .into())
}

#[tokio::test]
async fn live_socket_is_not_unlinked_during_rebind_attempt() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("live-rebind");
    let socket_path = harness.socket_path().to_path_buf();
    let first_handle = start_server(&harness).await?;

    let error = ServerDaemon::new(DaemonConfig::new(socket_path.clone()))
        .bind()
        .await
        .expect_err("second bind should fail while live server owns the socket");

    assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
    let _connection = UnixStream::connect(&socket_path).await?;

    first_handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn socket_file_is_removed_after_handle_drop() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("drop-cleanup");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    assert!(socket_path.exists());
    drop(handle);

    wait_for_socket_removal(&socket_path).await?;
    Ok(())
}

#[tokio::test]
async fn attach_session_returns_an_upgrade_response() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("attach-upgrade");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let response = send_request(
        &socket_path,
        &Request::AttachSession(AttachSessionRequest {
            target: session_name("alpha"),
        }),
    )
    .await?;

    assert_eq!(
        response,
        Response::AttachSession(rmux_proto::AttachSessionResponse {
            session_name: session_name("alpha"),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn switch_and_detach_require_an_attached_client_before_any_session_lookup(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("attached-client-required");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let switch_response = send_request(
        &socket_path,
        &Request::SwitchClient(rmux_proto::SwitchClientRequest {
            target: session_name("alpha"),
        }),
    )
    .await?;

    assert_eq!(
        switch_response,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::Message("no current client".to_owned()),
        })
    );

    let missing_switch_response = send_request(
        &socket_path,
        &Request::SwitchClient(rmux_proto::SwitchClientRequest {
            target: session_name("missing"),
        }),
    )
    .await?;

    assert_eq!(
        missing_switch_response,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::Message("no current client".to_owned()),
        })
    );

    let detach_response = send_request(
        &socket_path,
        &Request::DetachClient(rmux_proto::DetachClientRequest),
    )
    .await?;
    assert_eq!(
        detach_response,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::Server("detach-client requires an attached client".to_owned()),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn persistent_connection_handles_multiple_requests() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("persistent-connection");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;

    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let present = client
        .send_request(&Request::HasSession(HasSessionRequest {
            target: session_name("alpha"),
        }))
        .await?;
    assert_eq!(
        present,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );

    let removed = client
        .send_request(&Request::KillSession(KillSessionRequest {
            target: session_name("alpha"),
            kill_all_except_target: false,
            clear_alerts: false,
        }))
        .await?;
    assert_eq!(
        removed,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_closes_existing_connections() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("shutdown-closes-connections");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let mut client = ClientConnection::connect(&socket_path).await?;
    let created = client
        .send_request(&Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }))
        .await?;
    assert!(matches!(created, Response::NewSession(_)));
    handle.shutdown().await?;
    let error = client
        .send_request(&Request::HasSession(HasSessionRequest {
            target: session_name("alpha"),
        }))
        .await
        .expect_err("shutdown should close existing client connections");
    let io_error = error
        .downcast_ref::<io::Error>()
        .expect("connection failure should surface as an io::Error");
    assert!(matches!(
        io_error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
    ));
    Ok(())
}

#[tokio::test]
async fn concurrent_duplicate_creates_are_serialized() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("concurrent-duplicate");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;
    let barrier = Arc::new(Barrier::new(3));
    let request = Request::NewSession(NewSessionRequest {
        session_name: session_name("alpha"),
        detached: true,
        size: None,
        environment: None,
    });

    let first_task = spawn_request_task(socket_path.clone(), request.clone(), barrier.clone());
    let second_task = spawn_request_task(socket_path.clone(), request, barrier.clone());

    barrier.wait().await;

    let first = first_task.await.map_err(io::Error::other)??;
    let second = second_task.await.map_err(io::Error::other)??;
    let duplicate = Response::Error(rmux_proto::ErrorResponse {
        error: RmuxError::DuplicateSession("alpha".to_owned()),
    });
    let responses = [first, second];

    assert_eq!(
        responses
            .iter()
            .filter(|response| matches!(response, Response::NewSession(_)))
            .count(),
        1
    );
    assert_eq!(
        responses
            .iter()
            .filter(|response| **response == duplicate)
            .count(),
        1
    );

    handle.shutdown().await?;
    Ok(())
}

fn spawn_request_task(
    socket_path: std::path::PathBuf,
    request: Request,
    barrier: Arc<Barrier>,
) -> tokio::task::JoinHandle<Result<Response, io::Error>> {
    tokio::spawn(async move {
        barrier.wait().await;
        send_request(&socket_path, &request)
            .await
            .map_err(|error| io::Error::other(error.to_string()))
    })
}
