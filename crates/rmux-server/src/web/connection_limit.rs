use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Hard process-wide cap for authenticated web-share clients.
///
/// Role limits remain per-share product controls; this cap is a daemon safety
/// guard so a valid shared link cannot keep opening idle WebSockets until the
/// rmux process runs out of file descriptors.
pub(super) const DEFAULT_AUTHENTICATED_CONNECTION_LIMIT: usize = 256;

#[derive(Debug)]
pub(super) struct ConnectionLimit {
    active: AtomicUsize,
    max: usize,
}

impl ConnectionLimit {
    pub(super) fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicUsize::new(0),
            max,
        })
    }

    pub(super) fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let mut active = self.active.load(Ordering::Acquire);
        loop {
            if active >= self.max {
                return None;
            }
            match self.active.compare_exchange(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ConnectionPermit {
                        limit: Arc::clone(self),
                    });
                }
                Err(observed) => active = observed,
            }
        }
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(super) struct ConnectionPermit {
    limit: Arc<ConnectionLimit>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.limit.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionLimit;

    #[test]
    fn connection_limit_releases_on_drop() {
        let limit = ConnectionLimit::new(1);
        let permit = limit.try_acquire().expect("first connection fits");

        assert_eq!(limit.active_count(), 1);
        assert!(limit.try_acquire().is_none());

        drop(permit);
        assert_eq!(limit.active_count(), 0);
        assert!(limit.try_acquire().is_some());
    }
}
