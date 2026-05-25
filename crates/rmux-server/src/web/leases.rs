use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct LeaseBook {
    current_readers: AtomicUsize,
    max_readers: usize,
    operator_connected: AtomicBool,
}

impl LeaseBook {
    pub(crate) fn new(max_readers: usize) -> Arc<Self> {
        Arc::new(Self {
            current_readers: AtomicUsize::new(0),
            max_readers,
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

    pub(crate) fn try_read(self: &Arc<Self>) -> Option<ReadLease> {
        let mut current = self.current_readers.load(Ordering::Acquire);
        loop {
            if current >= self.max_readers {
                return None;
            }
            match self.current_readers.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ReadLease {
                        book: Arc::clone(self),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    pub(crate) fn reader_count(&self) -> usize {
        self.current_readers.load(Ordering::Acquire)
    }

    fn read_uncapped(self: &Arc<Self>) -> ReadLease {
        self.current_readers.fetch_add(1, Ordering::AcqRel);
        ReadLease {
            book: Arc::clone(self),
        }
    }
}

#[derive(Debug)]
pub(crate) struct ReadLease {
    book: Arc<LeaseBook>,
}

impl Drop for ReadLease {
    fn drop(&mut self) {
        self.book.current_readers.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) struct OperatorLease {
    book: Arc<LeaseBook>,
    active: bool,
}

impl OperatorLease {
    pub(crate) fn release_to_read(mut self) -> ReadLease {
        self.release_operator_slot();
        self.active = false;
        self.book.read_uncapped()
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
    Read(ReadLease),
}

impl ConnectionLease {
    pub(crate) fn is_operator(&self) -> bool {
        matches!(self, Self::Operator(_))
    }

    pub(crate) fn release_operator(self) -> Result<Self, Self> {
        match self {
            Self::Operator(lease) => Ok(Self::Read(lease.release_to_read())),
            Self::Read(lease) => Err(Self::Read(lease)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectionLease, LeaseBook};

    #[test]
    fn read_lease_tracks_count_until_drop() {
        let book = LeaseBook::new(1);
        let read = book.try_read().expect("read slot should be free");

        assert_eq!(book.reader_count(), 1);
        assert!(book.try_read().is_none());

        drop(read);
        assert_eq!(book.reader_count(), 0);
        assert!(book.try_read().is_some());
    }

    #[test]
    fn operator_release_converts_to_uncapped_read_without_leaking_slot() {
        let book = LeaseBook::new(1);
        let _read = book.try_read().expect("read cap should allow one");
        let operator = book.try_operator().expect("operator slot should be free");

        assert!(book.operator_connected());
        let released = operator.release_to_read();
        assert!(!book.operator_connected());
        assert_eq!(book.reader_count(), 2);
        assert!(book.try_operator().is_some());

        drop(released);
        assert_eq!(book.reader_count(), 1);
    }

    #[test]
    fn connection_lease_rejects_read_release() {
        let book = LeaseBook::new(1);
        let read = ConnectionLease::Read(book.try_read().expect("read slot"));

        let returned = read
            .release_operator()
            .expect_err("read cannot release operator mode");
        assert!(!returned.is_operator());
    }
}
