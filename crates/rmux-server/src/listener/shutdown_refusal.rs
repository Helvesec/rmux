//! A stopping server closes every connection it accepts, as tmux 3.7b's
//! `server_accept` does once `server_exit` is set.
//!
//! The endpoint stays bound until every accepted lifecycle hook has completed
//! or been cancelled: released earlier, an old hook could reconnect to a new
//! daemon generation through its inherited RMUX/TMUX environment. Left
//! unaccepted, a hook's own CLI call would sit in the kernel queue unanswered,
//! and the drain would wait the whole lifecycle-hook grace for that hook.
//! Closed at once, the call fails like tmux's (the server went away) and the
//! hook runs to its end.
//!
//! A bare close is not final for every client: a server-starting command
//! joins an endpoint that closes unanswered again, until a next daemon serves
//! it, which is how it outlives an ordinary exit. A process of this server's
//! own session (a hook, run-shell or if-shell job; panes start their own
//! session) would wait so for the very drain that waits for it. It is told
//! instead, on every request, what tmux's client prints when the server
//! closes on it, and its call fails at once.

use std::convert::Infallible;
use std::time::Duration;

use rmux_ipc::{LocalListener, LocalStream, PeerIdentity};
use rmux_proto::{ErrorResponse, Response, RmuxError};
use tokio::task::{JoinHandle, JoinSet};
use tracing::debug;

use super::Connection;

/// What tmux 3.7b's client prints when the server closes the connection on it.
const SERVER_EXITED_MESSAGE: &str = "server exited unexpectedly";

/// A failed accept does not end the refusal; it only paces the next attempt.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(25);

/// The bound listener of a stopping server, refusing connections until it is released.
#[derive(Debug)]
pub(super) struct RefusingListener(JoinHandle<Infallible>);

impl RefusingListener {
    /// Moves the still-bound listener, if any, into a task that closes each connection it accepts.
    pub(super) fn start(listener: impl Into<Option<LocalListener>>) -> Option<Self> {
        Self::start_answering(listener, runs_in_this_server_session)
    }

    /// As [`Self::start`], telling the peers `answered` selects that the server exited.
    fn start_answering(
        listener: impl Into<Option<LocalListener>>,
        answered: fn(&PeerIdentity) -> bool,
    ) -> Option<Self> {
        let listener = listener.into()?;
        Some(Self(tokio::spawn(refuse_every_connection(
            listener, answered,
        ))))
    }
}

/// Stops refusing and closes the listener; the endpoint is free once this returns.
pub(super) async fn release(refusing: Option<RefusingListener>) {
    let Some(RefusingListener(task)) = refusing else {
        return;
    };
    task.abort();
    match task.await {
        Err(error) if error.is_cancelled() => {}
        Err(error) => debug!("shutdown connection refusal ended abnormally: {error}"),
        Ok(never) => match never {},
    }
}

async fn refuse_every_connection(
    listener: LocalListener,
    answered: fn(&PeerIdentity) -> bool,
) -> Infallible {
    // Dropped with this task on release, which aborts every answer still open.
    let mut answers = JoinSet::new();
    loop {
        match listener.accept().await {
            Ok((stream, peer)) if answered(&peer) => {
                answers.spawn(answer_server_exited(stream));
            }
            Ok((stream, _peer)) => drop(stream),
            Err(error) => {
                debug!("accept failed while refusing connections during shutdown: {error}");
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
            }
        }
        while answers.try_join_next().is_some() {}
    }
}

/// Answers every request with tmux's lost-server message until the peer closes.
async fn answer_server_exited(stream: LocalStream) {
    let mut connection = Connection::new(stream);
    let response = Response::Error(ErrorResponse {
        error: RmuxError::Server(SERVER_EXITED_MESSAGE.to_owned()),
    });
    while let Ok(Some(_request)) = connection.read_request().await {
        if connection.write_response(&response).await.is_err() {
            return;
        }
    }
}

/// Whether `peer` shares this server's Unix session. A hidden daemon leads
/// its own session, so its peers there are the jobs it spawned outside panes.
#[cfg(unix)]
fn runs_in_this_server_session(peer: &PeerIdentity) -> bool {
    let Some(peer_pid) = i32::try_from(peer.pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match (
        rustix::process::getsid(Some(peer_pid)),
        rustix::process::getsid(None),
    ) {
        (Ok(peer_session), Ok(own_session)) => peer_session == own_session,
        _ => false,
    }
}

/// Windows has no Unix session; every peer keeps the bare close.
#[cfg(not(unix))]
fn runs_in_this_server_session(_peer: &PeerIdentity) -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::path::PathBuf;
    use std::time::Duration;

    use rmux_ipc::{LocalEndpoint, LocalListener};
    use rmux_proto::{
        encode_frame, DaemonStatusRequest, ErrorResponse, FrameDecoder, Request, Response,
        RmuxError,
    };

    use super::{release, RefusingListener, SERVER_EXITED_MESSAGE};

    const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

    fn bind_stopping_listener(name: &str) -> (PathBuf, PathBuf, LocalListener) {
        let directory =
            std::env::temp_dir().join(format!("rmux-refusal-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("socket directory");
        let path = directory.join("stopping.sock");
        let listener =
            LocalListener::bind(&LocalEndpoint::from_path(path.clone())).expect("bind listener");
        (directory, path, listener)
    }

    #[tokio::test]
    async fn a_connection_made_while_the_server_stops_is_closed_unanswered() {
        let (directory, path, listener) = bind_stopping_listener("close");
        // The peer of another session: an ordinary client, or a pane.
        let refusing = RefusingListener::start_answering(listener, |_| false);
        assert!(refusing.is_some(), "a bound listener refuses");

        let read = tokio::task::spawn_blocking(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(&path).expect("connect");
            stream
                .set_read_timeout(Some(CLIENT_TIMEOUT))
                .expect("read timeout");
            let _ = stream.write_all(b"request");
            let mut reply = Vec::new();
            stream.read_to_end(&mut reply).map(|_| reply)
        })
        .await
        .expect("client thread");
        match read {
            Ok(reply) => assert!(reply.is_empty(), "nothing is answered: {reply:?}"),
            Err(error) => assert!(
                matches!(
                    error.kind(),
                    ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
                ),
                "the connection is closed, not left pending: {error:?}"
            ),
        }

        release(refusing).await;
        assert!(
            directory.join("stopping.sock").exists(),
            "releasing the listener leaves the socket file to the shutdown cleanup"
        );
        std::fs::remove_dir_all(&directory).expect("remove socket directory");
    }

    #[tokio::test]
    async fn a_process_of_the_server_session_is_told_on_every_request_that_it_exited() {
        let (directory, path, listener) = bind_stopping_listener("answer");
        // This test process shares its own session, as a hook shares the daemon's.
        let refusing = RefusingListener::start(listener);

        let (responses, rest) = tokio::task::spawn_blocking(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(&path).expect("connect");
            stream
                .set_read_timeout(Some(CLIENT_TIMEOUT))
                .expect("read timeout");
            let request = encode_frame(&Request::DaemonStatus(DaemonStatusRequest)).expect("frame");
            let mut decoder = FrameDecoder::new();
            let mut responses = Vec::new();
            for _ in 0..2 {
                stream.write_all(&request).expect("request");
                let response = loop {
                    if let Some(response) = decoder.next_frame::<Response>().expect("response") {
                        break response;
                    }
                    let mut buffer = [0_u8; 256];
                    let read = stream.read(&mut buffer).expect("an answer, not a close");
                    assert_ne!(read, 0, "the connection is answered before it closes");
                    decoder.push_bytes(&buffer[..read]);
                };
                responses.push(response);
            }
            (responses, stream)
        })
        .await
        .expect("client thread");
        let told = Response::Error(ErrorResponse {
            error: RmuxError::Server(SERVER_EXITED_MESSAGE.to_owned()),
        });
        assert_eq!(responses, [told.clone(), told]);

        release(refusing).await;
        let closed = tokio::task::spawn_blocking(move || {
            let mut rest = rest;
            let mut tail = Vec::new();
            rest.read_to_end(&mut tail).map(|_| tail)
        })
        .await
        .expect("client thread");
        assert!(
            matches!(&closed, Ok(tail) if tail.is_empty())
                || matches!(&closed, Err(error) if error.kind() == ErrorKind::ConnectionReset),
            "releasing the refusal ends the answer: {closed:?}"
        );
        std::fs::remove_dir_all(&directory).expect("remove socket directory");
    }
}
