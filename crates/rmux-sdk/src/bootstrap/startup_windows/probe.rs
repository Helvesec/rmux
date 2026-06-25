use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use rmux_ipc::{connect_blocking, BlockingLocalStream, LocalEndpoint};
use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, Request, Response, RmuxError, SessionName,
};
use tokio::time::sleep;
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_NOT_CONNECTED,
};

use super::{StartupError, PROBE_CONNECT_TIMEOUT, PROBE_IO_TIMEOUT, PROBE_SESSION_NAME};
use crate::bootstrap::deadline::StartupDeadline;

pub(super) async fn probe_responsive(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
) -> Result<Option<BlockingLocalStream>, StartupError> {
    let endpoint_owned = endpoint.clone();
    let pipe_owned = pipe_name.to_path_buf();
    tokio::task::spawn_blocking(move || probe_blocking(&endpoint_owned, &pipe_owned))
        .await
        .map_err(|error| StartupError::PipeIo {
            operation: "join probe task",
            pipe_name: pipe_name.to_path_buf(),
            source: io::Error::other(format!("startup probe join failed: {error}")),
        })?
}

pub(super) fn probe_blocking(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
) -> Result<Option<BlockingLocalStream>, StartupError> {
    let mut stream = match connect_blocking(endpoint, PROBE_CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) => return classify_connect_error(error, pipe_name).map(|()| None),
    };

    stream
        .set_write_timeout(Some(PROBE_IO_TIMEOUT))
        .map_err(|source| StartupError::PipeIo {
            operation: "set probe write timeout",
            pipe_name: pipe_name.to_path_buf(),
            source,
        })?;
    stream
        .set_read_timeout(Some(PROBE_IO_TIMEOUT))
        .map_err(|source| StartupError::PipeIo {
            operation: "set probe read timeout",
            pipe_name: pipe_name.to_path_buf(),
            source,
        })?;

    let target = SessionName::new(PROBE_SESSION_NAME).map_err(|error| StartupError::PipeIo {
        operation: "build probe session name",
        pipe_name: pipe_name.to_path_buf(),
        source: io::Error::other(error),
    })?;
    let request = Request::HasSession(HasSessionRequest { target });
    let frame = encode_frame(&request).map_err(|error| StartupError::PipeIo {
        operation: "encode probe frame",
        pipe_name: pipe_name.to_path_buf(),
        source: io::Error::other(error),
    })?;

    if let Err(error) = stream.write_all(&frame).and_then(|()| stream.flush()) {
        return classify_io_error(error, "send probe frame", pipe_name).map(|()| None);
    }

    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Ok(None),
            Ok(bytes_read) => decoder.push_bytes(&buffer[..bytes_read]),
            Err(error) => {
                return classify_io_error(error, "read probe response", pipe_name).map(|()| None)
            }
        }
        match decoder.next_frame::<Response>() {
            Ok(Some(Response::HasSession(_))) => return Ok(Some(stream)),
            Ok(Some(response)) => {
                return Err(StartupError::PipeIo {
                    operation: "validate probe response",
                    pipe_name: pipe_name.to_path_buf(),
                    source: io::Error::other(format!(
                        "unexpected startup probe response: {response:?}"
                    )),
                });
            }
            Ok(None) => continue,
            Err(RmuxError::IncompleteFrame { .. }) => continue,
            Err(_) => return Ok(None),
        }
    }
}

fn classify_connect_error(error: io::Error, pipe_name: &Path) -> Result<(), StartupError> {
    if let Some(code) = error.raw_os_error() {
        if code == ERROR_FILE_NOT_FOUND as i32 {
            return Ok(());
        }
        if code == ERROR_PIPE_BUSY as i32 {
            return Err(StartupError::PipeBusy {
                pipe_name: pipe_name.to_path_buf(),
            });
        }
        if code == ERROR_NO_DATA as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32 {
            return Ok(());
        }
        if code == ERROR_ACCESS_DENIED as i32 {
            return Err(StartupError::PipeAccessDenied {
                pipe_name: pipe_name.to_path_buf(),
            });
        }
    }

    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Ok(()),
        io::ErrorKind::TimedOut => Err(StartupError::PipeBusy {
            pipe_name: pipe_name.to_path_buf(),
        }),
        io::ErrorKind::PermissionDenied => Err(StartupError::PipeAccessDenied {
            pipe_name: pipe_name.to_path_buf(),
        }),
        _ => Err(StartupError::PipeIo {
            operation: "open named pipe",
            pipe_name: pipe_name.to_path_buf(),
            source: error,
        }),
    }
}

fn classify_io_error(
    error: io::Error,
    operation: &'static str,
    pipe_name: &Path,
) -> Result<(), StartupError> {
    if let Some(code) = error.raw_os_error() {
        if code == ERROR_BROKEN_PIPE as i32
            || code == ERROR_PIPE_NOT_CONNECTED as i32
            || code == ERROR_NO_DATA as i32
            || code == ERROR_FILE_NOT_FOUND as i32
        {
            return Ok(());
        }
        if code == ERROR_ACCESS_DENIED as i32 {
            return Err(StartupError::PipeAccessDenied {
                pipe_name: pipe_name.to_path_buf(),
            });
        }
    }
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotFound
            | io::ErrorKind::TimedOut
            | io::ErrorKind::UnexpectedEof
    ) {
        return Ok(());
    }

    Err(StartupError::PipeIo {
        operation,
        pipe_name: pipe_name.to_path_buf(),
        source: error,
    })
}

pub(super) async fn wait_for_daemon(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<BlockingLocalStream, StartupError> {
    const MIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

    let effective_poll = poll_interval.max(MIN_POLL_INTERVAL);

    loop {
        match probe_responsive(endpoint, pipe_name).await {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => {}
            Err(StartupError::PipeBusy { .. }) => {
                // Pipe instances momentarily exhausted while the daemon comes
                // up; treat as transient and keep polling within budget.
            }
            Err(error) => return Err(error),
        }

        if deadline.is_elapsed() {
            return Err(StartupError::StartupTimeout {
                pipe_name: pipe_name.to_path_buf(),
                waited: deadline.elapsed(),
            });
        }
        sleep(deadline.sleep_for(effective_poll)).await;
    }
}

pub(super) fn wait_for_daemon_blocking(
    endpoint: &LocalEndpoint,
    pipe_name: &Path,
    deadline: StartupDeadline,
    poll_interval: Duration,
) -> Result<BlockingLocalStream, StartupError> {
    const MIN_POLL_INTERVAL: Duration = Duration::from_millis(1);

    let effective_poll = poll_interval.max(MIN_POLL_INTERVAL);

    loop {
        match probe_blocking(endpoint, pipe_name) {
            Ok(Some(stream)) => return Ok(stream),
            Ok(None) => {}
            Err(StartupError::PipeBusy { .. }) => {
                // Pipe instances can be momentarily exhausted while the
                // daemon is binding; keep polling within the same deadline.
            }
            Err(error) => return Err(error),
        }

        if deadline.is_elapsed() {
            return Err(StartupError::StartupTimeout {
                pipe_name: pipe_name.to_path_buf(),
                waited: deadline.elapsed(),
            });
        }
        std::thread::sleep(deadline.sleep_for(effective_poll));
    }
}
