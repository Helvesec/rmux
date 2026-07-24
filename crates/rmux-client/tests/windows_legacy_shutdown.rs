#![cfg(windows)]

use std::io;

use rmux_client::{connect, connect_for_server_shutdown, socket_path_for_label, ClientError};
use rmux_ipc::{LocalEndpoint, LocalListener};

#[test]
fn shutdown_connection_alone_can_select_the_recorded_legacy_endpoint() -> io::Result<()> {
    let label = format!("legacy-shutdown-client-{}", std::process::id());
    let managed_path = socket_path_for_label(&label).map_err(client_io_error)?;
    let legacy_path = legacy_path_for(&label, &managed_path);
    let legacy_endpoint = LocalEndpoint::from_path(legacy_path.clone());
    let _legacy_daemon = LocalListener::bind(&legacy_endpoint)?;

    let ordinary_error = connect(&managed_path).expect_err("managed endpoint should be absent");
    assert!(matches!(
        ordinary_error,
        ClientError::Io(ref error) if error.kind() == io::ErrorKind::NotFound
    ));

    let (connection, selected_path) =
        connect_for_server_shutdown(&managed_path).map_err(client_io_error)?;
    assert_eq!(selected_path, legacy_path);
    drop(connection);
    Ok(())
}

fn legacy_path_for(label: &str, managed_path: &std::path::Path) -> std::path::PathBuf {
    let managed = managed_path.to_string_lossy();
    let marker = managed
        .rfind("-g-")
        .expect("managed endpoint contains generation marker");
    std::path::PathBuf::from(format!("{}-{label}", &managed[..marker]))
}

fn client_io_error(error: ClientError) -> io::Error {
    match error {
        ClientError::Io(error) => error,
        error => io::Error::other(error),
    }
}
