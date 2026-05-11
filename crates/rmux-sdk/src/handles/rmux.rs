//! Opaque RMUX SDK facade handle.

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::builder::RmuxBuilder;
#[cfg(windows)]
use crate::bootstrap::deadline::StartupDeadline;
use crate::diagnostics::FEATURE_PROTOCOL_CAPABILITIES;
#[cfg(windows)]
use crate::diagnostics::FEATURE_TRANSPORT_UNIX_SOCKET;
#[cfg(unix)]
use crate::diagnostics::FEATURE_TRANSPORT_WINDOWS_PIPE;
use crate::transport::{DropGuard, TransportClient};
use crate::{
    bootstrap::discovery, ensure::EnsureSession, handles::Session, Pane, PaneRef, Result,
    RmuxEndpoint, RmuxError, SessionName, Window, WindowRef,
};
use rmux_proto::{
    HandshakeRequest, KillServerRequest, Request, Response, CAPABILITY_DAEMON_SHUTDOWN,
    CAPABILITY_HANDSHAKE, RMUX_WIRE_VERSION,
};
#[cfg(windows)]
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_NOT_CONNECTED};

const INTERNAL_DAEMON_FLAG: &str = "--__internal-daemon";
#[cfg(windows)]
const WINDOWS_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// Inert SDK facade for daemon-backed RMUX operations.
///
/// Constructing this handle only records endpoint configuration and does not
/// contact a daemon.
pub struct Rmux {
    endpoint: RmuxEndpoint,
    default_timeout: Option<Duration>,
    transport: Option<TransportClient>,
    drop_guard: DropGuard,
}

impl Rmux {
    /// Creates a facade configured to use default endpoint discovery.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a builder configured to use default endpoint discovery.
    #[must_use]
    pub fn builder() -> RmuxBuilder {
        RmuxBuilder::new()
    }

    /// Connects to a running daemon at `endpoint`.
    ///
    /// Passing [`RmuxEndpoint::Default`] resolves the platform default through
    /// SDK discovery. This method never starts a daemon.
    pub async fn connect(endpoint: RmuxEndpoint) -> Result<Self> {
        RmuxBuilder::new().endpoint(endpoint).connect().await
    }

    /// Connects to the default daemon, starting it if no daemon is reachable.
    ///
    /// The hidden daemon binary is resolved from
    /// [`crate::bootstrap::discovery::SDK_DAEMON_BINARY_ENV`] when set,
    /// otherwise `rmux` is resolved through the host `PATH`. Startup races are
    /// serialized by the platform bootstrap layer.
    pub async fn connect_or_start() -> Result<Self> {
        RmuxBuilder::new().connect_or_start().await
    }

    /// Connects to `endpoint`, starting a hidden daemon if no daemon is
    /// reachable there.
    ///
    /// This is the explicit-endpoint form of [`Self::connect_or_start`].
    pub async fn connect_or_start_at(endpoint: RmuxEndpoint) -> Result<Self> {
        RmuxBuilder::new()
            .endpoint(endpoint)
            .connect_or_start()
            .await
    }

    /// Returns the endpoint selection recorded by this facade.
    #[must_use]
    pub fn endpoint(&self) -> &RmuxEndpoint {
        &self.endpoint
    }

    /// Returns the operation timeout default recorded by this facade.
    #[must_use]
    pub const fn configured_default_timeout(&self) -> Option<Duration> {
        self.default_timeout
    }

    /// Resolves the endpoint that would be used by runtime SDK operations.
    ///
    /// This consults SDK discovery only when the recorded endpoint is
    /// [`RmuxEndpoint::Default`].
    pub fn resolved_endpoint(&self) -> Result<RmuxEndpoint> {
        discovery::resolve_endpoint(&self.endpoint)
    }

    /// Resolves the timeout that would be used by one runtime SDK operation.
    ///
    /// `per_operation_timeout` has precedence over this facade's configured
    /// default and can use `Duration::MAX` to request no timeout.
    #[must_use]
    pub fn resolved_timeout(&self, per_operation_timeout: Option<Duration>) -> Option<Duration> {
        discovery::resolve_timeout(per_operation_timeout, self.default_timeout)
    }

    /// Ensures a daemon session from a session builder.
    pub async fn ensure_session(&self, ensure: EnsureSession) -> Result<Session> {
        ensure.ensure(self).await
    }

    /// Returns a handle for an existing daemon session.
    pub async fn session(&self, session_name: SessionName) -> Result<Session> {
        self.ensure_session(EnsureSession::named(session_name).reuse_only())
            .await
    }

    /// Returns a daemon-backed handle for an exact window slot.
    ///
    /// Creating the handle connects to the configured daemon endpoint but
    /// does not require the window slot to exist yet. Operations on the
    /// returned handle observe the live daemon state for that session/index,
    /// including linked-window and grouped-session updates.
    pub async fn window(&self, target: WindowRef) -> Result<Window> {
        let endpoint = self.resolved_endpoint()?;
        let timeout = self.resolved_timeout(None);
        let transport = self
            .connect_resolved_transport_for_operation(&endpoint, timeout)
            .await?;
        Ok(Window::new(
            target,
            endpoint,
            self.configured_default_timeout(),
            transport,
        ))
    }

    /// Returns a daemon-backed handle for an exact pane slot.
    ///
    /// Creating the handle connects to the configured daemon endpoint but
    /// does not require the pane slot to exist yet. Operations on the
    /// returned handle resolve `(session, window, pane)` against live daemon
    /// state on every call, so linked windows and grouped sessions report
    /// the same stable pane identity through every sibling view.
    pub async fn pane(&self, target: PaneRef) -> Result<Pane> {
        let endpoint = self.resolved_endpoint()?;
        let timeout = self.resolved_timeout(None);
        let transport = self
            .connect_resolved_transport_for_operation(&endpoint, timeout)
            .await?;
        Ok(Pane::new(
            target,
            endpoint,
            self.configured_default_timeout(),
            transport,
        ))
    }

    /// Checks the live daemon for an exact session name.
    pub async fn has_session(&self, session_name: SessionName) -> Result<bool> {
        let client = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        super::session::has_session(&client, session_name).await
    }

    /// Lists exact session names currently reported by the daemon.
    pub async fn list_sessions(&self) -> Result<Vec<SessionName>> {
        let client = self
            .connect_transport_for_operation(self.resolved_timeout(None))
            .await?;
        super::session::list_session_names(&client).await
    }

    /// Negotiates daemon capabilities, requests daemon shutdown, and waits for
    /// the SDK transport to close.
    ///
    /// This method contacts the configured daemon endpoint. Transport and
    /// protocol errors are returned to the caller; dropping an [`Rmux`] handle
    /// remains cleanup-only and never waits for daemon shutdown.
    pub async fn shutdown(mut self) -> Result<()> {
        self.drop_guard.disarm();
        let client = match self.transport.take() {
            Some(client) => client,
            None => self.connect_transport().await?,
        };

        negotiate_shutdown_capability(&client).await?;
        let response = client
            .request(Request::KillServer(KillServerRequest))
            .await?;
        match response {
            Response::KillServer(_) => {
                if let Err(error) = client.shutdown().await {
                    if !is_clean_shutdown_close(&error) {
                        return Err(error);
                    }
                }
                Ok(())
            }
            Response::Error(error) => Err(error.into()),
            response => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
                "rmux daemon sent `{}` response for shutdown request",
                response.command_name()
            )))),
        }
    }

    pub(crate) fn from_config(endpoint: RmuxEndpoint, default_timeout: Option<Duration>) -> Self {
        Self {
            endpoint,
            default_timeout,
            transport: None,
            drop_guard: DropGuard::noop(),
        }
    }

    pub(crate) fn from_connected_transport(
        endpoint: RmuxEndpoint,
        default_timeout: Option<Duration>,
        transport: TransportClient,
    ) -> Self {
        Self {
            endpoint,
            default_timeout,
            transport: Some(transport),
            drop_guard: DropGuard::noop(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_transport_for_test(
        client: TransportClient,
        drop_request: Option<Request>,
    ) -> Self {
        let drop_guard = drop_request
            .map(|request| DropGuard::best_effort(client.clone(), request))
            .unwrap_or_else(DropGuard::noop);
        Self {
            endpoint: RmuxEndpoint::Default,
            default_timeout: None,
            transport: Some(client),
            drop_guard,
        }
    }

    async fn connect_transport(&self) -> Result<TransportClient> {
        let endpoint = self.resolved_endpoint()?;
        connect_transport(&endpoint, self.resolved_timeout(None)).await
    }

    pub(crate) async fn connect_transport_for_operation(
        &self,
        timeout: Option<Duration>,
    ) -> Result<TransportClient> {
        if let Some(client) = self.transport.as_ref() {
            return Ok(client.clone());
        }

        let endpoint = self.resolved_endpoint()?;
        connect_transport(&endpoint, timeout).await
    }

    pub(crate) async fn connect_resolved_transport_for_operation(
        &self,
        endpoint: &RmuxEndpoint,
        timeout: Option<Duration>,
    ) -> Result<TransportClient> {
        if let Some(client) = self.transport.as_ref() {
            return Ok(client.clone());
        }

        connect_transport(endpoint, timeout).await
    }
}

fn is_clean_shutdown_close(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::Transport { source, .. }
            if matches!(
                source.kind(),
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            )
    )
}

impl Default for Rmux {
    fn default() -> Self {
        RmuxBuilder::default().build()
    }
}

impl fmt::Debug for Rmux {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Rmux").finish_non_exhaustive()
    }
}

async fn negotiate_shutdown_capability(client: &TransportClient) -> Result<()> {
    let response = client
        .request(Request::Handshake(HandshakeRequest::requiring([
            CAPABILITY_HANDSHAKE,
            CAPABILITY_DAEMON_SHUTDOWN,
        ])))
        .await
        .map_err(normalize_handshake_error)?;

    match response {
        Response::Handshake(response) => {
            ensure_selected_wire_version(response.wire_version)?;
            ensure_capability(&response.capabilities, CAPABILITY_HANDSHAKE)?;
            ensure_capability(&response.capabilities, CAPABILITY_DAEMON_SHUTDOWN)
        }
        Response::Error(error) => Err(error.into()),
        response => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon sent `{}` response for capability handshake",
            response.command_name()
        )))),
    }
}

fn normalize_handshake_error(error: RmuxError) -> RmuxError {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Decode(message),
        } => unsupported_handshake_error(&message),
        RmuxError::Unsupported { feature, .. }
            if feature == crate::diagnostics::command_feature_id("handshake") =>
        {
            unsupported_handshake_error("daemon did not recognize the handshake request")
        }
        error => error,
    }
}

fn unsupported_handshake_error(detail: &str) -> RmuxError {
    RmuxError::unsupported(
        FEATURE_PROTOCOL_CAPABILITIES,
        format!(
            "upgrade the rmux daemon to one that advertises `{CAPABILITY_HANDSHAKE}` before using SDK daemon shutdown; {detail}"
        ),
    )
}

fn ensure_selected_wire_version(wire_version: u32) -> Result<()> {
    if wire_version == RMUX_WIRE_VERSION {
        return Ok(());
    }

    Err(RmuxError::protocol(
        rmux_proto::RmuxError::UnsupportedWireVersion {
            got: wire_version,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        },
    ))
}

fn ensure_capability(capabilities: &[String], feature: &str) -> Result<()> {
    if capabilities
        .iter()
        .any(|capability| capability.as_str() == feature)
    {
        return Ok(());
    }

    Err(RmuxError::protocol(
        rmux_proto::RmuxError::UnsupportedCapability {
            feature: feature.to_owned(),
            supported: capabilities.to_vec(),
        },
    ))
}

#[cfg(unix)]
async fn connect_transport(
    endpoint: &RmuxEndpoint,
    timeout: Option<Duration>,
) -> Result<TransportClient> {
    match endpoint {
        RmuxEndpoint::UnixSocket(path) => {
            let stream = timeout_io("connect to rmux daemon", timeout, async {
                tokio::net::UnixStream::connect(path).await
            })
            .await?;
            Ok(TransportClient::spawn(stream))
        }
        RmuxEndpoint::WindowsPipe(_) => Err(RmuxError::unsupported(
            FEATURE_TRANSPORT_WINDOWS_PIPE,
            "use a Unix socket endpoint on Unix SDK builds",
        )),
        RmuxEndpoint::Default => Err(RmuxError::transport(
            "resolve rmux SDK endpoint",
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "default endpoint was not resolved before connecting",
            ),
        )),
    }
}

pub(crate) async fn connect_transport_to_endpoint(
    endpoint: &RmuxEndpoint,
    timeout: Option<Duration>,
) -> Result<TransportClient> {
    connect_transport(endpoint, timeout).await
}

pub(crate) async fn connect_or_start_transport(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    connect_or_start_transport_for_platform(endpoint, default_timeout).await
}

#[cfg(unix)]
async fn connect_or_start_transport_for_platform(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    let timeout = startup_operation_timeout(default_timeout);
    let RmuxEndpoint::UnixSocket(socket_path) = endpoint else {
        return connect_transport(endpoint, timeout).await;
    };
    let socket_path = socket_path.clone();
    let outcome = crate::bootstrap::startup_unix::connect_or_start_with_timeout(
        &socket_path,
        || {
            let socket_path = socket_path.clone();
            async move { spawn_hidden_daemon(socket_path.as_os_str()) }
        },
        timeout,
        crate::bootstrap::startup_unix::STARTUP_POLL_INTERVAL,
    )
    .await
    .map_err(startup_error)?;
    Ok(TransportClient::spawn(outcome.into_stream()))
}

#[cfg(windows)]
async fn connect_or_start_transport_for_platform(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    let timeout = startup_operation_timeout(default_timeout);
    let RmuxEndpoint::WindowsPipe(pipe) = endpoint else {
        return connect_transport(endpoint, timeout).await;
    };
    let pipe_path = std::path::PathBuf::from(pipe);
    let outcome = crate::bootstrap::startup_windows::connect_or_start_with_timeout(
        &pipe_path,
        || {
            let pipe_path = pipe_path.clone();
            async move { spawn_hidden_daemon(pipe_path.as_os_str()) }
        },
        timeout,
        crate::bootstrap::startup_windows::STARTUP_POLL_INTERVAL,
    )
    .await
    .map_err(startup_error)?;
    // Windows startup probes use a blocking client stream owned by a private
    // Tokio runtime. The SDK transport actor must own an async pipe client on
    // the caller's runtime, so reconnect here with the same configured retry
    // budget instead of using a raw one-shot open.
    drop(outcome);
    connect_transport(endpoint, timeout).await
}

#[cfg(not(any(unix, windows)))]
async fn connect_or_start_transport_for_platform(
    endpoint: &RmuxEndpoint,
    default_timeout: Option<Duration>,
) -> Result<TransportClient> {
    connect_transport(endpoint, startup_operation_timeout(default_timeout)).await
}

fn startup_operation_timeout(default_timeout: Option<Duration>) -> Option<Duration> {
    discovery::resolve_timeout(None, default_timeout)
}

fn spawn_hidden_daemon(endpoint: &OsStr) -> io::Result<()> {
    let child = Command::new(daemon_binary())
        .arg(INTERNAL_DAEMON_FLAG)
        .arg(endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    drop(child);
    Ok(())
}

fn daemon_binary() -> std::ffi::OsString {
    std::env::var_os(discovery::SDK_DAEMON_BINARY_ENV).unwrap_or_else(|| "rmux".into())
}

fn startup_error(error: impl fmt::Display) -> RmuxError {
    RmuxError::transport(
        "connect or start rmux daemon",
        io::Error::other(error.to_string()),
    )
}

#[cfg(windows)]
async fn connect_transport(
    endpoint: &RmuxEndpoint,
    timeout: Option<Duration>,
) -> Result<TransportClient> {
    match endpoint {
        RmuxEndpoint::WindowsPipe(pipe) => {
            let stream = connect_windows_pipe(pipe, timeout).await?;
            Ok(TransportClient::spawn(stream))
        }
        RmuxEndpoint::UnixSocket(_) => Err(RmuxError::unsupported(
            FEATURE_TRANSPORT_UNIX_SOCKET,
            "use a Windows named-pipe endpoint on Windows SDK builds",
        )),
        RmuxEndpoint::Default => Err(RmuxError::transport(
            "resolve rmux SDK endpoint",
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "default endpoint was not resolved before connecting",
            ),
        )),
    }
}

#[cfg(windows)]
async fn connect_windows_pipe(pipe: &str, timeout: Option<Duration>) -> Result<NamedPipeClient> {
    let deadline = StartupDeadline::from_timeout(timeout);
    loop {
        match ClientOptions::new().open(std::path::Path::new(pipe)) {
            Ok(stream) => return Ok(stream),
            Err(error) if windows_pipe_connect_retryable(&error) => {
                if deadline.is_elapsed() {
                    return Err(RmuxError::transport(
                        "connect to rmux daemon",
                        timeout_error(
                            "connect to rmux daemon",
                            deadline.requested_timeout().unwrap_or(Duration::MAX),
                        ),
                    ));
                }
                tokio::time::sleep(deadline.sleep_for(WINDOWS_CONNECT_RETRY_INTERVAL)).await;
            }
            Err(error) => return Err(RmuxError::transport("connect to rmux daemon", error)),
        }
    }
}

#[cfg(windows)]
fn windows_pipe_connect_retryable(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_PIPE_BUSY as i32
                || code == ERROR_PIPE_NOT_CONNECTED as i32
                || code == ERROR_NO_DATA as i32
    )
}

#[cfg(not(any(unix, windows)))]
async fn connect_transport(
    _endpoint: &RmuxEndpoint,
    _timeout: Option<Duration>,
) -> Result<TransportClient> {
    Err(RmuxError::unsupported(
        "transport.local_ipc",
        "this target does not support rmux local IPC transports",
    ))
}

#[cfg(unix)]
async fn timeout_io<F, T>(
    operation: &'static str,
    timeout: Option<Duration>,
    future: F,
) -> Result<T>
where
    F: std::future::Future<Output = io::Result<T>>,
{
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| RmuxError::transport(operation, timeout_error(operation, timeout)))?
            .map_err(|error| RmuxError::transport(operation, error)),
        None => future
            .await
            .map_err(|error| RmuxError::transport(operation, error)),
    }
}

#[cfg(any(unix, windows))]
fn timeout_error(operation: &str, timeout: Duration) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "timed out after {}s while {operation}",
            timeout.as_secs_f32()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        encode_frame, ErrorResponse, FrameDecoder, HandshakeResponse, HasSessionRequest,
        HasSessionResponse, KillServerResponse, SessionName,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn alpha() -> SessionName {
        SessionName::new("alpha").expect("valid session")
    }

    fn cleanup_request() -> Request {
        Request::HasSession(HasSessionRequest { target: alpha() })
    }

    #[test]
    fn startup_timeout_honors_builder_default_and_unbounded_sentinel() {
        assert_eq!(
            startup_operation_timeout(Some(Duration::from_millis(123))),
            Some(Duration::from_millis(123))
        );
        assert_eq!(startup_operation_timeout(Some(Duration::MAX)), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_retry_policy_covers_transient_startup_errors() {
        let busy = io::Error::from_raw_os_error(ERROR_PIPE_BUSY as i32);
        let not_connected = io::Error::from_raw_os_error(ERROR_PIPE_NOT_CONNECTED as i32);
        let no_data = io::Error::from_raw_os_error(ERROR_NO_DATA as i32);
        let not_found = io::Error::new(io::ErrorKind::NotFound, "pipe absent");

        assert!(windows_pipe_connect_retryable(&busy));
        assert!(windows_pipe_connect_retryable(&not_connected));
        assert!(windows_pipe_connect_retryable(&no_data));
        assert!(!windows_pipe_connect_retryable(&not_found));
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

            let read = stream.read(&mut buffer).await.expect("read request bytes");
            assert_ne!(read, 0, "client closed before request arrived");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        stream.write_all(&frame).await.expect("write response");
        stream.flush().await.expect("flush response");
    }

    #[tokio::test]
    async fn shutdown_negotiates_capabilities_then_waits_for_transport_close() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client_stream), None);
        let shutdown = tokio::spawn(rmux.shutdown());

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server_stream,
            Response::Handshake(HandshakeResponse::current()),
        )
        .await;

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::KillServer(KillServerRequest)
        ));
        write_response(&mut server_stream, Response::KillServer(KillServerResponse)).await;
        drop(server_stream);

        shutdown
            .await
            .expect("shutdown task")
            .expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn shutdown_propagates_daemon_transport_errors() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client_stream), None);
        let shutdown = tokio::spawn(rmux.shutdown());

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server_stream,
            Response::Handshake(HandshakeResponse::current()),
        )
        .await;
        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::KillServer(KillServerRequest)
        ));
        drop(server_stream);

        match shutdown
            .await
            .expect("shutdown task")
            .expect_err("must fail")
        {
            RmuxError::Transport { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::UnexpectedEof);
            }
            error => panic!("expected transport error, got {error:?}"),
        }
    }

    #[tokio::test]
    async fn shutdown_maps_missing_daemon_shutdown_capability_to_unsupported() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client_stream), None);
        let shutdown = tokio::spawn(rmux.shutdown());

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server_stream,
            Response::Handshake(HandshakeResponse {
                wire_version: RMUX_WIRE_VERSION,
                capabilities: vec![CAPABILITY_HANDSHAKE.to_owned()],
            }),
        )
        .await;

        match shutdown
            .await
            .expect("shutdown task")
            .expect_err("must fail")
        {
            RmuxError::Unsupported { feature, .. } => {
                assert_eq!(feature, CAPABILITY_DAEMON_SHUTDOWN);
            }
            error => panic!("expected unsupported error, got {error:?}"),
        }
    }

    #[tokio::test]
    async fn shutdown_disarms_drop_guard_without_sending_cleanup_request() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(client_stream);
        let rmux = Rmux::from_transport_for_test(client, Some(cleanup_request()));
        let shutdown = tokio::spawn(rmux.shutdown());

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server_stream,
            Response::Handshake(HandshakeResponse::current()),
        )
        .await;

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::KillServer(KillServerRequest)
        ));
        write_response(&mut server_stream, Response::KillServer(KillServerResponse)).await;
        drop(server_stream);

        shutdown
            .await
            .expect("shutdown task")
            .expect("shutdown succeeds");
    }

    #[tokio::test]
    async fn drop_guard_cleanup_is_nonblocking_best_effort() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(client_stream);
        let rmux = Rmux::from_transport_for_test(client.clone(), Some(cleanup_request()));

        drop(rmux);

        assert_eq!(read_request(&mut server_stream).await, cleanup_request());
        write_response(
            &mut server_stream,
            Response::HasSession(HasSessionResponse { exists: true }),
        )
        .await;
    }

    #[tokio::test]
    async fn drop_guard_does_not_panic_when_transport_is_closed() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let client = TransportClient::spawn(client_stream);
        drop(server_stream);

        let _ = client.request(cleanup_request()).await;
        let rmux = Rmux::from_transport_for_test(client, Some(cleanup_request()));
        drop(rmux);
    }

    #[tokio::test]
    async fn shutdown_maps_error_response_through_protocol_diagnostics() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client_stream), None);
        let shutdown = tokio::spawn(rmux.shutdown());

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server_stream,
            Response::Error(ErrorResponse {
                error: rmux_proto::RmuxError::UnsupportedCapability {
                    feature: "capability.future".to_owned(),
                    supported: vec![CAPABILITY_HANDSHAKE.to_owned()],
                },
            }),
        )
        .await;

        match shutdown
            .await
            .expect("shutdown task")
            .expect_err("must fail")
        {
            RmuxError::Unsupported { feature, .. } => {
                assert_eq!(feature, "capability.future");
            }
            error => panic!("expected unsupported error, got {error:?}"),
        }
    }

    #[tokio::test]
    async fn shutdown_maps_handshake_decode_error_to_stable_unsupported_feature() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client_stream), None);
        let shutdown = tokio::spawn(rmux.shutdown());

        assert!(matches!(
            read_request(&mut server_stream).await,
            Request::Handshake(_)
        ));
        write_response(
            &mut server_stream,
            Response::Error(ErrorResponse {
                error: rmux_proto::RmuxError::Decode(
                    "unknown variant index 93 for Request".to_owned(),
                ),
            }),
        )
        .await;

        match shutdown
            .await
            .expect("shutdown task")
            .expect_err("must fail")
        {
            RmuxError::Unsupported { feature, .. } => {
                assert_eq!(feature, FEATURE_PROTOCOL_CAPABILITIES);
            }
            error => panic!("expected unsupported error, got {error:?}"),
        }
    }
}
