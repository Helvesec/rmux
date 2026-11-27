use std::future::Future;
use std::pin::Pin;

use rmux_ipc::PeerIdentity;
use rmux_os::identity::UserIdentity;
use rmux_proto::RmuxError;

use crate::handler::attach_support::ClientFlags;
use crate::handler::control_support::{current_control_queue_identity, ControlClientIdentity};
use crate::handler::{
    DetachedRequesterAccess, DetachedRequesterAuthority, RequestHandler, RequesterOrigin,
};
use crate::server_access::{current_owner_uid, AccessMode, ServerAccessAdmission};

enum DetachedAdmissionLookup {
    Absent,
    Unambiguous(ServerAccessAdmission),
    DeniedOrAmbiguous,
}

tokio::task_local! {
    /// The peer the connection loop authenticated for the task serving one
    /// accepted client connection.
    static AUTHENTICATED_CONNECTION_PEER: PeerIdentity;
}

/// Serves one accepted connection under the exact peer the OS authenticated
/// for it.
///
/// The pid alone cannot answer "whose request is this": the OS recycles pids,
/// and a connection's scope stays open across `forward_attach` and
/// `finish_attach`, so two live connections authenticated as different local
/// peers can key the same pid entry. Binding the peer to the connection task
/// keeps every request that connection dispatches on the identity that
/// connection was admitted under, whatever else currently shares its pid
/// (issue #182).
///
/// The scope owns the connection body behind a pointer rather than inline.
/// A connection body reaches the whole request-dispatch tree, so its state
/// machine is one of the deepest types in the server, and `rustc` runs one
/// layout query per level a type is nested under. Holding the body inline
/// stacked this scope's own levels onto the accept loop's and pushed the
/// layout of a connection task past rustc's default query-depth budget, so no
/// ordinary debug build of `rmux` or `rmux-daemon` compiled. Erasing the body
/// costs the caller one pointer however deep the body it guards grows, and the
/// scope still wraps every poll of that body.
pub(crate) fn with_authenticated_connection_peer<'body, T, F>(
    peer: PeerIdentity,
    future: F,
) -> impl Future<Output = T> + Send + 'body
where
    F: Future<Output = T> + Send + 'body,
    T: 'body,
{
    let future: Pin<Box<dyn Future<Output = T> + Send + 'body>> = Box::pin(future);
    AUTHENTICATED_CONNECTION_PEER.scope(peer, future)
}

/// The peer of the connection this task serves, when the request really is
/// that connection's own.
fn current_connection_peer(requester_pid: u32) -> Option<PeerIdentity> {
    AUTHENTICATED_CONNECTION_PEER
        .try_with(|peer| (peer.pid == requester_pid).then(|| peer.clone()))
        .ok()
        .flatten()
}

impl RequestHandler {
    pub(in crate::handler) async fn capture_requester_origin(
        &self,
        requester_pid: u32,
    ) -> RequesterOrigin {
        RequesterOrigin::new(
            requester_pid,
            self.requester_detached_authority(requester_pid).await,
        )
    }

    pub(in crate::handler) async fn requester_can_write(&self, requester_pid: u32) -> bool {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            return self.control_queue_can_write(identity).await;
        }

        // A detached scope identifies the exact RPC admission. Resolve it
        // before PID-only attach/control fallbacks so a colliding local PID
        // cannot lend authority to this request.
        match self.detached_admission_lookup(requester_pid) {
            DetachedAdmissionLookup::Absent => {}
            DetachedAdmissionLookup::Unambiguous(admission) => {
                return self
                    .server_access
                    .lock()
                    .expect("server access mutex must not be poisoned")
                    .revalidate_detached_admission(&admission)
                    .is_some_and(AccessMode::can_write);
            }
            DetachedAdmissionLookup::DeniedOrAmbiguous => return false,
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                return active.can_write && !active.flags.contains(ClientFlags::READONLY);
            }
        }

        let active_control = self.active_control.lock().await;
        if let Some(active) = active_control.by_pid.get(&requester_pid) {
            return active.can_write;
        }
        drop(active_control);

        requester_pid == std::process::id()
    }

    pub(in crate::handler) async fn requester_detached_authority(
        &self,
        requester_pid: u32,
    ) -> DetachedRequesterAuthority {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            return self
                .control_queue_access(identity)
                .await
                .and_then(|(user, can_write)| {
                    self.admission_for_identity_with_write_cap(&user, can_write)
                })
                .map_or(
                    DetachedRequesterAuthority::Denied,
                    DetachedRequesterAuthority::Admission,
                );
        }

        match self.detached_admission_lookup(requester_pid) {
            DetachedAdmissionLookup::Unambiguous(admission) => {
                return DetachedRequesterAuthority::Admission(admission);
            }
            DetachedAdmissionLookup::DeniedOrAmbiguous => {
                return DetachedRequesterAuthority::Denied;
            }
            DetachedAdmissionLookup::Absent => {}
        }

        let attach_access = {
            let active_attach = self.active_attach.lock().await;
            active_attach.by_pid.get(&requester_pid).map(|active| {
                (
                    active.user.clone(),
                    active.can_write && !active.flags.contains(ClientFlags::READONLY),
                )
            })
        };
        if let Some((user, can_write)) = attach_access {
            return self.authority_for_identity(&user, can_write);
        }

        let control_access = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .get(&requester_pid)
                .map(|active| (active.user.clone(), active.can_write))
        };
        if let Some((user, can_write)) = control_access {
            return self.authority_for_identity(&user, can_write);
        }

        if requester_pid == std::process::id() {
            return DetachedRequesterAuthority::Admission(
                self.server_access
                    .lock()
                    .expect("server access mutex must not be poisoned")
                    .owner_admission(),
            );
        }

        DetachedRequesterAuthority::Denied
    }

    /// The local peer the connection loop authenticated for the scope this
    /// request is running under, when there is one.
    ///
    /// This never derives an identity from the pid: the connection serving this
    /// task answers first, and otherwise the pid only selects the scopes the
    /// listener opened from its own accepted streams, where a set that
    /// disagrees resolves to nothing at all.
    pub(in crate::handler) fn authenticated_requester_peer(
        &self,
        requester_pid: u32,
    ) -> Option<PeerIdentity> {
        if let Some(peer) = current_connection_peer(requester_pid) {
            return Some(peer);
        }
        self.active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned")
            .get(&requester_pid)?
            .unambiguous_peer()
            .cloned()
    }

    /// The identity a client attaching on this connection will be registered
    /// under.
    ///
    /// A fresh attach renders its first frame before the listener registers it,
    /// and that listener registers the very peer it authenticated. Reading the
    /// same peer back here keeps the frame's `#{client_uid}` and
    /// `#{client_user}` on the delegated user rather than on the server owner
    /// (issue #182).
    ///
    /// A request with no connection-authenticated peer at all is an in-process
    /// dispatch and stays the owner's, exactly as before. A pid whose open
    /// scopes name different peers is neither: substituting the owner there
    /// would make the first frame disagree with the registration the listener
    /// then writes, so it fails closed before either becomes observable.
    pub(in crate::handler) fn attaching_client_identity(
        &self,
        requester_pid: u32,
    ) -> Result<(u32, UserIdentity), RmuxError> {
        if let Some(peer) = self.authenticated_requester_peer(requester_pid) {
            return Ok((peer.uid, peer.user));
        }
        if self.requester_peer_scopes_conflict(requester_pid) {
            return Err(RmuxError::Server(
                "attaching client identity is ambiguous for this requester".to_owned(),
            ));
        }
        Ok((current_owner_uid(), self.server_owner_identity()))
    }

    /// Whether this pid currently carries connections authenticated as
    /// different local peers.
    fn requester_peer_scopes_conflict(&self, requester_pid: u32) -> bool {
        self.active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned")
            .get(&requester_pid)
            .is_some_and(DetachedRequesterAccess::has_conflicting_peers)
    }

    fn detached_admission_lookup(&self, requester_pid: u32) -> DetachedAdmissionLookup {
        let detached_access = self
            .active_detached_requester_access
            .lock()
            .expect("active detached requester access mutex must not be poisoned");
        let Some(active) = detached_access.get(&requester_pid) else {
            return DetachedAdmissionLookup::Absent;
        };
        active.unambiguous_admission().cloned().map_or(
            DetachedAdmissionLookup::DeniedOrAmbiguous,
            DetachedAdmissionLookup::Unambiguous,
        )
    }

    fn authority_for_identity(
        &self,
        identity: &UserIdentity,
        can_write: bool,
    ) -> DetachedRequesterAuthority {
        self.admission_for_identity_with_write_cap(identity, can_write)
            .map_or(
                DetachedRequesterAuthority::Denied,
                DetachedRequesterAuthority::Admission,
            )
    }

    fn admission_for_identity_with_write_cap(
        &self,
        identity: &UserIdentity,
        can_write: bool,
    ) -> Option<ServerAccessAdmission> {
        self.server_access
            .lock()
            .ok()?
            .admission_for_identity_with_write_cap(identity, can_write)
    }

    async fn control_queue_access(
        &self,
        identity: ControlClientIdentity,
    ) -> Option<(UserIdentity, bool)> {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )
        .ok()?;
        active_control
            .by_pid
            .get(&identity.requester_pid())
            .map(|active| (active.user.clone(), active.can_write))
    }
}

#[cfg(test)]
mod tests {
    use std::future::{pending, ready};

    use rmux_os::identity::UserIdentity;

    use super::{current_connection_peer, with_authenticated_connection_peer, PeerIdentity};

    fn test_peer(pid: u32, uid: u32) -> PeerIdentity {
        PeerIdentity {
            pid,
            uid,
            user: UserIdentity::Uid(uid),
        }
    }

    /// The build seam: opening the scope must cost its caller a pointer, not
    /// the guarded body's whole state machine.
    ///
    /// `rustc` runs one layout query per level a type is nested under, and a
    /// connection body is deep enough that holding it inline here overflowed
    /// the default query-depth budget for every ordinary debug build of `rmux`
    /// and `rmux-daemon`. The falsifiable form of "this scope does not nest
    /// its body" is that the scope's own size does not move when the body's
    /// size does, whatever either size happens to be. The task-local is
    /// private to this module, so no caller can open the scope another way.
    #[test]
    fn the_peer_scope_costs_the_same_whatever_body_it_guards() {
        const PADDING: usize = 64 * 1024;

        let small = ready(());
        let large = async {
            let padding = [0_u8; PADDING];
            pending::<()>().await;
            std::hint::black_box(padding);
        };
        assert!(
            std::mem::size_of_val(&large) >= PADDING,
            "the large probe body must really carry its padding across the await"
        );

        let scoped_small = with_authenticated_connection_peer(test_peer(1, 1), small);
        let scoped_large = with_authenticated_connection_peer(test_peer(1, 1), large);
        assert_eq!(
            std::mem::size_of_val(&scoped_small),
            std::mem::size_of_val(&scoped_large),
            "the peer scope must hold its connection body behind a pointer"
        );
    }

    /// Erasing the body must not narrow what it is scoped to: the connection's
    /// peer is what the body reads on every poll, not only on its first.
    #[tokio::test]
    async fn every_poll_of_a_scoped_body_reads_the_connection_peer() {
        let peer = test_peer(std::process::id().wrapping_add(18_211), 18_211);
        let pid = peer.pid;

        let read = with_authenticated_connection_peer(peer.clone(), async move {
            let first = current_connection_peer(pid);
            // Yielding ends the poll this scope set the peer for, so the next
            // poll has to set it again.
            tokio::task::yield_now().await;
            [first, current_connection_peer(pid)]
        })
        .await;

        assert_eq!(read, [Some(peer.clone()), Some(peer)]);
    }
}
