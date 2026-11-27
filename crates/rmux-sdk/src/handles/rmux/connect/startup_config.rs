use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use rmux_proto::{DaemonStatusRequest, Request, Response};

use crate::transport::{OperationDeadline, TransportClient};
use crate::{Result, RmuxError};

const CONFIG_READY_POLL_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(any(windows, test))]
const WINDOWS_STARTUP_READY_EVENT_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
type SpawnedDaemon = super::startup_child::PendingDaemon;
#[cfg(not(unix))]
type SpawnedDaemon = ();

pub(super) fn spawn_hidden_daemon(
    endpoint: &OsStr,
    caller_cwd: Option<&Path>,
    deadline: OperationDeadline,
) -> io::Result<SpawnedDaemon> {
    let candidates = super::hidden_daemon_binary_candidates();
    let mut last_error = None;
    for (index, binary) in candidates.iter().enumerate() {
        let result = spawn_hidden_daemon_with_binary(endpoint, binary, caller_cwd, deadline);
        match result {
            Ok(child) => return Ok(child),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && index + 1 < candidates.len() =>
            {
                last_error = Some(error);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                last_error = Some(error);
                break;
            }
            Err(error) => return Err(error),
        }
    }

    Err(super::hidden_daemon_not_found_error(
        &candidates,
        last_error,
    ))
}

fn spawn_hidden_daemon_with_binary(
    endpoint: &OsStr,
    binary: &OsStr,
    caller_cwd: Option<&Path>,
    deadline: OperationDeadline,
) -> io::Result<SpawnedDaemon> {
    if matches!(deadline.remaining_timeout(), Some(remaining) if remaining.is_zero()) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "rmux hidden daemon startup deadline elapsed before launch",
        ));
    }
    let verified = rmux_os::build_identity::resolve(binary)?;
    let binary = verified.as_os_str();
    #[cfg(windows)]
    let ready = rmux_os::daemon::StartupReadyEvent::new()?;
    let mut command = hidden_daemon_command(binary, endpoint, caller_cwd);
    #[cfg(windows)]
    command.arg("--startup-ready-event").arg(ready.name());
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, true);
    #[cfg(windows)]
    let child = rmux_os::daemon::launch_hidden_daemon(command.get_program(), command.get_args())?;
    #[cfg(not(windows))]
    let child = rmux_os::daemon::spawn_hidden_daemon_command_requiring_job_breakaway(&mut command)?;
    #[cfg(not(unix))]
    drop(child);
    #[cfg(windows)]
    {
        let wait_timeout = startup_ready_event_wait_timeout(deadline);
        if !wait_timeout.is_zero() {
            let _ = ready.wait(wait_timeout);
        }
    }
    #[cfg(unix)]
    {
        Ok(SpawnedDaemon::new(child))
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn startup_ready_event_wait_timeout(deadline: OperationDeadline) -> Duration {
    deadline
        .remaining_timeout()
        .map_or(WINDOWS_STARTUP_READY_EVENT_TIMEOUT, |remaining| {
            remaining.min(WINDOWS_STARTUP_READY_EVENT_TIMEOUT)
        })
}

pub(super) fn hidden_daemon_command(
    binary: &OsStr,
    endpoint: &OsStr,
    caller_cwd: Option<&Path>,
) -> Command {
    let mut command = Command::new(binary);
    command.arg(super::INTERNAL_DAEMON_FLAG).arg(endpoint);
    rmux_bootstrap::hidden_daemon::append_config_args(
        &mut command,
        rmux_bootstrap::hidden_daemon::ConfigSelection::Default,
        true,
        caller_cwd,
        None,
        None,
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

pub(super) async fn wait_until_loaded(
    transport: &TransportClient,
    deadline: OperationDeadline,
) -> Result<()> {
    loop {
        let response = transport
            .with_operation_deadline(deadline)
            .request(Request::DaemonStatus(DaemonStatusRequest))
            .await?;
        match response {
            Response::DaemonStatus(status) if status.config_loading => {}
            Response::DaemonStatus(_) => return Ok(()),
            Response::Error(error) => return Err(RmuxError::from_proto(error.error)),
            response => {
                return Err(RmuxError::from_proto(rmux_proto::RmuxError::Server(
                    format!("unexpected startup readiness response: {response:?}"),
                )))
            }
        }

        let sleep_for = deadline
            .remaining_timeout()
            .map_or(CONFIG_READY_POLL_INTERVAL, |remaining| {
                remaining.min(CONFIG_READY_POLL_INTERVAL)
            });
        tokio::time::sleep(sleep_for).await;
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    use rmux_proto::{
        encode_frame, DaemonStatusResponse, ErrorResponse, FrameDecoder, Request, Response,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        hidden_daemon_command, startup_ready_event_wait_timeout, wait_until_loaded,
        WINDOWS_STARTUP_READY_EVENT_TIMEOUT,
    };
    use crate::transport::{OperationDeadline, TransportClient};
    use crate::RmuxError;

    #[test]
    fn hidden_daemon_loads_default_config_quietly_from_exact_caller_cwd() {
        let caller_cwd = Path::new("caller dir-é");
        let command = hidden_daemon_command(
            OsStr::new("rmux-daemon"),
            OsStr::new("endpoint with spaces"),
            Some(caller_cwd),
        );

        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new(super::super::INTERNAL_DAEMON_FLAG),
                OsStr::new("endpoint with spaces"),
                OsStr::new("--config-default"),
                OsStr::new("--config-quiet"),
                OsStr::new("--config-cwd"),
                caller_cwd.as_os_str(),
            ]
        );
    }

    #[test]
    fn hidden_daemon_argv_preserves_raw_endpoint_without_cwd() {
        #[cfg(unix)]
        let raw = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![b'x', 255])
        };
        #[cfg(windows)]
        let raw = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[120, 0xd800])
        };
        let command = hidden_daemon_command(OsStr::new("unused"), &raw, None);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("--__internal-daemon"),
                &raw,
                OsStr::new("--config-default"),
                OsStr::new("--config-quiet"),
            ]
        );
    }

    #[test]
    fn windows_startup_event_wait_uses_only_the_remaining_operation_budget() {
        let short = OperationDeadline::from_timeout(Some(Duration::from_millis(50)));
        assert!(startup_ready_event_wait_timeout(short) <= Duration::from_millis(50));

        let unbounded = OperationDeadline::from_timeout(None);
        assert_eq!(
            startup_ready_event_wait_timeout(unbounded),
            WINDOWS_STARTUP_READY_EVENT_TIMEOUT
        );
    }

    #[tokio::test]
    async fn readiness_waits_until_config_loading_is_false() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client_stream);
        let server = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server_stream).await,
                Request::DaemonStatus(_)
            ));
            write_status(&mut server_stream, true).await;
            assert!(matches!(
                read_request(&mut server_stream).await,
                Request::DaemonStatus(_)
            ));
            write_status(&mut server_stream, false).await;
        });

        wait_until_loaded(
            &transport,
            OperationDeadline::from_timeout(Some(Duration::from_secs(1))),
        )
        .await
        .expect("config readiness completes");
        server.await.expect("mock daemon does not panic");
    }

    #[tokio::test]
    async fn readiness_rpc_uses_the_existing_deadline() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client_stream);
        let server = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server_stream).await,
                Request::DaemonStatus(_)
            ));
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let error = wait_until_loaded(
            &transport,
            OperationDeadline::from_timeout(Some(Duration::from_millis(30))),
        )
        .await
        .expect_err("silent readiness response must time out");
        match error {
            RmuxError::Transport { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::TimedOut);
            }
            error => panic!("expected transport timeout, got {error:?}"),
        }
        server.abort();
    }

    #[tokio::test]
    async fn readiness_rejects_daemon_status_errors() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client_stream);
        let server = tokio::spawn(async move {
            assert!(matches!(
                read_request(&mut server_stream).await,
                Request::DaemonStatus(_)
            ));
            write_response(
                &mut server_stream,
                Response::Error(ErrorResponse {
                    error: rmux_proto::RmuxError::Server("status unavailable".to_owned()),
                }),
            )
            .await;
        });

        let error = wait_until_loaded(
            &transport,
            OperationDeadline::from_timeout(Some(Duration::from_secs(1))),
        )
        .await
        .expect_err("daemon-status errors cannot prove config readiness");
        assert!(matches!(error, RmuxError::Protocol { .. }));
        server.await.expect("mock daemon does not panic");
    }

    async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 256];
        loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return request;
            }
            let read = stream.read(&mut buffer).await.expect("request bytes");
            assert_ne!(read, 0, "client closed before request arrived");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_status(stream: &mut tokio::io::DuplexStream, config_loading: bool) {
        write_response(
            stream,
            Response::DaemonStatus(DaemonStatusResponse {
                rmux_version: "0.9.0".to_owned(),
                wire_version: rmux_proto::RMUX_WIRE_VERSION,
                session_count: 0,
                client_count: 0,
                config_loading,
            }),
        )
        .await;
    }

    async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("status response encodes");
        stream.write_all(&frame).await.expect("status response");
        stream.flush().await.expect("flush status response");
    }
}

#[cfg(all(test, unix))]
mod fd_inheritance_tests {
    mod support {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/daemon_fd_inheritance.rs"
        ));
    }

    #[test]
    fn sdk_hidden_daemon_closes_high_inherited_fd() {
        support::check("handles::rmux::connect::startup_config::fd_inheritance_tests::sdk_hidden_daemon_closes_high_inherited_fd", |binary, endpoint| {
            super::spawn_hidden_daemon_with_binary(
                endpoint.as_os_str(), binary.as_os_str(), None,
                super::OperationDeadline::from_timeout(None),
            ).unwrap().detach().unwrap()
        });
    }
}
