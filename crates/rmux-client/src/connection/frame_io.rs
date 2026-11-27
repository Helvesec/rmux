use std::io::{self, Read, Write};
use std::time::Duration;

use rmux_bootstrap::deadline::StartupDeadline;
use rmux_ipc::BlockingLocalStream;
use rmux_proto::{encode_frame, Request, Response};

use super::{encode_legacy_wire_frame, Connection, READ_BUFFER_SIZE};
use crate::ClientError;

/// Direction of a detached RPC transfer whose socket deadline may expire.
#[derive(Debug, Clone, Copy)]
pub(super) enum RpcTransfer {
    /// Reading the server response.
    Response,
    /// Writing the client request.
    Request,
}

impl RpcTransfer {
    fn armed_deadline(self, stream: &BlockingLocalStream) -> Option<Duration> {
        match self {
            Self::Response => stream.read_timeout().ok().flatten(),
            #[cfg(unix)]
            Self::Request => stream.write_timeout().ok().flatten(),
            #[cfg(not(unix))]
            Self::Request => None,
        }
    }

    const fn activity(self) -> &'static str {
        match self {
            Self::Response => "waiting for the rmux server response",
            Self::Request => "sending a request to the rmux server",
        }
    }
}

/// Reports an expired detached RPC socket deadline as a named timeout.
///
/// Every detached RPC stream is blocking, so a Unix socket fails with
/// `EAGAIN` only when the `SO_RCVTIMEO` or `SO_SNDTIMEO` deadline armed by this
/// module expires. Passed through, that reads as a resource shortage
/// ("Resource temporarily unavailable", os error 35 on Darwin). Report it as
/// `TimedOut` with the deadline, as the Windows named-pipe stream already does.
pub(super) fn expired_deadline_error(
    stream: &BlockingLocalStream,
    error: io::Error,
    transfer: RpcTransfer,
) -> io::Error {
    if error.kind() != io::ErrorKind::WouldBlock {
        return error;
    }
    let message = match transfer.armed_deadline(stream) {
        Some(deadline) => format!(
            "timed out after {}s {}",
            deadline.as_secs_f32(),
            transfer.activity()
        ),
        None => format!("timed out {}", transfer.activity()),
    };
    io::Error::new(io::ErrorKind::TimedOut, message)
}

impl Connection {
    pub(crate) fn write_request(&mut self, request: &Request) -> Result<(), ClientError> {
        let frame = encode_frame(request).map_err(ClientError::Protocol)?;
        self.write_frame(&frame)
    }

    pub(crate) fn write_legacy_wire_request(
        &mut self,
        request: &Request,
        wire_version: u32,
    ) -> Result<(), ClientError> {
        let frame = encode_legacy_wire_frame(request, wire_version)?;
        self.write_frame(&frame)
    }

    fn write_frame(&mut self, mut bytes: &[u8]) -> Result<(), ClientError> {
        if self.operation_deadline.is_none() {
            return self
                .stream
                .write_all(bytes)
                .map_err(|error| self.operation_io_error(error, RpcTransfer::Request));
        }
        while !bytes.is_empty() {
            self.apply_write_deadline()?;
            match self.stream.write(bytes) {
                Ok(0) => return Err(ClientError::Io(io::ErrorKind::WriteZero.into())),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(self.operation_io_error(error, RpcTransfer::Request)),
            }
        }
        Ok(())
    }

    pub(crate) fn read_response(&mut self) -> Result<Response, ClientError> {
        self.read_response_with_limit(None)
    }

    pub(super) fn read_response_with_limit(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> Result<Response, ClientError> {
        let read_deadline = timeout.map(|timeout| StartupDeadline::from_timeout(Some(timeout)));
        let mut buffer = [0u8; READ_BUFFER_SIZE];
        loop {
            self.apply_read_deadline(read_deadline)?;
            match self.decoder.next_frame::<Response>() {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(error) => return Err(ClientError::Protocol(error)),
            }
            let bytes_read = match self.stream.read(&mut buffer) {
                Ok(bytes_read) => bytes_read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(self.operation_io_error(error, RpcTransfer::Response)),
            };
            if bytes_read == 0 {
                return Err(ClientError::UnexpectedEof);
            }
            self.decoder.push_bytes(&buffer[..bytes_read]);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use rmux_proto::{Request, SetBufferRequest};

    use super::super::{
        read_response_frame_exact, set_read_timeout, set_write_timeout, Connection,
    };
    use crate::ClientError;

    /// Stands in for an RPC deadline that expires while the daemon is stalled.
    const EXPIRED_DEADLINE: Duration = Duration::from_millis(50);

    fn assert_named_timeout(error: ClientError, expected: &str) {
        match error {
            ClientError::Io(error) => {
                assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
                assert_eq!(error.raw_os_error(), None, "{error}");
                assert!(error.to_string().contains(expected), "{error}");
            }
            other => panic!("expected a named timeout, got {other}"),
        }
    }

    #[test]
    fn expired_response_deadline_is_a_named_timeout() {
        let (client, _silent_daemon) = UnixStream::pair().expect("stream pair");
        let mut connection = Connection::new(client).expect("connection");
        set_read_timeout(&connection.stream, Some(EXPIRED_DEADLINE)).expect("read deadline");

        let error = connection.read_response().expect_err("silent daemon");

        assert_named_timeout(
            error,
            "timed out after 0.05s waiting for the rmux server response",
        );
    }

    #[test]
    fn expired_request_write_deadline_is_a_named_timeout() {
        let (client, _unread_daemon) = UnixStream::pair().expect("stream pair");
        let mut connection = Connection::new(client).expect("connection");
        set_write_timeout(&connection.stream, Some(EXPIRED_DEADLINE)).expect("write deadline");
        let oversized = Request::SetBuffer(Box::new(SetBufferRequest {
            name: None,
            content: vec![b'x'; 4 << 20],
            append: false,
            new_name: None,
            set_clipboard: false,
            target_client: None,
        }));

        let error = connection
            .write_request(&oversized)
            .expect_err("unread daemon");

        assert_named_timeout(
            error,
            "timed out after 0.05s sending a request to the rmux server",
        );
    }

    #[test]
    fn expired_control_upgrade_response_deadline_is_a_named_timeout() {
        let (mut client, _silent_daemon) = UnixStream::pair().expect("stream pair");
        set_read_timeout(&client, Some(EXPIRED_DEADLINE)).expect("read deadline");

        let error = read_response_frame_exact(&mut client).expect_err("silent daemon");

        assert_named_timeout(
            error,
            "timed out after 0.05s waiting for the rmux server response",
        );
    }
}
