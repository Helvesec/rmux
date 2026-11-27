//! Transfer a single invocation's stdin demand to an independent local client.
use std::collections::HashMap;
use std::io;
use std::sync::Mutex;
use std::time::Duration;

use rmux_os::identity::UserIdentity;
use rmux_proto::{Response, SourceFileRelayV1Response};
use tokio::sync::{oneshot, watch};

use super::source_input::{read_on_connection, InputReply, ReadDemand};
use super::Connection;

#[derive(Debug)]
pub(super) enum RelayDemand {
    Ready(InputReply),
    Pending(oneshot::Receiver<ReadDemand>),
}

impl RelayDemand {
    async fn wait(self) -> Option<InputReply> {
        match self {
            Self::Ready(reply) => Some(reply),
            Self::Pending(read) => read.await.ok().map(|read| read.reply),
        }
    }
}

#[derive(Debug)]
struct Pending {
    demand: RelayDemand,
    proof: [u8; 16],
    claimed: oneshot::Sender<()>,
}

#[derive(Debug, Default)]
pub(crate) struct PendingRelays(Mutex<HashMap<(UserIdentity, [u8; 16]), Pending>>);

struct Registration<'a> {
    registry: &'a PendingRelays,
    key: (UserIdentity, [u8; 16]),
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.registry.0.lock().unwrap().remove(&self.key);
    }
}

impl PendingRelays {
    pub(super) async fn handoff(
        &self,
        conn: &mut Connection,
        user: &UserIdentity,
        demand: RelayDemand,
        shutdown: &mut watch::Receiver<()>,
    ) -> io::Result<()> {
        let mut secrets = [[0; 16]; 2];
        getrandom::fill(secrets.as_flattened_mut())
            .map_err(|error| io::Error::other(error.to_string()))?;
        let [ticket, proof] = secrets;
        let registration = Registration {
            registry: self,
            key: (user.clone(), ticket),
        };
        let (claimed, ack) = oneshot::channel();
        self.0.lock().unwrap().insert(
            registration.key.clone(),
            Pending {
                demand,
                proof,
                claimed,
            },
        );
        conn.write_response(&Response::SourceFileRelayV1(SourceFileRelayV1Response {
            ticket,
            proof,
        }))
        .await?;
        tokio::select! {
            result = ack => result.map_err(io::Error::other),
            result = rmux_ipc::wait_for_peer_close(&conn.stream) => result.and(Err(io::Error::other("source-file caller closed during stdin handoff"))),
            _ = shutdown.changed() => Err(io::Error::other("shutdown during stdin handoff")),
            _ = tokio::time::sleep(Duration::from_secs(5)) => Err(io::Error::new(io::ErrorKind::TimedOut, "source-file stdin reader did not connect")),
        }
    }

    pub(super) async fn serve(
        &self,
        conn: &mut Connection,
        user: &UserIdentity,
        ticket: [u8; 16],
        shutdown: &mut watch::Receiver<()>,
    ) -> io::Result<()> {
        let Some(pending) = self.0.lock().unwrap().remove(&(user.clone(), ticket)) else {
            return Ok(());
        };
        conn.write_response(&Response::SourceFileRelayReadyV1(
            rmux_proto::SourceFileRelayReadyV1Response {
                proof: pending.proof,
            },
        ))
        .await?;
        if pending.claimed.send(()).is_err() {
            return Ok(());
        }
        let reply = tokio::select! {
            reply = pending.demand.wait() => reply,
            result = rmux_ipc::wait_for_peer_close(&conn.stream) => return result,
            _ = shutdown.changed() => return Ok(()),
        };
        if let Some(reply) = reply {
            read_on_connection(conn, reply, shutdown).await?;
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn source_input_reader_claim_is_user_bound_and_single_use() {
        let registry = PendingRelays::default();
        let owner = UserIdentity::Uid(10);
        let ticket = [1; 16];
        let (claimed, mut ack) = oneshot::channel();
        let (offer, demand) = oneshot::channel();
        registry.0.lock().unwrap().insert(
            (owner.clone(), ticket),
            Pending {
                demand: RelayDemand::Pending(demand),
                proof: [2; 16],
                claimed,
            },
        );
        let (stream, _client) = tokio::net::UnixStream::pair().unwrap();
        let mut conn = Connection::new(stream);
        let (_stop, mut shutdown) = watch::channel(());
        registry
            .serve(&mut conn, &UserIdentity::Uid(11), ticket, &mut shutdown)
            .await
            .unwrap();
        assert_eq!(ack.try_recv(), Err(oneshot::error::TryRecvError::Empty));
        drop(offer); // The background branch finishes without reading.
        registry
            .serve(&mut conn, &owner, ticket, &mut shutdown)
            .await
            .unwrap();
        assert_eq!(ack.try_recv(), Ok(()));
        assert!(registry.0.lock().unwrap().is_empty());
    }
}
