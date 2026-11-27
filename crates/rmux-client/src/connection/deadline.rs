use std::io;
use std::path::Path;

use rmux_bootstrap::deadline::StartupDeadline;

use super::frame_io::{expired_deadline_error, RpcTransfer};
use super::{
    connect_stream_with_timeout, connect_with_timeout_using, set_read_timeout, set_write_timeout,
    Connection, SOCKET_CONNECT_TIMEOUT,
};
use crate::ClientError;

/// Connects with one deadline shared by connection setup and subsequent RPC I/O.
///
/// The deadline is checked on every read and write, including partial frames.
/// Ordinary connections retain their existing per-RPC timeout policy.
pub fn connect_with_deadline(
    path: &Path,
    deadline: StartupDeadline,
) -> Result<Connection, ClientError> {
    let timeout = remaining(deadline)?.map_or(SOCKET_CONNECT_TIMEOUT, |left| {
        left.min(SOCKET_CONNECT_TIMEOUT)
    });
    let mut connection = connect_with_timeout_using(path, timeout, connect_stream_with_timeout)?;
    connection.operation_deadline = Some(deadline);
    Ok(connection)
}

impl Connection {
    pub(super) fn apply_read_deadline(
        &self,
        read_deadline: Option<StartupDeadline>,
    ) -> Result<(), ClientError> {
        if let Some(deadline) = self.operation_deadline {
            let operation_left = remaining(deadline)?;
            let read_left = read_deadline.map(remaining).transpose()?.flatten();
            let timeout = match (operation_left, read_left) {
                (Some(operation), Some(read)) => Some(operation.min(read)),
                (operation, read) => operation.or(read),
            };
            set_read_timeout(&self.stream, timeout).map_err(ClientError::Io)?;
        }
        Ok(())
    }

    pub(super) fn apply_write_deadline(&self) -> Result<(), ClientError> {
        if let Some(deadline) = self.operation_deadline {
            set_write_timeout(&self.stream, remaining(deadline)?).map_err(ClientError::Io)?;
        }
        Ok(())
    }

    pub(super) fn operation_io_error(
        &self,
        error: io::Error,
        transfer: RpcTransfer,
    ) -> ClientError {
        if self.operation_deadline.is_some()
            && matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
        {
            return timed_out();
        }
        ClientError::Io(expired_deadline_error(&self.stream, error, transfer))
    }
}

fn remaining(deadline: StartupDeadline) -> Result<Option<std::time::Duration>, ClientError> {
    match deadline.remaining_timeout() {
        Some(left) if left.is_zero() => Err(timed_out()),
        remaining => Ok(remaining),
    }
}

fn timed_out() -> ClientError {
    ClientError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        "automation operation timed out",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::{Duration, Instant};

    #[test]
    fn explicit_read_timeout_remains_shorter_than_the_operation_budget() {
        let path =
            std::env::temp_dir().join(format!("rmux-read-budget-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let mut connection = connect_with_deadline(
            &path,
            StartupDeadline::from_timeout(Some(Duration::from_secs(1))),
        )
        .unwrap();
        let (_peer, _) = listener.accept().unwrap();
        let start = Instant::now();
        let error = connection
            .read_response_with_read_timeout(Duration::from_millis(20))
            .unwrap_err();
        assert!(matches!(error, ClientError::Io(error) if error.kind() == io::ErrorKind::TimedOut));
        assert!(start.elapsed() < Duration::from_millis(250));
        assert!(
            connection
                .operation_deadline
                .unwrap()
                .remaining_timeout()
                .unwrap()
                > Duration::from_millis(500)
        );
        std::fs::remove_file(path).unwrap();
    }
}
