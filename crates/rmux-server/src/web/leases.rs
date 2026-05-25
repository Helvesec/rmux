use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct LeaseBook {
    current_viewers: AtomicUsize,
    max_viewers: usize,
    operator_connected: AtomicBool,
}

impl LeaseBook {
    pub(crate) fn new(max_viewers: usize) -> Arc<Self> {
        Arc::new(Self {
            current_viewers: AtomicUsize::new(0),
            max_viewers,
            operator_connected: AtomicBool::new(false),
        })
    }

    pub(crate) fn operator_connected(&self) -> bool {
        self.operator_connected.load(Ordering::Acquire)
    }

    pub(crate) fn try_operator(self: &Arc<Self>) -> Option<OperatorLease> {
        self.operator_connected
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| OperatorLease {
                book: Arc::clone(self),
                active: true,
            })
    }

    pub(crate) fn try_viewer(self: &Arc<Self>) -> Option<ViewerLease> {
        let mut current = self.current_viewers.load(Ordering::Acquire);
        loop {
            if current >= self.max_viewers {
                return None;
            }
            match self.current_viewers.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ViewerLease {
                        book: Arc::clone(self),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn viewer_count(&self) -> usize {
        self.current_viewers.load(Ordering::Acquire)
    }

    fn viewer_uncapped(self: &Arc<Self>) -> ViewerLease {
        self.current_viewers.fetch_add(1, Ordering::AcqRel);
        ViewerLease {
            book: Arc::clone(self),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewerLease {
    book: Arc<LeaseBook>,
}

impl Drop for ViewerLease {
    fn drop(&mut self) {
        self.book.current_viewers.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) struct OperatorLease {
    book: Arc<LeaseBook>,
    active: bool,
}

impl OperatorLease {
    pub(crate) fn release_to_viewer(mut self) -> ViewerLease {
        self.release_operator_slot();
        self.active = false;
        self.book.viewer_uncapped()
    }

    fn release_operator_slot(&self) {
        self.book.operator_connected.store(false, Ordering::Release);
    }
}

impl Drop for OperatorLease {
    fn drop(&mut self) {
        if self.active {
            self.release_operator_slot();
        }
    }
}

#[derive(Debug)]
pub(crate) enum ConnectionLease {
    Operator(OperatorLease),
    Viewer(ViewerLease),
}

impl ConnectionLease {
    pub(crate) fn is_operator(&self) -> bool {
        matches!(self, Self::Operator(_))
    }

    pub(crate) fn release_operator(self) -> Result<Self, Self> {
        match self {
            Self::Operator(lease) => Ok(Self::Viewer(lease.release_to_viewer())),
            Self::Viewer(lease) => Err(Self::Viewer(lease)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionLease, LeaseBook};

    #[test]
    fn viewer_lease_tracks_count_until_drop() {
        let book = LeaseBook::new(1);
        let viewer = book.try_viewer().expect("viewer slot should be free");

        assert_eq!(book.viewer_count(), 1);
        assert!(book.try_viewer().is_none());

        drop(viewer);
        assert_eq!(book.viewer_count(), 0);
        assert!(book.try_viewer().is_some());
    }

    #[test]
    fn operator_release_converts_to_uncapped_viewer_without_leaking_slot() {
        let book = LeaseBook::new(1);
        let _viewer = book.try_viewer().expect("viewer cap should allow one");
        let operator = book.try_operator().expect("operator slot should be free");

        assert!(book.operator_connected());
        let released = operator.release_to_viewer();
        assert!(!book.operator_connected());
        assert_eq!(book.viewer_count(), 2);
        assert!(book.try_operator().is_some());

        drop(released);
        assert_eq!(book.viewer_count(), 1);
    }

    #[test]
    fn connection_lease_rejects_viewer_release() {
        let book = LeaseBook::new(1);
        let viewer = ConnectionLease::Viewer(book.try_viewer().expect("viewer slot"));

        let returned = viewer
            .release_operator()
            .expect_err("viewer cannot release operator mode");
        assert!(!returned.is_operator());
    }
}
