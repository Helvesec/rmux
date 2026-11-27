//! A failed bootstrap relinquishes only the daemon generation it launched.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmux_bootstrap::startup_unix::{
    connect_or_start_with, StartupError, StartupOutcome, STARTUP_POLL_INTERVAL,
};

use super::{
    auto_start_error_from_startup, hidden_daemon_command_base, spawn_hidden_daemon,
    startup_outcome_into_connection, startup_outcome_provenance, AutoStartConfig, AutoStartError,
    ServerConnectionProvenance,
};
use crate::{ClientError, Connection};

pub(super) struct StartupOwner(UnixStream);

impl StartupOwner {
    pub(super) fn release(mut self) {
        // Readiness succeeded: ordinary command/session lifecycle now owns
        // the daemon. EOF without this byte means bootstrap was abandoned.
        let _ = self.0.write_all(&[1]);
    }
}

/// Joins the daemon serving `socket_path`, or starts one from `binary_path`.
///
/// `resolve` selects the executable to spawn. Identity builds verify it there,
/// and on a loaded host that check alone can outlast the whole startup
/// deadline. It therefore runs before [`connect_or_start_with`] arms the
/// deadline, which keeps bounding what it documents: the lock, the spawn by
/// `launch`, and the daemon bind.
///
/// Only a launch needs that executable, so a first pass joins a running
/// daemon without resolving anything; a refused executable cannot keep the
/// client from a daemon that is already serving.
pub(super) fn connect_or_start<T>(
    socket_path: &Path,
    binary_path: &Path,
    resolve: impl FnOnce(&Path) -> io::Result<PathBuf>,
    launch: impl FnOnce(&Path) -> io::Result<T>,
    deadline: Duration,
) -> Result<(StartupOutcome, Option<T>), AutoStartError> {
    let startup_error = |error| auto_start_error_from_startup(error, binary_path, socket_path);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AutoStartError::Client(ClientError::Io(error)))?;
    if let Some(joined) = runtime
        .block_on(join_running_daemon(socket_path, deadline))
        .map_err(startup_error)?
    {
        return Ok((joined, None));
    }
    let verified = resolve(binary_path).map_err(|error| AutoStartError::Launch {
        path: binary_path.to_path_buf(),
        error,
    })?;
    let mut launched = None;
    let launched_slot = &mut launched;
    let outcome = runtime
        .block_on(connect_or_start_with(
            socket_path,
            move || async move {
                *launched_slot = Some(launch(&verified)?);
                Ok(())
            },
            deadline,
            STARTUP_POLL_INTERVAL,
        ))
        .map_err(startup_error)?;
    Ok((outcome, launched))
}

/// Joins or starts a daemon through `attempt`, then probes it, until one
/// answers the readiness probe.
///
/// A daemon that is exiting stops accepting but keeps its endpoint bound until
/// its shutdown drain ends, so the kernel still queues a new connection that
/// is then reset, or closed by the shut admission gate, without an answer. The
/// probe has no side effect, so a joined daemon that ends the probe that way
/// is joined again: the next attempt meets either the next daemon or an absent
/// endpoint to start one on. A daemon this client started is never retried.
pub(super) fn connect_ready<T>(
    mut attempt: impl FnMut() -> Result<(StartupOutcome, Option<T>), AutoStartError>,
    mut probe: impl FnMut(Connection) -> Result<Connection, AutoStartError>,
    rejoin_window: Duration,
) -> Result<(ServerConnectionProvenance, Connection, Option<T>), AutoStartError> {
    let rejoin_deadline = Instant::now() + rejoin_window;
    loop {
        let (outcome, startup_owner) = attempt()?;
        let provenance = startup_outcome_provenance(&outcome);
        let connection = startup_outcome_into_connection(outcome)?;
        match probe(connection) {
            Ok(connection) => return Ok((provenance, connection, startup_owner)),
            Err(AutoStartError::Client(error))
                if provenance == ServerConnectionProvenance::JoinedExisting
                    && closed_unanswered(&error)
                    && Instant::now() < rejoin_deadline =>
            {
                std::thread::sleep(STARTUP_POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn closed_unanswered(error: &ClientError) -> bool {
    match error {
        ClientError::UnexpectedEof => true,
        ClientError::Io(error) => matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
        ),
        ClientError::Protocol(_) | ClientError::Attach(_) | ClientError::ServerExited => false,
    }
}

/// Runs the bootstrap with a launcher that declines to launch: the result is
/// the daemon already serving `socket_path`, or `None` when one must start.
async fn join_running_daemon(
    socket_path: &Path,
    deadline: Duration,
) -> Result<Option<StartupOutcome>, StartupError> {
    let mut must_launch = false;
    let must_launch_slot = &mut must_launch;
    let joined = connect_or_start_with(
        socket_path,
        move || async move {
            *must_launch_slot = true;
            Err(io::Error::other("no daemon is serving; launch deferred"))
        },
        deadline,
        STARTUP_POLL_INTERVAL,
    )
    .await;
    match joined {
        Ok(outcome) => Ok(Some(outcome)),
        Err(StartupError::Launcher { .. }) if must_launch => Ok(None),
        Err(error) => Err(error),
    }
}

/// Verifies `binary_path`, then launches it as this client's startup owner.
pub(super) fn launch(
    binary_path: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> io::Result<StartupOwner> {
    let verified = rmux_os::build_identity::resolve(binary_path.as_os_str())?;
    launch_verified(&verified, socket_path, config)
}

/// Launches an executable already returned by `build_identity::resolve`.
pub(super) fn launch_verified(
    verified: &Path,
    socket_path: &Path,
    config: &AutoStartConfig,
) -> io::Result<StartupOwner> {
    let (owner, daemon) = UnixStream::pair()?;
    let mut command = hidden_daemon_command_base(verified, socket_path, config);
    command
        .arg("--startup-owner-fd")
        .arg(daemon.as_raw_fd().to_string());
    #[cfg(not(target_os = "linux"))]
    let preserved = [daemon.as_raw_fd()];
    #[cfg(target_os = "linux")]
    let mut ready = linux_ready::StartupReadyEvent::new()?;
    #[cfg(target_os = "linux")]
    let preserved = {
        ready.append_hidden_daemon_args(&mut command);
        [daemon.as_raw_fd(), ready.raw_fd()]
    };
    rmux_os::daemon::configure_hidden_daemon_command_preserving_fds(&mut command, true, &preserved);
    spawn_hidden_daemon(command)?;
    #[cfg(target_os = "linux")]
    ready.wait_for_signal(linux_ready::STARTUP_READY_EVENT_TIMEOUT);
    Ok(StartupOwner(owner))
}

/// Linux startup readiness travels over an inherited eventfd. Its descriptor,
/// daemon argument and bounded wait form one platform selection.
#[cfg(target_os = "linux")]
mod linux_ready {
    use std::fs::File;
    use std::io::{self, Read};
    use std::os::fd::AsRawFd;
    use std::process::Command;
    use std::time::{Duration, Instant};

    pub(super) const STARTUP_READY_EVENT_TIMEOUT: Duration = Duration::from_millis(20);

    pub(super) struct StartupReadyEvent {
        file: File,
    }

    impl StartupReadyEvent {
        pub(super) fn new() -> io::Result<Self> {
            let fd = rustix::event::eventfd(
                0,
                rustix::event::EventfdFlags::NONBLOCK | rustix::event::EventfdFlags::CLOEXEC,
            )
            .map_err(io::Error::from)?;
            Ok(Self { file: fd.into() })
        }

        pub(super) fn append_hidden_daemon_args(&self, command: &mut Command) {
            command
                .arg("--startup-ready-fd")
                .arg(self.file.as_raw_fd().to_string());
        }

        pub(super) fn raw_fd(&self) -> i32 {
            self.file.as_raw_fd()
        }

        pub(super) fn wait_for_signal(&mut self, timeout: Duration) {
            let deadline = Instant::now() + timeout;
            let mut bytes = [0_u8; 8];
            loop {
                match self.file.read_exact(&mut bytes) {
                    Ok(()) => return,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use rmux_bootstrap::startup_unix::StartupOutcome;
    use rmux_proto::{encode_frame, ErrorResponse, Response, RmuxError};

    use super::connect_or_start;
    use crate::auto_start::{AutoStartConfig, ServerConnectionProvenance};
    use crate::{AutoStartError, ClientError};

    const DEADLINE: Duration = Duration::from_secs(1);
    /// A spawned daemon needs some time before its socket accepts connections.
    const DAEMON_BIND_DELAY: Duration = Duration::from_millis(100);

    struct PrivateDirectory(PathBuf);

    impl PrivateDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!("rmux-so-{label}-{}", std::process::id()));
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

    fn spawn_daemon_binding_later(socket: &Path) -> JoinHandle<io::Result<UnixListener>> {
        let socket = socket.to_path_buf();
        thread::spawn(move || {
            thread::sleep(DAEMON_BIND_DELAY);
            UnixListener::bind(socket)
        })
    }

    /// T1791 and S1813: on a cold, loaded identity build, verifying the daemon
    /// executable used up the whole startup deadline before the daemon was
    /// even spawned. Verification must happen before that deadline is armed.
    #[test]
    fn executable_verification_does_not_consume_the_startup_deadline() {
        let directory = PrivateDirectory::new("verify");
        let socket = directory.0.join("s");
        let (outcome, daemon) = connect_or_start(
            &socket,
            Path::new("rmux-daemon"),
            |binary| {
                thread::sleep(DEADLINE);
                Ok(binary.to_path_buf())
            },
            |_| Ok(spawn_daemon_binding_later(&socket)),
            DEADLINE,
        )
        .expect("slow verification must not expire the daemon bind deadline");
        assert!(outcome.is_owner());
        let daemon = daemon.expect("the startup owner launched the daemon");
        daemon
            .join()
            .expect("daemon thread")
            .expect("daemon bound its socket");
    }

    /// R2983 point 4: only a launch needs the verified executable. A daemon
    /// that is already serving is joined without running the verifier, so its
    /// cost and its refusal stay off every later command.
    #[test]
    fn a_running_daemon_is_joined_without_verifying_the_executable() {
        let directory = PrivateDirectory::new("join");
        let socket = directory.0.join("s");
        let _daemon = UnixListener::bind(&socket).expect("running daemon socket");
        let mut verified = false;
        let (outcome, launched) = connect_or_start(
            &socket,
            Path::new("rmux-daemon"),
            |_| {
                verified = true;
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "candidate executable identity rejected",
                ))
            },
            |_| -> io::Result<()> { panic!("a running daemon must not be relaunched") },
            DEADLINE,
        )
        .expect("a running daemon must be joined even if its executable is refused");
        assert!(!outcome.is_owner());
        assert!(launched.is_none());
        assert!(!verified, "joining a running daemon must not verify");
    }

    /// A daemon another client starts while this one verifies is joined, not
    /// launched twice; when none appears, the verified executable is launched.
    #[test]
    fn verification_is_followed_by_a_fresh_probe_and_launches_its_result() {
        let directory = PrivateDirectory::new("race");
        let socket = directory.0.join("s");
        let mut concurrent = None;
        let (outcome, launched) = connect_or_start(
            &socket,
            Path::new("rmux-daemon"),
            |binary| {
                concurrent = Some(UnixListener::bind(&socket)?);
                Ok(binary.to_path_buf())
            },
            |_| -> io::Result<()> { panic!("the concurrent daemon must be joined") },
            DEADLINE,
        )
        .expect("join the daemon started during verification");
        assert!(!outcome.is_owner());
        assert!(launched.is_none());
        assert!(concurrent.is_some());

        let fresh = directory.0.join("t");
        let (outcome, launched) = connect_or_start(
            &fresh,
            Path::new("rmux-daemon"),
            |_| Ok(PathBuf::from("/verified/rmux-daemon")),
            |verified| {
                assert_eq!(verified, Path::new("/verified/rmux-daemon"));
                Ok(spawn_daemon_binding_later(&fresh))
            },
            DEADLINE,
        )
        .expect("launch the verified executable");
        assert!(outcome.is_owner());
        launched
            .expect("the startup owner launched the daemon")
            .join()
            .expect("daemon thread")
            .expect("daemon bound its socket");
    }

    /// The deadline still bounds the launch itself and the daemon bind.
    #[test]
    fn slow_launch_still_expires_the_startup_deadline() {
        let directory = PrivateDirectory::new("launch");
        let socket = directory.0.join("s");
        let error = connect_or_start(
            &socket,
            Path::new("rmux-daemon"),
            |binary| Ok(binary.to_path_buf()),
            |_| {
                thread::sleep(DEADLINE);
                Ok(spawn_daemon_binding_later(&socket))
            },
            DEADLINE,
        )
        .expect_err("a launch that outlasts the deadline must time out");
        assert!(
            matches!(error, AutoStartError::TimedOut { .. }),
            "unexpected error: {error}"
        );
    }

    /// S1972: killing the last session makes an exit-empty daemon stop
    /// accepting while its endpoint stays bound through the shutdown drain. A
    /// starting command joined it there and failed on the reset or EOF instead
    /// of waiting for the endpoint to serve again.
    #[test]
    fn a_joined_daemon_that_closes_the_probe_unanswered_is_joined_again() {
        let directory = PrivateDirectory::new("exiting");
        let socket = directory.0.join("s");
        let exiting = UnixListener::bind(&socket).expect("exiting daemon socket");
        let next_path = directory.0.join("s.next");
        let next = UnixListener::bind(&next_path).expect("next daemon socket");
        let handover = {
            let socket = socket.clone();
            thread::spawn(move || {
                thread::sleep(DAEMON_BIND_DELAY);
                // The next generation owns the path before the exiting one
                // drops its never-accepted connection, so no attempt finds an
                // absent endpoint and launches.
                std::fs::rename(&next_path, &socket).expect("publish next daemon");
                drop(exiting);
                let (mut stream, _) = next.accept().expect("accept rejoined probe");
                let mut request = [0_u8; 1024];
                assert!(stream.read(&mut request).expect("read probe") > 0);
                let response = Response::Error(ErrorResponse {
                    error: RmuxError::UnknownCommand("daemon-status".to_owned()),
                });
                let frame = encode_frame(&response).expect("encode probe response");
                stream.write_all(&frame).expect("answer probe");
            })
        };
        let mut attempts = 0;
        let (provenance, _connection, launched) = super::connect_ready(
            || {
                attempts += 1;
                connect_or_start(
                    &socket,
                    Path::new("rmux-daemon"),
                    |binary| Ok(binary.to_path_buf()),
                    |_| -> io::Result<()> { Err(io::Error::other("must join, not launch")) },
                    DEADLINE,
                )
            },
            |connection| {
                super::super::probe_connected_server(
                    connection,
                    &AutoStartConfig::disabled(),
                    &socket,
                )
            },
            DEADLINE,
        )
        .expect("the next daemon generation answers");
        handover.join().expect("handover thread");
        assert_eq!(provenance, ServerConnectionProvenance::JoinedExisting);
        assert!(launched.is_none());
        assert_eq!(attempts, 2);
    }

    /// Only a joined daemon is retried: one this client launched and that
    /// closes the probe unanswered failed to start, which must surface.
    #[test]
    fn a_launched_daemon_that_closes_the_probe_unanswered_is_not_retried() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime for the started stream");
        let _context = runtime.enter();
        let mut attempts = 0;
        // Keep each daemon end open: macOS refuses socket options once the
        // peer is gone, which would fail before the probe runs.
        let mut daemons = Vec::new();
        let error = super::connect_ready(
            || {
                attempts += 1;
                let (client, daemon) = std::os::unix::net::UnixStream::pair().expect("stream pair");
                daemons.push(daemon);
                client.set_nonblocking(true).expect("nonblocking stream");
                let stream = tokio::net::UnixStream::from_std(client).expect("tokio stream");
                Ok((StartupOutcome::Started(stream), Some(())))
            },
            |_| Err(AutoStartError::Client(ClientError::UnexpectedEof)),
            DEADLINE,
        )
        .expect_err("a launched daemon's failed probe surfaces");
        assert!(
            matches!(error, AutoStartError::Client(ClientError::UnexpectedEof)),
            "unexpected error: {error}"
        );
        assert_eq!(attempts, 1);
    }
}
