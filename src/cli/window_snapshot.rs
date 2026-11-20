use super::{
    resolve_window_target_or_current, run_command_resolved, run_payload_command_resolved,
    ExitFailure,
};
use crate::cli_args::{ExportWindowSnapshotArgs, RestoreWindowSnapshotArgs};
use rmux_client::Connection;
use rmux_proto::CAPABILITY_RMUX_WINDOW_SNAPSHOT_V1;
use std::fs;
use std::io::{self, Read as _};
use std::path::Path;

#[derive(Clone, Copy)]
enum SnapshotVersion {
    V1,
    V2,
}

pub(super) fn run_export_window_snapshot(
    args: ExportWindowSnapshotArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    export_snapshot(args, socket_path, SnapshotVersion::V1)
}
pub(super) fn run_export_window_snapshot_v2(
    args: ExportWindowSnapshotArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    export_snapshot(args, socket_path, SnapshotVersion::V2)
}
fn export_snapshot(
    args: ExportWindowSnapshotArgs,
    socket_path: &Path,
    version: SnapshotVersion,
) -> Result<i32, ExitFailure> {
    let command = match version {
        SnapshotVersion::V1 => "export-window-snapshot",
        SnapshotVersion::V2 => "export-window-snapshot-v2",
    };
    run_payload_command_resolved(socket_path, command, move |connection| {
        if matches!(version, SnapshotVersion::V1) {
            require_window_snapshot_capability(connection, command)?;
        }
        let target = resolve_window_target_or_current(connection, args.target.as_ref(), command)?;
        match version {
            SnapshotVersion::V1 => connection.export_window_snapshot(target),
            SnapshotVersion::V2 => connection.export_window_snapshot_v2(target),
        }
        .map_err(ExitFailure::from_client)
    })
}
pub(super) fn run_restore_window_snapshot(
    args: RestoreWindowSnapshotArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    restore_snapshot(args, socket_path, SnapshotVersion::V1)
}
pub(super) fn run_restore_window_snapshot_v2(
    args: RestoreWindowSnapshotArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    restore_snapshot(args, socket_path, SnapshotVersion::V2)
}
fn restore_snapshot(
    args: RestoreWindowSnapshotArgs,
    socket_path: &Path,
    version: SnapshotVersion,
) -> Result<i32, ExitFailure> {
    let command = match version {
        SnapshotVersion::V1 => "restore-window-snapshot",
        SnapshotVersion::V2 => "restore-window-snapshot-v2",
    };
    run_command_resolved(socket_path, command, move |connection| {
        if matches!(version, SnapshotVersion::V1) {
            require_window_snapshot_capability(connection, command)?;
        }
        let json = read_window_snapshot_payload(&args.snapshot_path, command)?;
        let target = resolve_window_target_or_current(connection, args.target.as_ref(), command)?;
        match version {
            SnapshotVersion::V1 => {
                let snapshot =
                    rmux_proto::decode_rmux_window_snapshot_json(&json).map_err(|e| {
                        ExitFailure::new(1, format!("{command}: invalid RMUX window snapshot: {e}"))
                    })?;
                connection.restore_window_snapshot(target, snapshot)
            }
            SnapshotVersion::V2 => {
                let snapshot =
                    rmux_proto::decode_rmux_window_snapshot_v2_json(&json).map_err(|e| {
                        ExitFailure::new(1, format!("{command}: invalid RMUX window snapshot: {e}"))
                    })?;
                connection.restore_window_snapshot_v2(target, snapshot)
            }
        }
        .map_err(ExitFailure::from_client)
    })
}

fn require_window_snapshot_capability(
    connection: &mut Connection,
    command_name: &'static str,
) -> Result<(), ExitFailure> {
    if !connection
        .supports_capability(CAPABILITY_RMUX_WINDOW_SNAPSHOT_V1)
        .map_err(ExitFailure::from_client)?
    {
        return Err(ExitFailure::new(
            1,
            format!("{command_name} requires {CAPABILITY_RMUX_WINDOW_SNAPSHOT_V1}"),
        ));
    }
    Ok(())
}

fn read_window_snapshot_payload(path: &Path, command: &'static str) -> Result<String, ExitFailure> {
    if path == Path::new("-") {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).map_err(|error| {
            ExitFailure::new(1, format!("{command}: failed to read stdin: {error}"))
        })?;
        return Ok(input);
    }

    fs::read_to_string(path).map_err(|error| {
        ExitFailure::new(
            1,
            format!("{command}: failed to read {}: {error}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
    #[test]
    fn export_window_snapshot_capability_absence_stops_before_request() {
        use rmux_proto::{encode_frame, FrameDecoder, HandshakeResponse, Request, Response};
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fls1-snapshot-cap-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut decoder = FrameDecoder::new();
            let mut bytes = [0; 8192];
            let request = loop {
                if let Some(request) = decoder.next_frame::<Request>().unwrap() {
                    break request;
                }
                let count = stream.read(&mut bytes).unwrap();
                assert!(count > 0, "client closed before handshake");
                decoder.push_bytes(&bytes[..count]);
            };
            assert!(matches!(request, Request::Handshake(_)));
            stream
                .write_all(
                    &encode_frame(&Response::Handshake(HandshakeResponse {
                        wire_version: rmux_proto::RMUX_WIRE_VERSION,
                        capabilities: Vec::new(),
                    }))
                    .unwrap(),
                )
                .unwrap();
            let mut byte = [0];
            assert_eq!(
                stream.read(&mut byte).unwrap(),
                0,
                "client must not send export-window-snapshot without capability"
            );
        });

        let result = run_export_window_snapshot(
            ExportWindowSnapshotArgs {
                target: Some(crate::cli_args::parse_target_spec("alpha:0").unwrap()),
            },
            &path,
        );

        std::fs::remove_file(&path).unwrap();
        server.join().unwrap();
        let error = result.expect_err("missing capability must fail");
        assert!(error
            .message()
            .contains("export-window-snapshot requires commands.window_snapshot.v1"));
    }
}
