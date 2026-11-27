//! Unix `connect_or_start`: join or start the daemon, then wait for its
//! readiness, joining again while a joined daemon is still exiting.
use std::future::Future;
use std::io;
use std::path::Path;

use rmux_bootstrap::startup_unix::{
    connect_or_start_with_timeout, StartupOutcome, DEFAULT_STARTUP_DEADLINE, STARTUP_POLL_INTERVAL,
};
use tokio::time::Instant;

use super::{startup_config, startup_error};
use crate::transport::{OperationDeadline, TransportClient};
use crate::{Result, RmuxError};

/// Whether the transport reached a daemon that already served the endpoint or
/// one this client launched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DaemonProvenance {
    JoinedExisting,
    Started,
}

pub(super) struct StartupTransport {
    pub(super) provenance: DaemonProvenance,
    pub(super) transport: TransportClient,
}

/// Joins the daemon serving `socket_path`, or starts one when none does.
pub(super) async fn join_or_start(
    socket_path: &Path,
    deadline: OperationDeadline,
    launcher_cwd: Option<&Path>,
) -> Result<StartupTransport> {
    let mut pending_daemon = None;
    let outcome = connect_or_start_with_timeout(
        socket_path,
        || {
            let pending_daemon = &mut pending_daemon;
            async move {
                *pending_daemon = Some(startup_config::spawn_hidden_daemon(
                    socket_path.as_os_str(),
                    launcher_cwd,
                    deadline,
                )?);
                Ok(())
            }
        },
        deadline.remaining_timeout(),
        STARTUP_POLL_INTERVAL,
    )
    .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(daemon) = pending_daemon.as_mut() {
                daemon.stop().map_err(|source| {
                    RmuxError::transport("stop failed rmux daemon startup", source)
                })?;
            }
            return Err(startup_error(error));
        }
    };
    if let Some(daemon) = pending_daemon {
        // A published daemon may already serve other callers. Its lifetime is
        // independent of this SDK client, including later config/RPC errors.
        drop(daemon.detach());
    }
    let provenance = match outcome {
        StartupOutcome::JoinedExisting(_) => DaemonProvenance::JoinedExisting,
        StartupOutcome::Started(_) => DaemonProvenance::Started,
    };
    let transport = TransportClient::spawn(outcome.into_stream()).with_operation_deadline(deadline);
    Ok(StartupTransport {
        provenance,
        transport,
    })
}

/// Joins or starts a daemon through `attempt` until one answers readiness.
///
/// Killing the last session makes an `exit-empty` daemon stop accepting while
/// its endpoint stays bound through the shutdown drain, so the kernel still
/// queues a new connection that is then reset, or closed, without an answer.
/// Readiness is a read-only `daemon-status`, so a joined daemon that ends it
/// that way is joined again: the next attempt meets either the next daemon or
/// an absent endpoint to start one on. A daemon this client started is never
/// retried. Rejoining stops at the operation deadline and, like the CLI, after
/// the bootstrap's default startup deadline.
pub(super) async fn connect_ready<A, F>(
    deadline: OperationDeadline,
    mut attempt: A,
) -> Result<TransportClient>
where
    A: FnMut() -> F,
    F: Future<Output = Result<StartupTransport>>,
{
    let rejoin_until = Instant::now() + DEFAULT_STARTUP_DEADLINE;
    loop {
        let StartupTransport {
            provenance,
            transport,
        } = attempt().await?;
        let error = match startup_config::wait_until_loaded(&transport, deadline).await {
            Ok(()) => return Ok(transport),
            Err(error) => error,
        };
        let remaining = deadline.remaining_timeout();
        let may_rejoin = provenance == DaemonProvenance::JoinedExisting
            && closed_unanswered(&error)
            && Instant::now() < rejoin_until
            && remaining.is_none_or(|remaining| !remaining.is_zero());
        if !may_rejoin {
            return Err(error);
        }
        drop(transport);
        let pause = remaining.map_or(STARTUP_POLL_INTERVAL, |remaining| {
            remaining.min(STARTUP_POLL_INTERVAL)
        });
        tokio::time::sleep(pause).await;
    }
}

fn closed_unanswered(error: &RmuxError) -> bool {
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

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::Duration;

    use rmux_proto::{encode_frame, DaemonStatusResponse, FrameDecoder, Request, Response};

    use super::{connect_ready, DaemonProvenance, StartupTransport};
    use crate::transport::{OperationDeadline, TransportClient};
    use crate::{RmuxEndpoint, RmuxError};

    /// Lets the joined connection queue on the exiting daemon first.
    const EXIT_DELAY: Duration = Duration::from_millis(100);

    struct PrivateDirectory(PathBuf);

    impl PrivateDirectory {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rmux-sdk-us-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).expect("create private test directory");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("make test directory owner-only");
            Self(path)
        }
    }

    impl Drop for PrivateDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn answer_daemon_status(listener: &UnixListener) {
        let (mut stream, _) = listener.accept().expect("accept rejoined readiness");
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 256];
        loop {
            if let Some(request) = decoder.next_frame::<Request>().expect("request decodes") {
                assert!(matches!(request, Request::DaemonStatus(_)));
                break;
            }
            let read = stream.read(&mut buffer).expect("read readiness");
            assert_ne!(read, 0, "client closed before readiness arrived");
            decoder.push_bytes(&buffer[..read]);
        }
        let response = Response::DaemonStatus(DaemonStatusResponse {
            rmux_version: "0.11.0".to_owned(),
            wire_version: rmux_proto::RMUX_WIRE_VERSION,
            session_count: 0,
            client_count: 0,
            config_loading: false,
        });
        let frame = encode_frame(&response).expect("encode readiness");
        stream.write_all(&frame).expect("answer readiness");
    }

    /// S1983: killing the last session makes an exit-empty daemon stop
    /// accepting while its endpoint stays bound through the shutdown drain.
    /// `connect_or_start` joined it there and failed on the reset or EOF of
    /// its readiness request instead of waiting for the endpoint to serve.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connect_or_start_joins_again_when_the_joined_daemon_is_exiting() {
        let directory = PrivateDirectory::new("exiting");
        let socket = directory.0.join("s");
        let exiting = UnixListener::bind(&socket).expect("exiting daemon socket");
        let next_path = directory.0.join("s.next");
        let next = UnixListener::bind(&next_path).expect("next daemon socket");
        let handover = {
            let socket = socket.clone();
            std::thread::spawn(move || {
                std::thread::sleep(EXIT_DELAY);
                // The next generation owns the path before the exiting one
                // drops its never-accepted connection, so no attempt finds an
                // absent endpoint and launches a real daemon.
                std::fs::rename(&next_path, &socket).expect("publish next daemon");
                drop(exiting);
                answer_daemon_status(&next);
            })
        };

        let connected = super::super::connect_or_start_transport(
            &RmuxEndpoint::UnixSocket(socket.clone()),
            Some(Duration::from_secs(5)),
        )
        .await
        // Fail before joining: the next daemon waits for a rejoin that an
        // unfixed client never makes.
        .expect("the next daemon generation answers readiness");
        handover.join().expect("handover thread");
        assert_eq!(connected.endpoint, RmuxEndpoint::UnixSocket(socket));
    }

    fn closed_transport() -> TransportClient {
        let (client, daemon) = tokio::io::duplex(1024);
        drop(daemon);
        TransportClient::spawn(client)
    }

    fn assert_closed_unanswered(error: &RmuxError) {
        match error {
            RmuxError::Transport { source, .. } => assert!(
                matches!(
                    source.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ),
                "unexpected transport failure: {source}"
            ),
            error => panic!("expected a closed transport, got {error:?}"),
        }
    }

    /// Only a joined daemon is retried: one this client started and that
    /// closes readiness unanswered failed to start, which must surface.
    #[tokio::test]
    async fn a_started_daemon_that_closes_readiness_unanswered_is_not_retried() {
        let mut attempts = 0;
        let error = connect_ready(
            OperationDeadline::from_timeout(Some(Duration::from_secs(5))),
            || {
                attempts += 1;
                async {
                    Ok(StartupTransport {
                        provenance: DaemonProvenance::Started,
                        transport: closed_transport(),
                    })
                }
            },
        )
        .await
        .expect_err("a started daemon's failed readiness surfaces");
        assert_closed_unanswered(&error);
        assert_eq!(attempts, 1);
    }

    /// An endpoint that keeps closing readiness unanswered is not joined
    /// past the operation deadline.
    #[tokio::test]
    async fn rejoining_stops_at_the_operation_deadline() {
        let deadline = Duration::from_millis(200);
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let error = connect_ready(OperationDeadline::from_timeout(Some(deadline)), || {
            attempts += 1;
            async {
                Ok(StartupTransport {
                    provenance: DaemonProvenance::JoinedExisting,
                    transport: closed_transport(),
                })
            }
        })
        .await
        .expect_err("a daemon that never answers surfaces");
        // The last attempt meets the closed transport or the elapsed deadline.
        assert!(
            matches!(error, RmuxError::Transport { .. }),
            "unexpected error: {error:?}"
        );
        assert!(attempts > 1, "a joined daemon is joined again");
        assert!(
            started.elapsed() < deadline + Duration::from_secs(2),
            "rejoining outlived the operation deadline: {:?}",
            started.elapsed()
        );
    }
}
