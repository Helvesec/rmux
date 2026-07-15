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

pub(super) fn spawn_hidden_daemon(
    endpoint: &OsStr,
    caller_cwd: Option<&Path>,
    deadline: OperationDeadline,
) -> io::Result<()> {
    let candidates = super::hidden_daemon_binary_candidates();
    let mut last_error = None;
    for (index, binary) in candidates.iter().enumerate() {
        let result =
            match spawn_hidden_daemon_with_binary(endpoint, binary, caller_cwd, deadline, true) {
                Ok(()) => return Ok(()),
                Err(error)
                    if rmux_os::daemon::should_retry_hidden_daemon_without_breakaway(&error) =>
                {
                    spawn_hidden_daemon_with_binary(endpoint, binary, caller_cwd, deadline, false)
                }
                Err(error) => Err(error),
            };
        match result {
            Ok(()) => return Ok(()),
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
    allow_job_breakaway: bool,
) -> io::Result<()> {
    if matches!(deadline.remaining_timeout(), Some(remaining) if remaining.is_zero()) {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "rmux hidden daemon startup deadline elapsed before launch",
        ));
    }
    #[cfg(windows)]
    let ready = rmux_os::daemon::StartupReadyEvent::new()?;
    let mut command = hidden_daemon_command(binary, endpoint, caller_cwd);
    #[cfg(windows)]
    command.arg("--startup-ready-event").arg(ready.name());
    rmux_os::daemon::configure_hidden_daemon_command(&mut command, allow_job_breakaway);
    let child = rmux_os::daemon::spawn_hidden_daemon_command(&mut command)?;
    drop(child);
    #[cfg(windows)]
    {
        let wait_timeout = startup_ready_event_wait_timeout(deadline);
        if !wait_timeout.is_zero() {
            let _ = ready.wait(wait_timeout);
        }
    }
    Ok(())
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
    command
        .arg(super::INTERNAL_DAEMON_FLAG)
        .arg(endpoint)
        .arg("--config-default")
        .arg("--config-quiet");
    if let Some(caller_cwd) = caller_cwd {
        command.arg("--config-cwd").arg(caller_cwd);
    }
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
            Response::Error(error) => return Err(error.into()),
            response => {
                return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
                    "unexpected startup readiness response: {response:?}"
                ))))
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
