use rmux_ipc::PeerIdentity;

use crate::server_access::ServerAccessAdmission;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum DetachedRequesterAuthority {
    Admission(ServerAccessAdmission),
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct RequesterOrigin {
    pub(in crate::handler) requester_pid: u32,
    pub(in crate::handler) authority: DetachedRequesterAuthority,
}

impl RequesterOrigin {
    #[must_use]
    pub(in crate::handler) const fn new(
        requester_pid: u32,
        authority: DetachedRequesterAuthority,
    ) -> Self {
        Self {
            requester_pid,
            authority,
        }
    }

    #[must_use]
    pub(in crate::handler) const fn requester_pid(&self) -> u32 {
        self.requester_pid
    }
}

/// One in-flight detached scope for a requester.
///
/// `peer` is present only when the connection loop opened this scope directly
/// from an accepted stream, so it is the local peer the OS authenticated for
/// that connection rather than anything derived from the pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct DetachedRequesterScope {
    pub(in crate::handler) authority: DetachedRequesterAuthority,
    peer: Option<PeerIdentity>,
}

impl DetachedRequesterScope {
    #[must_use]
    pub(in crate::handler) const fn new(
        authority: DetachedRequesterAuthority,
        peer: Option<PeerIdentity>,
    ) -> Self {
        Self { authority, peer }
    }
}

#[derive(Debug, Default)]
pub(in crate::handler) struct DetachedRequesterAccess {
    pub(in crate::handler) scopes: Vec<DetachedRequesterScope>,
}

impl DetachedRequesterAccess {
    /// Identical nested scopes are one authority. Any identity, epoch,
    /// mode, or denied-scope disagreement is ambiguous and fails closed.
    pub(in crate::handler) fn unambiguous_admission(&self) -> Option<&ServerAccessAdmission> {
        let first = &self.scopes.first()?.authority;
        let DetachedRequesterAuthority::Admission(admission) = first else {
            return None;
        };
        self.scopes
            .iter()
            .all(|candidate| &candidate.authority == first)
            .then_some(admission)
    }

    /// The local peer every scope this requester currently holds was
    /// authenticated as.
    ///
    /// Concurrent scopes disagreeing on the peer, or holding none, can only
    /// mean two different local processes reached the same pid key, so this
    /// fails closed rather than lending one process the other's identity.
    pub(in crate::handler) fn unambiguous_peer(&self) -> Option<&PeerIdentity> {
        let first = self.scopes.first()?.peer.as_ref()?;
        self.scopes
            .iter()
            .all(|candidate| candidate.peer.as_ref() == Some(first))
            .then_some(first)
    }

    /// Whether two scopes on this pid were authenticated as different local
    /// peers.
    ///
    /// [`Self::unambiguous_peer`] cannot say why it found nothing, and the two
    /// reasons are not interchangeable: a pid holding no authenticated peer at
    /// all is an in-process dispatch, while a pid holding two is a reused pid
    /// whose requests must not borrow either peer's identity (issue #182).
    pub(in crate::handler) fn has_conflicting_peers(&self) -> bool {
        let mut peers = self.scopes.iter().filter_map(|scope| scope.peer.as_ref());
        let Some(first) = peers.next() else {
            return false;
        };
        peers.any(|candidate| candidate != first)
    }

    pub(in crate::handler) fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }
}
