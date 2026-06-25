use std::borrow::Cow;
use std::path::Path;

use rmux_proto::{
    DaemonStatusResponse, Response, RmuxError, CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE,
    CAPABILITY_DAEMON_STATUS, RMUX_WIRE_VERSION,
};

use crate::shell_quote::shell_quote_path;
use crate::{ClientError, Connection};

const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
#[cfg(debug_assertions)]
const CLIENT_VERSION_OVERRIDE_ENV: &str = "RMUX_INTERNAL_CLIENT_VERSION";
#[cfg(debug_assertions)]
const INTERNAL_TEST_OPT_IN_ENV: &str = "RMUX_ALLOW_INTERNAL_BINARY_OVERRIDE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonFreshness {
    Current,
    StaleIdle(StaleDaemon),
    StaleActive(StaleDaemon),
    Incompatible(IncompatibleDaemon),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StaleDaemon {
    pub(crate) daemon_version: Option<String>,
    pub(crate) session_count: usize,
    pub(crate) client_count: usize,
    supports_shutdown_if_idle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncompatibleDaemon {
    pub(crate) daemon_version: Option<String>,
    pub(crate) daemon_wire_version: Option<u32>,
}

enum CapabilitySupport {
    Supported(bool),
    Incompatible(IncompatibleDaemon),
}

impl StaleDaemon {
    #[must_use]
    pub(crate) const fn supports_shutdown_if_idle(&self) -> bool {
        self.supports_shutdown_if_idle
    }
}

pub(crate) fn inspect_daemon(connection: &mut Connection) -> Result<DaemonFreshness, ClientError> {
    match supports_capability_or_incompatible(connection, CAPABILITY_DAEMON_STATUS)? {
        CapabilitySupport::Supported(true) => return inspect_current_daemon(connection),
        CapabilitySupport::Supported(false) => {}
        CapabilitySupport::Incompatible(incompatible) => {
            return Ok(DaemonFreshness::Incompatible(incompatible));
        }
    }

    let session_count = legacy_session_count(connection)?;
    Ok(DaemonFreshness::StaleActive(StaleDaemon {
        daemon_version: None,
        session_count,
        client_count: 0,
        supports_shutdown_if_idle: false,
    }))
}

pub(crate) fn daemon_status_matches_current_client(
    status: &DaemonStatusResponse,
) -> Result<bool, IncompatibleDaemon> {
    if status.wire_version != RMUX_WIRE_VERSION {
        return Err(IncompatibleDaemon {
            daemon_version: Some(format!("v{}", status.rmux_version)),
            daemon_wire_version: Some(status.wire_version),
        });
    }

    Ok(status.rmux_version == client_version().as_ref())
}

pub(crate) fn request_idle_shutdown(
    connection: &mut Connection,
    stale: &StaleDaemon,
) -> Result<bool, ClientError> {
    if stale.supports_shutdown_if_idle() {
        return match connection.shutdown_if_idle()? {
            Response::ShutdownIfIdle(response) => Ok(response.shutdown),
            Response::Error(_) => Ok(false),
            other => Err(unexpected_response("shutdown-if-idle", &other)),
        };
    }

    Ok(false)
}

pub(crate) fn warn_stale_active_daemon(stale: &StaleDaemon, socket_path: &Path) {
    let daemon = stale
        .daemon_version
        .as_deref()
        .unwrap_or("an older release");
    let client_version = client_version();
    eprintln!("rmux: daemon is {daemon}, client is v{client_version}.");
    eprintln!("rmux: Existing sessions or clients are still running on the old daemon.");
    eprintln!(
        "rmux: Run `{}` when you are ready to restart them with v{client_version}.",
        kill_server_command(socket_path)
    );
}

fn inspect_current_daemon(connection: &mut Connection) -> Result<DaemonFreshness, ClientError> {
    let supports_shutdown_if_idle = match supports_capability_or_incompatible(
        connection,
        CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE,
    )? {
        CapabilitySupport::Supported(supported) => supported,
        CapabilitySupport::Incompatible(incompatible) => {
            return Ok(DaemonFreshness::Incompatible(incompatible));
        }
    };
    match connection.daemon_status()? {
        Response::DaemonStatus(status) => match daemon_status_matches_current_client(&status) {
            Err(incompatible) => Ok(DaemonFreshness::Incompatible(incompatible)),
            Ok(true) => Ok(DaemonFreshness::Current),
            Ok(false) => Ok(classify_stale_daemon(StaleDaemon {
                daemon_version: Some(format!("v{}", status.rmux_version)),
                session_count: status.session_count,
                client_count: status.client_count,
                supports_shutdown_if_idle,
            })),
        },
        Response::Error(_) => {
            let session_count = legacy_session_count(connection)?;
            Ok(DaemonFreshness::StaleActive(StaleDaemon {
                daemon_version: None,
                session_count,
                client_count: 0,
                supports_shutdown_if_idle: false,
            }))
        }
        other => Err(unexpected_response("daemon-status", &other)),
    }
}

fn supports_capability_or_incompatible(
    connection: &mut Connection,
    capability: &str,
) -> Result<CapabilitySupport, ClientError> {
    match connection.supports_capability(capability) {
        Ok(supported) => Ok(CapabilitySupport::Supported(supported)),
        Err(error) if is_unsupported_wire_version(&error) => {
            Ok(CapabilitySupport::Incompatible(IncompatibleDaemon {
                daemon_version: None,
                daemon_wire_version: None,
            }))
        }
        Err(error) => Err(error),
    }
}

fn classify_stale_daemon(stale: StaleDaemon) -> DaemonFreshness {
    if stale.session_count == 0 && stale.client_count == 0 {
        DaemonFreshness::StaleIdle(stale)
    } else {
        DaemonFreshness::StaleActive(stale)
    }
}

fn legacy_session_count(connection: &mut Connection) -> Result<usize, ClientError> {
    match connection.display_message(None, true, Some("#{server_sessions}".to_owned()))? {
        Response::DisplayMessage(response) => response
            .output
            .as_ref()
            .and_then(|output| std::str::from_utf8(output.stdout()).ok())
            .and_then(|text| text.trim().parse::<usize>().ok())
            .ok_or_else(|| {
                ClientError::Protocol(rmux_proto::RmuxError::Server(
                    "daemon did not report server_sessions".to_owned(),
                ))
            }),
        Response::Error(error) => Err(ClientError::Protocol(error.error)),
        other => Err(unexpected_response("display-message", &other)),
    }
}

fn unexpected_response(command: &str, response: &Response) -> ClientError {
    ClientError::Protocol(rmux_proto::RmuxError::Server(format!(
        "unexpected {command} response: {response:?}"
    )))
}

pub(crate) fn incompatible_daemon_message(incompatible: &IncompatibleDaemon) -> String {
    let daemon = incompatible
        .daemon_version
        .as_deref()
        .unwrap_or("from an older release");
    match incompatible.daemon_wire_version {
        Some(wire_version) => format!(
            "running daemon {daemon} uses incompatible protocol {wire_version}; this client supports protocol {RMUX_WIRE_VERSION}"
        ),
        None => format!(
            "running daemon {daemon} uses an incompatible protocol; this client supports protocol {RMUX_WIRE_VERSION}"
        ),
    }
}

fn client_version() -> Cow<'static, str> {
    #[cfg(debug_assertions)]
    {
        if std::env::var_os(INTERNAL_TEST_OPT_IN_ENV).is_some_and(|value| value == "1") {
            if let Some(version) = std::env::var_os(CLIENT_VERSION_OVERRIDE_ENV)
                .and_then(|value| value.into_string().ok())
                .filter(|value| !value.is_empty())
            {
                return Cow::Owned(version);
            }
        }
    }

    Cow::Borrowed(CLIENT_VERSION)
}

fn is_unsupported_wire_version(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Protocol(RmuxError::UnsupportedWireVersion { .. })
    )
}

fn kill_server_command(socket_path: &Path) -> String {
    format!("rmux -S {} kill-server", shell_quote_path(socket_path))
}

#[cfg(test)]
mod tests {
    use super::{
        classify_stale_daemon, daemon_status_matches_current_client, DaemonFreshness, StaleDaemon,
    };
    #[cfg(unix)]
    use super::{inspect_daemon, request_idle_shutdown};

    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    #[cfg(unix)]
    use std::thread;

    #[cfg(unix)]
    use rmux_proto::{
        encode_frame, CommandOutput, DaemonStatusRequest, DisplayMessageRequest,
        DisplayMessageResponse, ErrorResponse, FrameDecoder, HandshakeRequest, HandshakeResponse,
        Request, Response, RmuxError, CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE, CAPABILITY_DAEMON_STATUS,
    };
    use rmux_proto::{DaemonStatusResponse, RMUX_WIRE_VERSION};

    #[cfg(unix)]
    use crate::Connection;

    #[test]
    fn stale_daemon_without_sessions_or_clients_is_idle() {
        let stale = StaleDaemon {
            daemon_version: Some("v0.5.0+stale".to_owned()),
            session_count: 0,
            client_count: 0,
            supports_shutdown_if_idle: true,
        };

        assert_eq!(
            classify_stale_daemon(stale.clone()),
            DaemonFreshness::StaleIdle(stale)
        );
    }

    #[test]
    fn stale_daemon_with_sessions_stays_active() {
        let stale = StaleDaemon {
            daemon_version: Some("v0.5.0+stale".to_owned()),
            session_count: 1,
            client_count: 0,
            supports_shutdown_if_idle: true,
        };

        assert_eq!(
            classify_stale_daemon(stale.clone()),
            DaemonFreshness::StaleActive(stale)
        );
    }

    #[test]
    fn stale_daemon_with_clients_stays_active() {
        let stale = StaleDaemon {
            daemon_version: None,
            session_count: 0,
            client_count: 1,
            supports_shutdown_if_idle: false,
        };

        assert_eq!(
            classify_stale_daemon(stale.clone()),
            DaemonFreshness::StaleActive(stale)
        );
    }

    #[test]
    fn current_status_matches_client_version() {
        let status = daemon_status(env!("CARGO_PKG_VERSION"), RMUX_WIRE_VERSION);

        assert!(daemon_status_matches_current_client(&status).expect("wire version is compatible"));
    }

    #[test]
    fn stale_status_does_not_match_client_version() {
        let status = daemon_status("0.5.0+stale", RMUX_WIRE_VERSION);

        assert!(!daemon_status_matches_current_client(&status).expect("wire version is compatible"));
    }

    #[test]
    fn status_wire_mismatch_reports_incompatible_daemon() {
        let status = daemon_status("0.5.0+stale", RMUX_WIRE_VERSION + 1);
        let incompatible = daemon_status_matches_current_client(&status)
            .expect_err("wire mismatch is incompatible");

        assert_eq!(incompatible.daemon_version.as_deref(), Some("v0.5.0+stale"));
        assert_eq!(
            incompatible.daemon_wire_version,
            Some(RMUX_WIRE_VERSION + 1)
        );
    }

    #[cfg(unix)]
    #[test]
    fn idle_shutdown_without_modern_capability_is_refused_locally() {
        let stale = StaleDaemon {
            daemon_version: None,
            session_count: 0,
            client_count: 0,
            supports_shutdown_if_idle: false,
        };
        let (client, _server) = UnixStream::pair().expect("create stream pair");
        let mut connection = Connection::new(client).expect("connection with timeout");

        assert!(
            !request_idle_shutdown(&mut connection, &stale).expect("local refusal succeeds"),
            "legacy daemons must not be killed as an idle-shutdown fallback"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspect_current_daemon_returns_current_without_second_handshake() {
        let mut connection = connection_with_script(vec![
            (
                Request::Handshake(HandshakeRequest::current()),
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: vec![
                        CAPABILITY_DAEMON_STATUS.to_owned(),
                        CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE.to_owned(),
                    ],
                }),
            ),
            (
                Request::DaemonStatus(DaemonStatusRequest),
                Response::DaemonStatus(DaemonStatusResponse {
                    rmux_version: env!("CARGO_PKG_VERSION").to_owned(),
                    wire_version: RMUX_WIRE_VERSION,
                    session_count: 0,
                    client_count: 0,
                    config_loading: false,
                }),
            ),
        ]);

        assert_eq!(
            inspect_daemon(&mut connection).expect("inspect succeeds"),
            DaemonFreshness::Current
        );
    }

    #[cfg(unix)]
    #[test]
    fn inspect_modern_stale_idle_daemon_is_idle() {
        let mut connection = connection_with_status("0.5.0+stale", RMUX_WIRE_VERSION, 0, 0);

        assert!(matches!(
            inspect_daemon(&mut connection).expect("inspect succeeds"),
            DaemonFreshness::StaleIdle(stale)
                if stale.daemon_version.as_deref() == Some("v0.5.0+stale")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inspect_modern_stale_daemon_with_sessions_is_active() {
        let mut connection = connection_with_status("0.5.0+stale", RMUX_WIRE_VERSION, 1, 0);

        assert!(matches!(
            inspect_daemon(&mut connection).expect("inspect succeeds"),
            DaemonFreshness::StaleActive(stale)
                if stale.session_count == 1
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inspect_modern_wire_mismatch_is_incompatible() {
        let mut connection = connection_with_status("0.5.0+stale", RMUX_WIRE_VERSION + 1, 1, 0);

        assert!(matches!(
            inspect_daemon(&mut connection).expect("inspect succeeds"),
            DaemonFreshness::Incompatible(incompatible)
                if incompatible.daemon_version.as_deref() == Some("v0.5.0+stale")
                    && incompatible.daemon_wire_version == Some(RMUX_WIRE_VERSION + 1)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inspect_legacy_daemon_is_never_idle_even_without_sessions() {
        let mut connection = connection_with_script(vec![
            (
                Request::Handshake(HandshakeRequest::current()),
                Response::Error(ErrorResponse {
                    error: RmuxError::UnknownCommand("handshake".to_owned()),
                }),
            ),
            (
                Request::DisplayMessage(DisplayMessageRequest {
                    target: None,
                    print: true,
                    message: Some("#{server_sessions}".to_owned()),
                    empty_target_context: false,
                }),
                Response::DisplayMessage(DisplayMessageResponse::from_output(
                    CommandOutput::from_stdout(b"0\n".to_vec()),
                )),
            ),
        ]);

        assert!(matches!(
            inspect_daemon(&mut connection).expect("inspect succeeds"),
            DaemonFreshness::StaleActive(stale)
                if stale.session_count == 0 && !stale.supports_shutdown_if_idle()
        ));
    }

    #[cfg(unix)]
    fn connection_with_status(
        rmux_version: &str,
        wire_version: u32,
        session_count: usize,
        client_count: usize,
    ) -> Connection {
        connection_with_script(vec![
            (
                Request::Handshake(HandshakeRequest::current()),
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: vec![
                        CAPABILITY_DAEMON_STATUS.to_owned(),
                        CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE.to_owned(),
                    ],
                }),
            ),
            (
                Request::DaemonStatus(DaemonStatusRequest),
                Response::DaemonStatus(DaemonStatusResponse {
                    rmux_version: rmux_version.to_owned(),
                    wire_version,
                    session_count,
                    client_count,
                    config_loading: false,
                }),
            ),
        ])
    }

    fn daemon_status(rmux_version: &str, wire_version: u32) -> DaemonStatusResponse {
        DaemonStatusResponse {
            rmux_version: rmux_version.to_owned(),
            wire_version,
            session_count: 0,
            client_count: 0,
            config_loading: false,
        }
    }

    #[cfg(unix)]
    fn connection_with_script(script: Vec<(Request, Response)>) -> Connection {
        let (client, server) = UnixStream::pair().expect("create stream pair");
        thread::spawn(move || {
            let mut stream = server;
            for (expected, response) in script {
                expect_request(&mut stream, expected);
                write_response(&mut stream, response);
            }
        });
        Connection::new(client).expect("connection with timeout")
    }

    #[cfg(unix)]
    fn expect_request(stream: &mut UnixStream, expected: Request) {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 128];

        loop {
            match decoder.next_frame::<Request>() {
                Ok(Some(actual)) => {
                    assert_eq!(actual, expected);
                    return;
                }
                Ok(None) => {}
                Err(error) => panic!("invalid request frame: {error}"),
            }

            let bytes_read = stream.read(&mut buffer).expect("read request bytes");
            assert!(bytes_read > 0, "client closed before request frame arrived");
            decoder.push_bytes(&buffer[..bytes_read]);
        }
    }

    #[cfg(unix)]
    fn write_response(stream: &mut UnixStream, response: Response) {
        let frame = encode_frame(&response).expect("encode response");
        stream.write_all(&frame).expect("write response frame");
    }
}
