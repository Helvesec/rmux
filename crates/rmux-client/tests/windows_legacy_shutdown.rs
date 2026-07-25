#![cfg(windows)]

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use rmux_client::{connect, connect_for_server_shutdown, socket_path_for_label, ClientError};
use rmux_ipc::{LocalEndpoint, LocalListener, PeerIdentity};
use rmux_proto::{FrameDecoder, Request};
use tokio::io::AsyncReadExt;
use tokio::time::timeout;

const LEGACY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// `LocalListener::bind` builds Tokio named-pipe servers, so it must run inside
/// a runtime, while the client half is deliberately blocking and builds its own
/// runtime in `rmux_ipc::connect_blocking`. Nesting those two is a panic, so the
/// daemon lives on the test runtime and the client work runs on a blocking task.
#[tokio::test]
async fn shutdown_connection_alone_can_select_the_recorded_legacy_endpoint() -> io::Result<()> {
    let label = format!("legacy-shutdown-client-{}", std::process::id());
    let managed_path = socket_path_for_label(&label).map_err(client_io_error)?;
    let legacy_path = legacy_path_for(&label, &managed_path);
    let legacy_endpoint = LocalEndpoint::from_path(legacy_path.clone());
    let legacy_daemon = LocalListener::bind(&legacy_endpoint)?;

    let accepted = tokio::spawn(async move { read_shutdown_request(legacy_daemon).await });
    tokio::task::yield_now().await;

    let selected_path = timeout(
        LEGACY_SHUTDOWN_TIMEOUT,
        tokio::task::spawn_blocking(move || request_legacy_shutdown(&managed_path)),
    )
    .await
    .expect("legacy shutdown probe timed out")
    .expect("legacy shutdown probe task")?;
    assert_eq!(selected_path, legacy_path);

    let (peer, request) = timeout(LEGACY_SHUTDOWN_TIMEOUT, accepted)
        .await
        .expect("legacy daemon accept timed out")
        .expect("legacy daemon accept task")?;
    assert_eq!(peer.pid, std::process::id());
    assert!(
        matches!(request, Request::KillServer(_)),
        "legacy daemon received {request:?} instead of a kill-server request"
    );
    Ok(())
}

/// Runs the blocking client half: the managed generation must look absent to an
/// ordinary command, and only the shutdown path may reach the legacy endpoint.
fn request_legacy_shutdown(managed_path: &std::path::Path) -> io::Result<PathBuf> {
    let ordinary_error = connect(managed_path).expect_err("managed endpoint should be absent");
    assert!(
        matches!(
            ordinary_error,
            ClientError::Io(ref error) if error.kind() == io::ErrorKind::NotFound
        ),
        "ordinary connect reported {ordinary_error:?} instead of an absent endpoint"
    );

    let (mut connection, selected_path) =
        connect_for_server_shutdown(managed_path).map_err(client_io_error)?;
    connection
        .kill_server_after_write()
        .map_err(client_io_error)?;
    drop(connection);
    Ok(selected_path)
}

/// Stands in for the pre-rotation daemon: accepts one client and decodes the
/// first request frame it sends.
async fn read_shutdown_request(listener: LocalListener) -> io::Result<(PeerIdentity, Request)> {
    let (mut stream, peer) = listener.accept().await?;
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match decoder.next_frame::<Request>() {
            Ok(Some(request)) => return Ok((peer, request)),
            Ok(None) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        let bytes_read = stream.read(&mut buffer).await?;
        if bytes_read == 0 {
            return Err(io::Error::other(
                "legacy daemon closed before a shutdown request arrived",
            ));
        }
        decoder.push_bytes(&buffer[..bytes_read]);
    }
}

fn legacy_path_for(label: &str, managed_path: &std::path::Path) -> PathBuf {
    let managed = managed_path.to_string_lossy();
    let marker = managed
        .rfind("-g-")
        .expect("managed endpoint contains generation marker");
    PathBuf::from(format!("{}-{label}", &managed[..marker]))
}

fn client_io_error(error: ClientError) -> io::Error {
    match error {
        ClientError::Io(error) => error,
        error => io::Error::other(error),
    }
}
