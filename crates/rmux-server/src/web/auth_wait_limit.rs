use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};

/// Authentication work that has completed the encrypted pre-ready handshake
/// but has not yet acquired an established-connection permit.
pub(super) const DEFAULT_AUTH_WAIT_LIMIT: usize = 256;
pub(super) const DEFAULT_AUTH_WAIT_PER_KEY_LIMIT: usize = 8;
pub(super) const DEFAULT_AUTH_WAIT_PER_PEER_LIMIT: usize = 4;

#[derive(Debug)]
pub(super) struct AuthWaitLimit {
    inner: Mutex<AuthWaitState>,
    max_per_key: usize,
    max_per_peer: usize,
    max_total: usize,
}

#[derive(Debug, Default)]
struct AuthWaitState {
    by_key: HashMap<String, usize>,
    by_peer: HashMap<IpAddr, usize>,
    total: usize,
}

impl AuthWaitLimit {
    pub(super) fn new(max_total: usize, max_per_key: usize, max_per_peer: usize) -> Arc<Self> {
        debug_assert!(max_total > 0, "auth wait capacity must be non-zero");
        debug_assert!(
            max_per_key > 0,
            "per-key auth wait capacity must be non-zero"
        );
        debug_assert!(
            max_per_peer > 0,
            "per-peer auth wait capacity must be non-zero"
        );
        Arc::new(Self {
            inner: Mutex::new(AuthWaitState::default()),
            max_per_key,
            max_per_peer,
            max_total,
        })
    }

    pub(super) fn try_acquire(
        self: &Arc<Self>,
        key: &str,
        peer: Option<IpAddr>,
    ) -> Option<AuthWaitPermit> {
        // Public tunnels and local reverse proxies connect to the daemon over
        // loopback, so that address identifies the proxy rather than the
        // remote viewer. Applying the peer cap there would turn it into an
        // accidental daemon-wide cap. Global and per-key limits still bound
        // those waiters; directly connected peers retain the per-peer cap.
        let peer = peer
            .filter(|peer| !peer.is_loopback())
            .map(auth_peer_bucket);
        let mut state = self.inner.lock().expect("auth wait limit mutex poisoned");
        if state.total >= self.max_total
            || state.by_key.get(key).copied().unwrap_or(0) >= self.max_per_key
            || peer.is_some_and(|peer| {
                state.by_peer.get(&peer).copied().unwrap_or(0) >= self.max_per_peer
            })
        {
            return None;
        }
        state.total += 1;
        *state.by_key.entry(key.to_owned()).or_default() += 1;
        if let Some(peer) = peer {
            *state.by_peer.entry(peer).or_default() += 1;
        }
        Some(AuthWaitPermit {
            key: key.to_owned(),
            limit: Arc::clone(self),
            peer,
        })
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> usize {
        self.inner
            .lock()
            .expect("auth wait limit mutex poisoned")
            .total
    }

    fn release(&self, key: &str, peer: Option<IpAddr>) {
        let mut state = self.inner.lock().expect("auth wait limit mutex poisoned");
        state.total = state.total.saturating_sub(1);
        decrement_count(&mut state.by_key, key);
        if let Some(peer) = peer {
            decrement_count(&mut state.by_peer, &peer);
        }
    }
}

#[derive(Debug)]
pub(super) struct AuthWaitPermit {
    key: String,
    limit: Arc<AuthWaitLimit>,
    peer: Option<IpAddr>,
}

impl Drop for AuthWaitPermit {
    fn drop(&mut self) {
        self.limit.release(&self.key, self.peer);
    }
}

fn decrement_count<K, Q>(counts: &mut HashMap<K, usize>, key: &Q)
where
    K: Eq + std::hash::Hash + std::borrow::Borrow<Q>,
    Q: Eq + std::hash::Hash + ?Sized,
{
    let remove = match counts.get_mut(key) {
        Some(count) => {
            *count = count.saturating_sub(1);
            *count == 0
        }
        None => false,
    };
    if remove {
        counts.remove(key);
    }
}

pub(super) fn auth_peer_bucket(peer: IpAddr) -> IpAddr {
    match peer {
        IpAddr::V4(addr) => IpAddr::V4(addr),
        IpAddr::V6(addr) => {
            let mut octets = addr.octets();
            octets[8..].fill(0);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::AuthWaitLimit;

    #[test]
    fn one_key_cannot_consume_the_global_wait_capacity() {
        let limit = AuthWaitLimit::new(2, 1, 2);
        let first = limit
            .try_acquire("share-a:spectator", None)
            .expect("first fits");

        assert!(limit.try_acquire("share-a:spectator", None).is_none());
        let other = limit
            .try_acquire("share-b:spectator", None)
            .expect("another key retains a slot");
        assert_eq!(limit.active_count(), 2);

        drop(first);
        drop(other);
        assert_eq!(limit.active_count(), 0);
    }

    #[test]
    fn one_peer_cannot_fill_waiters_across_keys() {
        let limit = AuthWaitLimit::new(3, 3, 1);
        let first_peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let other_peer = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));
        let first = limit
            .try_acquire("share-a:spectator", Some(first_peer))
            .expect("first peer fits");

        assert!(limit
            .try_acquire("share-b:spectator", Some(first_peer))
            .is_none());
        assert!(limit
            .try_acquire("share-b:spectator", Some(other_peer))
            .is_some());

        drop(first);
        assert!(limit
            .try_acquire("share-b:spectator", Some(first_peer))
            .is_some());
    }

    #[test]
    fn loopback_proxy_peers_rely_on_global_and_key_limits() {
        let limit = AuthWaitLimit::new(3, 2, 1);
        let first = limit
            .try_acquire("share-a:spectator", Some(IpAddr::V4(Ipv4Addr::LOCALHOST)))
            .expect("first loopback waiter fits");
        let second = limit
            .try_acquire("share-b:spectator", Some(IpAddr::V4(Ipv4Addr::LOCALHOST)))
            .expect("the proxy address must not couple independent keys");
        let third = limit
            .try_acquire(
                "share-c:spectator",
                Some(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
            )
            .expect("IPv6 loopback is proxy-local too");

        assert!(limit
            .try_acquire("share-d:spectator", Some(IpAddr::V4(Ipv4Addr::LOCALHOST)))
            .is_none());
        assert_eq!(limit.active_count(), 3, "the global limit still applies");

        drop(first);
        drop(second);
        drop(third);
        assert_eq!(limit.active_count(), 0);
    }
}
