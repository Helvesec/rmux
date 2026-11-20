//! Blocking Unix-socket transport for detached RPC traffic.

use std::ffi::OsStr;
#[cfg(all(test, unix))]
use std::ffi::OsString;
#[cfg(all(test, unix))]
use std::fs;
use std::io::{self, Read};
#[cfg(all(test, unix))]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod capabilities;

use crate::ClientError;
use rmux_bootstrap::deadline::StartupDeadline;

mod deadline;
mod frame_io;
pub use deadline::connect_with_deadline;
use rmux_ipc::{connect_blocking, BlockingLocalStream, LocalEndpoint};
use rmux_proto::{
    encode_frame, AttachSessionResponse, ControlMode, ControlModeResponse, FrameDecoder, Request,
    Response, RmuxError, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION,
};

/// Read buffer size for blocking socket reads.
const READ_BUFFER_SIZE: usize = 8192;
/// Default timeout for establishing detached RPC connections.
const SOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Default timeout for writing detached RPC requests.
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// Default timeout for ordinary detached RPC response reads.
const SOCKET_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
/// Old detached wire versions kept only for targeted daemon shutdown recovery.
const LEGACY_SHUTDOWN_MIN_WIRE_VERSION: u32 = 1;
const LEGACY_SHUTDOWN_MAX_WIRE_VERSION: u32 = RMUX_WIRE_VERSION - 1;

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

/// Resolves the top-level socket path from `-L`, `-S`, inherited multiplexer
/// environment, or defaults.
///
/// `-S` wins over `-L`; both command-line forms win over inherited
/// multiplexer environment.
pub fn resolve_socket_path(
    socket_name: Option<&OsStr>,
    socket_path: Option<&Path>,
) -> Result<PathBuf, ClientError> {
    rmux_ipc::resolve_endpoint(socket_name, socket_path)
        .map(LocalEndpoint::into_path)
        .map_err(ClientError::Io)
}

/// Resolves a socket path for a tmux-compatible shim invocation.
///
/// This path may consume `$TMUX`; the native RMUX path intentionally does not.
pub fn resolve_tmux_compatible_socket_path(
    socket_name: Option<&OsStr>,
    socket_path: Option<&Path>,
) -> Result<PathBuf, ClientError> {
    rmux_ipc::resolve_tmux_compatible_endpoint(socket_name, socket_path)
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
    connect_or_absent_with_timeout(socket_path, SOCKET_CONNECT_TIMEOUT)
}

/// Attempts to connect to the RMUX server within a caller-provided timeout,
/// distinguishing an absent server from other transport failures.
///
/// This is intended for bounded lifecycle probes. Ordinary detached clients
/// should use [`connect_or_absent`], which applies the standard connect timeout.
pub(crate) fn connect_or_absent_with_timeout(
    socket_path: &Path,
    timeout: Duration,
) -> Result<ConnectResult, ClientError> {
    connect_or_absent_with_timeout_using(socket_path, timeout, connect_stream_with_timeout)
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
    operation_deadline: Option<StartupDeadline>,
}

/// The explicit result of requesting an attach-stream upgrade.
// Keep the public enum shape stable: callers match `Rejected(Response)` directly.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum AttachTransition {
    /// The server accepted the attach request and switched protocols.
    Upgraded(AttachSessionUpgrade),
    /// The server responded without switching protocols.
    Rejected(Response),
}

/// The explicit result of requesting a control-mode upgrade.
// Keep the public enum shape stable: callers match `Rejected(Response)` directly.
#[allow(clippy::large_enum_variant)]
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
            operation_deadline: None,
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

    /// Sends a request without a detached response read timeout.
    ///
    /// This is reserved for scripting requests whose server-side completion can
    /// legitimately block beyond the normal five-second detached RPC bound.
    pub(crate) fn roundtrip_without_read_timeout(
        &mut self,
        request: &Request,
    ) -> Result<Response, ClientError> {
        self.with_read_timeout(None, |connection| connection.roundtrip(request))
    }

    /// Reads the next detached response without a response read timeout.
    ///
    /// This is used for already-armed long-running requests where another
    /// connection may cancel the server-side wait on timeout.
    pub fn read_response_without_read_timeout(&mut self) -> Result<Response, ClientError> {
        self.with_read_timeout(None, Self::read_response)
    }

    /// Reads the next detached response with a caller-provided read timeout.
    pub fn read_response_with_read_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Response, ClientError> {
        self.with_read_timeout(Some(timeout), |connection| {
            connection.read_response_with_limit(Some(timeout))
        })
    }

    /// Runs `operation` under `timeout`, then restores the previous read
    /// timeout. An operation error takes precedence over a restore error.
    fn with_read_timeout<T>(
        &mut self,
        timeout: Option<Duration>,
        operation: impl FnOnce(&mut Self) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        let previous_timeout = read_timeout(&self.stream).map_err(ClientError::Io)?;
        set_read_timeout(&self.stream, timeout).map_err(ClientError::Io)?;
        let result = operation(self);
        let restore_result =
            set_read_timeout(&self.stream, previous_timeout).map_err(ClientError::Io);
        result.and_then(|value| restore_result.map(|()| value))
    }

    pub(crate) fn stream_mut(&mut self) -> &mut BlockingLocalStream {
        &mut self.stream
    }

    pub(crate) fn into_attach_upgrade(
        self,
        response: AttachSessionResponse,
    ) -> Result<AttachSessionUpgrade, ClientError> {
        clear_upgraded_stream_timeouts(&self.stream).map_err(ClientError::Io)?;
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
        clear_upgraded_stream_timeouts(&self.stream).map_err(ClientError::Io)?;

        Ok(ControlModeUpgrade {
            response,
            stream: self.stream,
        })
    }
}

/// Removes the detached RPC deadlines from a stream that becomes a long-lived
/// attach or control-mode stream.
fn clear_upgraded_stream_timeouts(stream: &BlockingLocalStream) -> io::Result<()> {
    set_read_timeout(stream, None)?;
    set_write_timeout(stream, None)
}

fn encode_legacy_wire_frame(request: &Request, wire_version: u32) -> Result<Vec<u8>, ClientError> {
    if !(LEGACY_SHUTDOWN_MIN_WIRE_VERSION..=LEGACY_SHUTDOWN_MAX_WIRE_VERSION)
        .contains(&wire_version)
    {
        return Err(ClientError::Protocol(RmuxError::UnsupportedWireVersion {
            got: wire_version,
            minimum: LEGACY_SHUTDOWN_MIN_WIRE_VERSION,
            maximum: LEGACY_SHUTDOWN_MAX_WIRE_VERSION,
        }));
    }

    let mut frame = encode_frame(request).map_err(ClientError::Protocol)?;
    if frame.first().copied() != Some(RMUX_FRAME_MAGIC) {
        return Err(ClientError::Protocol(RmuxError::Encode(
            "current frame encoder produced an invalid RMUX envelope".to_owned(),
        )));
    }

    if RMUX_WIRE_VERSION > 0x7f || wire_version > 0x7f {
        return Err(ClientError::Protocol(RmuxError::Encode(
            "legacy shutdown recovery expects single-byte wire versions".to_owned(),
        )));
    }

    match frame.get_mut(1) {
        Some(version) if *version == RMUX_WIRE_VERSION as u8 => {
            *version = wire_version as u8;
            Ok(frame)
        }
        _ => Err(ClientError::Protocol(RmuxError::Encode(
            "current frame encoder used an unexpected wire-version envelope".to_owned(),
        ))),
    }
}

/// Maps a request write failure on a detached RPC stream, naming an expired
/// write deadline as a timeout.
pub(crate) fn request_write_error(stream: &BlockingLocalStream, error: io::Error) -> ClientError {
    ClientError::Io(frame_io::expired_deadline_error(
        stream,
        error,
        frame_io::RpcTransfer::Request,
    ))
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
        Err(error) => Err(ClientError::Io(frame_io::expired_deadline_error(
            stream,
            error,
            frame_io::RpcTransfer::Response,
        ))),
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

/// Sets the read deadline of a detached RPC stream.
///
/// Every read and write deadline change in this module goes through
/// [`set_read_timeout`] or [`set_write_timeout`] so the Darwin disconnect rule
/// below holds on every path.
fn set_read_timeout(stream: &BlockingLocalStream, timeout: Option<Duration>) -> io::Result<()> {
    ignore_timeout_change_after_peer_disconnect(stream.set_read_timeout(timeout))
}

/// Sets the write deadline of a detached RPC stream; see [`set_read_timeout`].
fn set_write_timeout(stream: &BlockingLocalStream, timeout: Option<Duration>) -> io::Result<()> {
    ignore_timeout_change_after_peer_disconnect(stream.set_write_timeout(timeout))
}

/// Accepts a deadline change that Darwin refused because the peer is gone.
///
/// The server may send a complete response or upgrade output and close the
/// socket before the client changes a deadline. XNU's setsockopt then fails
/// with EINVAL, because the socket can neither send nor receive any more. Such
/// a socket can no longer block, so the deadline is moot: buffered bytes stay
/// readable and the next read or write reports the disconnect normally.
fn ignore_timeout_change_after_peer_disconnect(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if timeout_change_rejected_after_peer_disconnect(&error) => Ok(()),
        result => result,
    }
}

/// Matches only the kernel's EINVAL: the standard library reports invalid
/// durations, such as zero, as `InvalidInput` without an OS error code.
#[cfg(target_os = "macos")]
fn timeout_change_rejected_after_peer_disconnect(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EINVAL)
}

#[cfg(not(target_os = "macos"))]
fn timeout_change_rejected_after_peer_disconnect(_error: &io::Error) -> bool {
    false
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
