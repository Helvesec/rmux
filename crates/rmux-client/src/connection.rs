//! Blocking Unix-socket transport for detached RPC traffic.

use std::ffi::OsStr;
#[cfg(all(test, unix))]
use std::ffi::OsString;
#[cfg(all(test, unix))]
use std::fs;
use std::io::{self, Read, Write};
#[cfg(all(test, unix))]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ClientError;
use rmux_ipc::{connect_blocking, BlockingLocalStream, LocalEndpoint};
use rmux_proto::{
    encode_frame, AttachSessionResponse, ControlMode, ControlModeResponse, FrameDecoder,
    HandshakeRequest, Request, Response, RmuxError,
};

/// Read buffer size for blocking socket reads.
const READ_BUFFER_SIZE: usize = 8192;
/// Default timeout for establishing detached RPC connections.
const SOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Default timeout for writing detached RPC requests.
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default timeout for ordinary detached RPC response reads.
const SOCKET_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(all(test, unix))]
const FALLBACK_SOCKET_ROOT: &str = "/tmp";
#[cfg(all(test, unix))]
const SOCKET_DIR_PREFIX: &str = "rmux";

/// Computes the default RMUX client socket path.
///
/// The path uses an rmux-specific per-user directory so an rmux client never
/// speaks the rmux wire protocol to a real tmux server.
pub fn default_socket_path() -> Result<PathBuf, ClientError> {
    rmux_ipc::default_endpoint()
        .map(LocalEndpoint::into_path)
        .map_err(ClientError::Io)
}

/// Computes an rmux socket path for a top-level `-L` socket name.
pub fn socket_path_for_label(label: impl AsRef<OsStr>) -> Result<PathBuf, ClientError> {
    rmux_ipc::endpoint_for_label(label)
        .map(LocalEndpoint::into_path)
        .map_err(ClientError::Io)
}

/// Resolves the top-level socket path from `-L`, `-S`, `$RMUX`, or defaults.
///
/// `-S` wins over `-L`; both command-line forms win over `$RMUX`.
pub fn resolve_socket_path(
    socket_name: Option<&OsStr>,
    socket_path: Option<&Path>,
) -> Result<PathBuf, ClientError> {
    rmux_ipc::resolve_endpoint(socket_name, socket_path)
        .map(LocalEndpoint::into_path)
        .map_err(ClientError::Io)
}

/// Result of attempting to connect to the RMUX server.
// Keep the successful path as an owned public `Connection`; boxing would add an
// unnecessary API wrinkle around the small absent-server control-flow case.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum ConnectResult {
    /// Successfully connected to the server.
    Connected(Connection),
    /// The server is absent (socket does not exist or connection refused).
    Absent,
}

/// Attempts to connect to the RMUX server, distinguishing absent servers from
/// real connection errors.
///
/// Returns [`ConnectResult::Absent`] when the socket does not exist or the
/// connection is refused, which lets callers like `kill-session` succeed with
/// exit code `0` for an absent server. Returns an error only for unexpected
/// transport failures.
pub fn connect_or_absent(socket_path: &Path) -> Result<ConnectResult, ClientError> {
    connect_or_absent_with_timeout_using(
        socket_path,
        SOCKET_CONNECT_TIMEOUT,
        connect_stream_with_timeout,
    )
}

/// Connects to the RMUX server, returning an error if the server is absent.
pub fn connect(socket_path: &Path) -> Result<Connection, ClientError> {
    connect_with_timeout_using(
        socket_path,
        SOCKET_CONNECT_TIMEOUT,
        connect_stream_with_timeout,
    )
}

/// A blocking connection to the RMUX server that exchanges typed frames.
#[derive(Debug)]
pub struct Connection {
    stream: BlockingLocalStream,
    decoder: FrameDecoder,
    handshake_capabilities: Option<Vec<String>>,
}

/// The explicit result of requesting an attach-stream upgrade.
#[derive(Debug)]
pub enum AttachTransition {
    /// The server accepted the attach request and switched protocols.
    Upgraded(AttachSessionUpgrade),
    /// The server responded without switching protocols.
    Rejected(Response),
}

/// The explicit result of requesting a control-mode upgrade.
#[derive(Debug)]
pub enum ControlTransition {
    /// The server accepted the control-mode request and switched protocols.
    Upgraded(ControlModeUpgrade),
    /// The server responded without switching protocols.
    Rejected(Response),
}

/// A detached connection that has transitioned into attach-stream mode.
#[derive(Debug)]
pub struct AttachSessionUpgrade {
    response: AttachSessionResponse,
    stream: BlockingLocalStream,
    initial_bytes: Vec<u8>,
}

/// A detached connection that has transitioned into control-mode streaming.
#[derive(Debug)]
pub struct ControlModeUpgrade {
    pub(crate) response: ControlModeResponse,
    pub(crate) stream: BlockingLocalStream,
}

impl AttachSessionUpgrade {
    /// Returns the upgrade response sent by the server.
    #[must_use]
    pub const fn response(&self) -> &AttachSessionResponse {
        &self.response
    }

    /// Consumes the upgrade and returns the raw attach-stream socket.
    #[must_use]
    pub fn into_stream(self) -> BlockingLocalStream {
        self.stream
    }

    /// Consumes the upgrade and returns the raw attach-stream socket plus any
    /// bytes already read beyond the detached response frame.
    #[must_use]
    pub fn into_parts(self) -> (BlockingLocalStream, Vec<u8>) {
        (self.stream, self.initial_bytes)
    }
}

impl ControlModeUpgrade {
    /// Returns the upgrade response sent by the server.
    #[must_use]
    pub const fn response(&self) -> &ControlModeResponse {
        &self.response
    }

    /// Returns the negotiated control-mode flavor.
    #[must_use]
    pub const fn mode(&self) -> ControlMode {
        self.response.mode
    }

    /// Consumes the upgrade and returns the raw control-mode socket.
    #[must_use]
    pub fn into_stream(self) -> BlockingLocalStream {
        self.stream
    }
}

impl Connection {
    pub(crate) fn new(stream: BlockingLocalStream) -> Result<Self, ClientError> {
        set_read_timeout(&stream, Some(SOCKET_RESPONSE_TIMEOUT)).map_err(ClientError::Io)?;
        set_write_timeout(&stream, Some(SOCKET_WRITE_TIMEOUT)).map_err(ClientError::Io)?;

        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            handshake_capabilities: None,
        })
    }

    /// Sends a request and reads the server's response.
    ///
    /// Server-side `Response::Error` payloads are returned as-is in the `Ok`
    /// variant so callers can pattern-match on them. Only transport and framing
    /// failures produce `Err`.
    pub fn roundtrip(&mut self, request: &Request) -> Result<Response, ClientError> {
        self.write_request(request)?;
        self.read_response()
    }

    /// Returns whether the connected daemon advertises a protocol capability.
    ///
    /// Optional client behavior uses this as a soft gate: older daemons that do
    /// not answer the handshake shape or report an error are treated as not
    /// supporting the capability, leaving the connection usable for legacy
    /// requests.
    pub fn supports_capability(&mut self, capability: &str) -> Result<bool, ClientError> {
        if let Some(capabilities) = &self.handshake_capabilities {
            return Ok(capabilities.iter().any(|supported| supported == capability));
        }

        match self.roundtrip(&Request::Handshake(HandshakeRequest::current()))? {
            Response::Handshake(response) => {
                self.handshake_capabilities = Some(response.capabilities);
                Ok(self
                    .handshake_capabilities
                    .as_ref()
                    .expect("handshake capabilities were just cached")
                    .iter()
                    .any(|supported| supported == capability))
            }
            Response::Error(error) => {
                if matches!(&error.error, RmuxError::UnsupportedWireVersion { .. }) {
                    return Err(ClientError::Protocol(error.error));
                }
                self.handshake_capabilities = Some(Vec::new());
                Ok(false)
            }
            _ => {
                self.handshake_capabilities = Some(Vec::new());
                Ok(false)
            }
        }
    }

    /// Sends a request without a detached response read timeout.
    ///
    /// This is reserved for scripting requests whose server-side completion can
    /// legitimately block beyond the normal five-second detached RPC bound.
    pub(crate) fn roundtrip_without_read_timeout(
        &mut self,
        request: &Request,
    ) -> Result<Response, ClientError> {
        let previous_timeout = read_timeout(&self.stream).map_err(ClientError::Io)?;
        set_read_timeout(&self.stream, None).map_err(ClientError::Io)?;
        let result = self.roundtrip(request);
        set_read_timeout(&self.stream, previous_timeout).map_err(ClientError::Io)?;
        result
    }

    pub(crate) fn write_request(&mut self, request: &Request) -> Result<(), ClientError> {
        let frame = encode_frame(request).map_err(ClientError::Protocol)?;
        self.stream.write_all(&frame).map_err(ClientError::Io)
    }

    pub(crate) fn read_response(&mut self) -> Result<Response, ClientError> {
        let mut buffer = [0u8; READ_BUFFER_SIZE];

        loop {
            match self.decoder.next_frame::<Response>() {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(error) => return Err(ClientError::Protocol(error)),
            }

            let bytes_read = match self.stream.read(&mut buffer) {
                Ok(bytes_read) => bytes_read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(ClientError::Io(error)),
            };

            if bytes_read == 0 {
                return Err(ClientError::UnexpectedEof);
            }

            self.decoder.push_bytes(&buffer[..bytes_read]);
        }
    }

    pub(crate) fn stream_mut(&mut self) -> &mut BlockingLocalStream {
        &mut self.stream
    }

    pub(crate) fn into_attach_upgrade(
        self,
        response: AttachSessionResponse,
    ) -> Result<AttachSessionUpgrade, ClientError> {
        set_read_timeout(&self.stream, None).map_err(ClientError::Io)?;
        set_write_timeout(&self.stream, None).map_err(ClientError::Io)?;
        let initial_bytes = self.decoder.remaining_bytes().to_vec();

        Ok(AttachSessionUpgrade {
            response,
            stream: self.stream,
            initial_bytes,
        })
    }

    pub(crate) fn into_control_upgrade(
        self,
        response: ControlModeResponse,
    ) -> Result<ControlModeUpgrade, ClientError> {
        set_read_timeout(&self.stream, None).map_err(ClientError::Io)?;
        set_write_timeout(&self.stream, None).map_err(ClientError::Io)?;

        Ok(ControlModeUpgrade {
            response,
            stream: self.stream,
        })
    }
}

pub(crate) fn read_response_frame_exact(
    stream: &mut BlockingLocalStream,
) -> Result<Response, ClientError> {
    let mut decoder = FrameDecoder::new();
    let mut byte = [0_u8; 1];

    loop {
        match decoder.next_frame::<Response>() {
            Ok(Some(response)) => return Ok(response),
            Ok(None) => {}
            Err(error) => return Err(ClientError::Protocol(error)),
        }

        read_exact_or_eof(stream, &mut byte)?;
        decoder.push_bytes(&byte);
    }
}

fn read_exact_or_eof(
    stream: &mut BlockingLocalStream,
    buffer: &mut [u8],
) -> Result<(), ClientError> {
    match stream.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(ClientError::UnexpectedEof)
        }
        Err(error) => Err(ClientError::Io(error)),
    }
}

#[cfg(all(test, unix))]
fn socket_path_from_parts(
    rmux_tmpdir: Option<&OsStr>,
    user_id: u32,
    label: &OsStr,
) -> io::Result<PathBuf> {
    let root = socket_root_from_parts(rmux_tmpdir)?;
    let base = root.join(format!("{SOCKET_DIR_PREFIX}-{user_id}"));
    let mut path = base.into_os_string().into_vec();
    path.push(b'/');
    path.extend_from_slice(label.as_bytes());

    Ok(PathBuf::from(OsString::from_vec(path)))
}

#[cfg(all(test, unix))]
fn socket_root_from_parts(rmux_tmpdir: Option<&OsStr>) -> io::Result<PathBuf> {
    let rmux_tmpdir = rmux_tmpdir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let candidates = rmux_tmpdir
        .into_iter()
        .chain(std::iter::once(PathBuf::from(FALLBACK_SOCKET_ROOT)));

    for candidate in candidates {
        if let Ok(resolved) = fs::canonicalize(&candidate) {
            return Ok(resolved);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no suitable rmux socket directory",
    ))
}

fn connect_or_absent_with_timeout_using<F>(
    socket_path: &Path,
    timeout: Duration,
    connect_stream: F,
) -> Result<ConnectResult, ClientError>
where
    F: FnOnce(&Path, Duration) -> io::Result<BlockingLocalStream>,
{
    match connect_stream(socket_path, timeout) {
        Ok(stream) => Ok(ConnectResult::Connected(Connection::new(stream)?)),
        Err(error) if is_absent_error(&error) => Ok(ConnectResult::Absent),
        Err(error) => Err(ClientError::Io(error)),
    }
}

fn connect_with_timeout_using<F>(
    socket_path: &Path,
    timeout: Duration,
    connect_stream: F,
) -> Result<Connection, ClientError>
where
    F: FnOnce(&Path, Duration) -> io::Result<BlockingLocalStream>,
{
    let stream = connect_stream(socket_path, timeout).map_err(ClientError::Io)?;
    Connection::new(stream)
}

fn connect_stream_with_timeout(
    socket_path: &Path,
    timeout: Duration,
) -> io::Result<BlockingLocalStream> {
    connect_blocking(
        &LocalEndpoint::from_path(socket_path.to_path_buf()),
        timeout,
    )
}

fn read_timeout(stream: &BlockingLocalStream) -> io::Result<Option<Duration>> {
    stream.read_timeout()
}

fn set_read_timeout(stream: &BlockingLocalStream, timeout: Option<Duration>) -> io::Result<()> {
    stream.set_read_timeout(timeout)
}

fn set_write_timeout(stream: &BlockingLocalStream, timeout: Option<Duration>) -> io::Result<()> {
    stream.set_write_timeout(timeout)
}

/// Returns `true` for I/O errors that indicate the server is not running.
fn is_absent_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

#[cfg(all(test, unix))]
mod tests {
    include!("connection/tests.rs");
}
