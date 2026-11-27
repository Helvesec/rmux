//! A running source can ask its own connection for stdin once.
use std::cell::Cell;
use std::future::Future;
use std::io;

use rmux_ipc::wait_for_peer_close;
use rmux_proto::{Request, Response, RmuxError};
use tokio::sync::{oneshot, watch};

use super::Connection;

#[cfg(all(test, unix))]
#[path = "source_input_tests.rs"]
mod tests;

pub(super) type InputReply = oneshot::Sender<Result<Vec<u8>, String>>;

#[derive(Debug)]
pub(crate) struct SourceStdin(oneshot::Sender<ReadDemand>);

#[derive(Debug)]
pub(super) struct ReadDemand {
    pub(super) reply: InputReply,
    foreground: bool,
}

tokio::task_local! {
    static OFFER: Cell<Option<SourceStdin>>;
}

impl SourceStdin {
    pub(crate) fn from_offer() -> Result<Self, RmuxError> {
        OFFER
            .try_with(Cell::take)
            .ok()
            .flatten()
            .ok_or_else(unavailable)
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.0.is_closed()
    }

    pub(crate) async fn read(self) -> Result<Vec<u8>, RmuxError> {
        let (reply, bytes) = oneshot::channel();
        // Spawned queue descendants run outside the foreground dispatch scope.
        let foreground = OFFER.try_with(|_| ()).is_ok();
        self.0
            .send(ReadDemand { reply, foreground })
            .map_err(|_| unavailable())?;
        bytes
            .await
            .map_err(|_| unavailable())?
            .map_err(RmuxError::Server)
    }
}

fn unavailable() -> RmuxError {
    RmuxError::Server("source-file stdin channel unavailable".into())
}

pub(super) async fn exchange<F: Future>(
    conn: &mut Connection,
    dispatch: F,
    shutdown: &mut watch::Receiver<()>,
    relay: Option<(
        &super::source_relay::PendingRelays,
        &rmux_os::identity::UserIdentity,
    )>,
) -> io::Result<F::Output> {
    use super::source_relay::RelayDemand;

    let (client, mut demand) = oneshot::channel();
    // Completing this future drops the task-local offer even if admission failed.
    let execution = async move {
        OFFER
            .scope(Cell::new(Some(SourceStdin(client))), dispatch)
            .await
    };
    tokio::pin!(execution);
    let read = tokio::select! {
        outcome = &mut execution => {
            if let Some((registry, user)) = relay {
                let reader = match demand.try_recv() {
                    Ok(read) => RelayDemand::Ready(read.reply),
                    Err(oneshot::error::TryRecvError::Empty) => RelayDemand::Pending(demand),
                    Err(oneshot::error::TryRecvError::Closed) => return Ok(outcome),
                };
                registry.handoff(conn, user, reader, shutdown).await?;
            }
            return Ok(outcome);
        }
        read = &mut demand => read,
        result = wait_for_peer_close(&conn.stream) => {
            result?;
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "source-file caller closed"));
        }
        _ = shutdown.changed() => return Err(io::Error::new(io::ErrorKind::Interrupted, "shutdown during source-file")),
    };
    if let Ok(read) = read {
        match relay {
            Some((registry, user)) if !read.foreground => {
                registry
                    .handoff(conn, user, RelayDemand::Ready(read.reply), shutdown)
                    .await?;
            }
            _ => {
                read_on_connection(conn, read.reply, shutdown).await?;
            }
        }
    }
    tokio::select! {
        outcome = execution => Ok(outcome),
        result = wait_for_peer_close(&conn.stream) => {
            result?;
            Err(io::Error::new(io::ErrorKind::UnexpectedEof, "source-file caller closed"))
        }
        _ = shutdown.changed() => Err(io::Error::new(io::ErrorKind::Interrupted, "shutdown during source-file")),
    }
}

pub(super) async fn read_on_connection(
    conn: &mut Connection,
    mut reply: InputReply,
    shutdown: &mut watch::Receiver<()>,
) -> io::Result<()> {
    conn.write_response(&Response::SourceFileReadStdinV1(
        rmux_proto::SourceFileReadStdinV1Response,
    ))
    .await?;
    let data = tokio::select! {
        incoming = conn.read_request() => incoming?,
        _ = reply.closed() => return Err(io::Error::other("source-file input consumer closed")),
        _ = shutdown.changed() => return Err(io::Error::new(io::ErrorKind::Interrupted, "shutdown during source-file stdin")),
    };
    match data.map(|incoming| incoming.request) {
        Some(Request::SourceFileInputDataV1(bytes)) => {
            let _ = reply.send(bytes.data);
            Ok(())
        }
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "client closed during source-file stdin",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected source-file stdin data",
        )),
    }
}
