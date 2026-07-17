use std::collections::VecDeque;
use std::future::pending;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::web::auth_wait_limit::auth_peer_bucket;

#[derive(Clone)]
pub(super) struct PreAuthQueue {
    inner: Arc<Mutex<PreAuthQueueState>>,
    capacity: usize,
    per_ip_capacity: usize,
}

#[derive(Default)]
struct PreAuthQueueState {
    next_id: u64,
    entries: VecDeque<PreAuthEntry>,
}

struct PreAuthEntry {
    cancellation: Option<oneshot::Sender<()>>,
    id: u64,
    peer_bucket: Option<IpAddr>,
    protected: bool,
    released: Option<oneshot::Sender<()>>,
}

pub(super) struct PreAuthGuard {
    queue: PreAuthQueue,
    id: u64,
}

pub(super) struct PreAuthAdmission {
    cancellation: PreAuthCancellation,
    guard: PreAuthGuard,
}

pub(super) struct PreAuthCancellation {
    receiver: oneshot::Receiver<()>,
}

enum AdmissionAttempt {
    Admitted(PreAuthAdmission),
    Rejected,
    WaitForRelease(oneshot::Receiver<()>),
}

impl PreAuthQueue {
    #[cfg(test)]
    pub(super) fn new(capacity: usize) -> Self {
        Self::with_per_ip_capacity(capacity, capacity)
    }

    pub(super) fn with_per_ip_capacity(capacity: usize, per_ip_capacity: usize) -> Self {
        debug_assert!(capacity > 0, "pre-auth queue capacity must be non-zero");
        debug_assert!(
            per_ip_capacity > 0,
            "pre-auth per-IP capacity must be non-zero"
        );
        Self {
            inner: Arc::new(Mutex::new(PreAuthQueueState::default())),
            capacity,
            per_ip_capacity,
        }
    }

    #[cfg(test)]
    pub(super) fn try_register(&self) -> Option<PreAuthAdmission> {
        match self.try_register_inner(None, false) {
            AdmissionAttempt::Admitted(admission) => Some(admission),
            AdmissionAttempt::Rejected | AdmissionAttempt::WaitForRelease(_) => None,
        }
    }

    #[cfg(test)]
    pub(super) fn try_register_peer(&self, peer_ip: IpAddr) -> Option<PreAuthAdmission> {
        match self.try_register_inner(Some(peer_ip), false) {
            AdmissionAttempt::Admitted(admission) => Some(admission),
            AdmissionAttempt::Rejected | AdmissionAttempt::WaitForRelease(_) => None,
        }
    }

    pub(super) async fn admit_peer(&self, peer_ip: IpAddr) -> Option<PreAuthAdmission> {
        loop {
            match self.try_register_inner(Some(peer_ip), true) {
                AdmissionAttempt::Admitted(admission) => return Some(admission),
                AdmissionAttempt::Rejected => return None,
                AdmissionAttempt::WaitForRelease(released) => {
                    let _ = released.await;
                }
            }
        }
    }

    fn try_register_inner(
        &self,
        peer_ip: Option<IpAddr>,
        evict_unproved: bool,
    ) -> AdmissionAttempt {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        // Tunnel providers and local reverse proxies connect through loopback,
        // so that address identifies the proxy rather than one remote viewer.
        // Applying the per-peer cap there would let a few incomplete requests
        // starve every viewer behind the tunnel. The global queue capacity still
        // bounds all pending loopback handshakes.
        let peer_bucket = peer_ip
            .filter(|peer| !peer.is_loopback())
            .map(auth_peer_bucket);
        if let Some(peer_bucket) = peer_bucket {
            let active_for_ip = state
                .entries
                .iter()
                .filter(|entry| entry.peer_bucket == Some(peer_bucket))
                .count();
            if active_for_ip >= self.per_ip_capacity {
                return AdmissionAttempt::Rejected;
            }
        }
        if state.entries.len() >= self.capacity {
            if !evict_unproved {
                return AdmissionAttempt::Rejected;
            }
            let Some(oldest_unproved) = state
                .entries
                .iter_mut()
                .find(|entry| !entry.protected && entry.cancellation.is_some())
            else {
                return AdmissionAttempt::Rejected;
            };
            let (released_sender, released) = oneshot::channel();
            oldest_unproved.released = Some(released_sender);
            let cancellation = oldest_unproved
                .cancellation
                .take()
                .expect("unproved entry has a cancellation sender");
            if cancellation.send(()).is_err() {
                oldest_unproved.released = None;
                return AdmissionAttempt::Rejected;
            }
            return AdmissionAttempt::WaitForRelease(released);
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        let (cancellation, receiver) = oneshot::channel();
        state.entries.push_back(PreAuthEntry {
            cancellation: Some(cancellation),
            id,
            peer_bucket,
            protected: false,
            released: None,
        });
        AdmissionAttempt::Admitted(PreAuthAdmission {
            cancellation: PreAuthCancellation { receiver },
            guard: PreAuthGuard {
                queue: self.clone(),
                id,
            },
        })
    }

    fn remove(&self, id: u64) {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        if let Some(index) = state.entries.iter().position(|entry| entry.id == id) {
            if let Some(mut entry) = state.entries.remove(index) {
                if let Some(released) = entry.released.take() {
                    let _ = released.send(());
                }
            }
        }
    }

    fn protect(&self, id: u64) -> bool {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        if entry.cancellation.is_none() {
            return false;
        }
        entry.protected = true;
        true
    }

    #[cfg(test)]
    pub(super) fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .expect("pre-auth queue lock poisoned")
            .entries
            .len()
    }
}

impl PreAuthAdmission {
    pub(super) fn into_parts(self) -> (PreAuthGuard, PreAuthCancellation) {
        (self.guard, self.cancellation)
    }
}

impl PreAuthCancellation {
    pub(super) async fn cancelled(self) {
        if self.receiver.await.is_err() {
            pending::<()>().await;
        }
    }
}

impl PreAuthGuard {
    pub(super) fn protect(&self) -> bool {
        self.queue.protect(self.id)
    }
}

impl Drop for PreAuthGuard {
    fn drop(&mut self) {
        self.queue.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use tokio::sync::oneshot;

    use crate::web::auth_wait_limit::auth_peer_bucket;

    use super::PreAuthQueue;

    #[test]
    fn pre_auth_queue_enforces_per_ip_capacity() {
        let queue = PreAuthQueue::with_per_ip_capacity(8, 2);
        let first_ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let second_ip = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));
        let first = queue
            .try_register_peer(first_ip)
            .expect("first connection from IP fits");
        let second = queue
            .try_register_peer(first_ip)
            .expect("second connection from IP fits");

        assert!(
            queue.try_register_peer(first_ip).is_none(),
            "third pending connection from same IP is rejected"
        );
        assert!(
            queue.try_register_peer(second_ip).is_some(),
            "another IP can still use free global slots"
        );

        drop(first);
        assert!(
            queue.try_register_peer(first_ip).is_some(),
            "dropping a guard frees that IP's slot"
        );
        drop(second);
    }

    #[test]
    fn pre_auth_queue_buckets_ipv6_peers_by_64_prefix() {
        let queue = PreAuthQueue::with_per_ip_capacity(8, 2);
        let first = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, 1));
        let second = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, 2));
        let third = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, 3));
        let different_prefix = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 1, 3, 0, 0, 0, 1));

        assert_eq!(auth_peer_bucket(first), auth_peer_bucket(second));
        let first_guard = queue
            .try_register_peer(first)
            .expect("first connection from /64 fits");
        let second_guard = queue
            .try_register_peer(second)
            .expect("second connection from /64 fits");

        assert!(
            queue.try_register_peer(third).is_none(),
            "third pending connection from same IPv6 /64 is rejected"
        );
        assert!(
            queue.try_register_peer(different_prefix).is_some(),
            "another IPv6 /64 can still use free global slots"
        );

        drop(first_guard);
        drop(second_guard);
    }

    #[test]
    fn pre_auth_queue_bounds_loopback_tunnels_globally_without_a_shared_peer_cap() {
        let queue = PreAuthQueue::with_per_ip_capacity(6, 2);
        let loopback_peers = [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ];
        let guards = loopback_peers
            .into_iter()
            .map(|peer| {
                queue
                    .try_register_peer(peer)
                    .expect("independent tunnel viewer fits within the global bound")
            })
            .collect::<Vec<_>>();

        assert!(
            queue
                .try_register_peer(IpAddr::V4(Ipv4Addr::LOCALHOST))
                .is_none(),
            "loopback handshakes must remain globally bounded"
        );

        drop(guards);
        assert_eq!(queue.pending_count(), 0);
    }

    #[tokio::test]
    async fn pre_auth_queue_skips_protected_entries_when_shedding_load() {
        let queue = PreAuthQueue::new(2);
        let protected = queue.try_register().expect("protected slot");
        let (protected_guard, protected_cancellation) = protected.into_parts();
        assert!(protected_guard.protect());

        let unproved = queue.try_register().expect("unproved slot");
        let (unproved_guard, unproved_cancellation) = unproved.into_parts();
        let unproved_task = tokio::spawn(async move {
            unproved_cancellation.cancelled().await;
            drop(unproved_guard);
        });

        let replacement = queue
            .admit_peer(IpAddr::V4(Ipv4Addr::LOCALHOST))
            .await
            .expect("unproved slot is replaceable");
        unproved_task.await.expect("unproved task joins");
        assert_eq!(queue.pending_count(), 2);

        drop(replacement);
        drop(protected_guard);
        drop(protected_cancellation);
        assert_eq!(queue.pending_count(), 0);
    }

    #[tokio::test]
    async fn pre_auth_replacement_waits_for_the_cancelled_slot_to_release() {
        let queue = PreAuthQueue::new(1);
        let incumbent = queue.try_register().expect("incumbent slot");
        let (incumbent_guard, incumbent_cancellation) = incumbent.into_parts();
        let (cancelled_sender, cancelled) = oneshot::channel();
        let (release_sender, release) = oneshot::channel();
        let incumbent_task = tokio::spawn(async move {
            incumbent_cancellation.cancelled().await;
            let _ = cancelled_sender.send(());
            let _ = release.await;
            drop(incumbent_guard);
        });

        let replacement_queue = queue.clone();
        let replacement_task = tokio::spawn(async move {
            replacement_queue
                .admit_peer(IpAddr::V4(Ipv4Addr::LOCALHOST))
                .await
        });
        cancelled.await.expect("incumbent receives cancellation");
        assert_eq!(queue.pending_count(), 1);
        assert!(!replacement_task.is_finished());

        release_sender.send(()).expect("release incumbent");
        incumbent_task.await.expect("incumbent task joins");
        let replacement = replacement_task
            .await
            .expect("replacement task joins")
            .expect("replacement claims the released slot");
        assert_eq!(queue.pending_count(), 1);
        drop(replacement);
        assert_eq!(queue.pending_count(), 0);
    }
}
