use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::Mutex;

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
    id: u64,
    peer_bucket: Option<IpAddr>,
}

pub(super) struct PreAuthGuard {
    queue: PreAuthQueue,
    id: u64,
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
    pub(super) fn try_register(&self) -> Option<PreAuthGuard> {
        self.try_register_inner(None)
    }

    pub(super) fn try_register_peer(&self, peer_ip: IpAddr) -> Option<PreAuthGuard> {
        self.try_register_inner(Some(peer_ip))
    }

    fn try_register_inner(&self, peer_ip: Option<IpAddr>) -> Option<PreAuthGuard> {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        if state.entries.len() >= self.capacity {
            return None;
        }
        let peer_bucket = peer_ip.map(auth_peer_bucket);
        if let Some(peer_bucket) = peer_bucket {
            let active_for_ip = state
                .entries
                .iter()
                .filter(|entry| entry.peer_bucket == Some(peer_bucket))
                .count();
            if active_for_ip >= self.per_ip_capacity {
                return None;
            }
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.entries.push_back(PreAuthEntry { id, peer_bucket });
        Some(PreAuthGuard {
            queue: self.clone(),
            id,
        })
    }

    fn remove(&self, id: u64) {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        if let Some(index) = state.entries.iter().position(|entry| entry.id == id) {
            state.entries.remove(index);
        }
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

impl Drop for PreAuthGuard {
    fn drop(&mut self) {
        self.queue.remove(self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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
}
