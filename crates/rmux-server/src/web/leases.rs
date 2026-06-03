use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct LeaseBook {
    current_operators: AtomicUsize,
    current_spectators: AtomicUsize,
    max_operators: Option<usize>,
    max_spectators: Option<usize>,
    next_operator_id: AtomicU64,
    operator_order: Mutex<Vec<u64>>,
}

impl LeaseBook {
    pub(crate) fn new(max_spectators: Option<usize>, max_operators: Option<usize>) -> Arc<Self> {
        Arc::new(Self {
            current_operators: AtomicUsize::new(0),
            current_spectators: AtomicUsize::new(0),
            max_operators,
            max_spectators,
            next_operator_id: AtomicU64::new(1),
            operator_order: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn operator_count(&self) -> usize {
        self.current_operators.load(Ordering::Acquire)
    }

    pub(crate) fn try_operator(self: &Arc<Self>) -> Option<OperatorLease> {
        increment_bounded(&self.current_operators, self.max_operators)?;
        let id = self.next_operator_id.fetch_add(1, Ordering::AcqRel);
        self.operator_order
            .lock()
            .expect("operator order mutex must not be poisoned")
            .push(id);
        Some(OperatorLease {
            book: Arc::clone(self),
            id,
        })
    }

    pub(crate) fn try_spectator(self: &Arc<Self>) -> Option<SpectatorLease> {
        increment_bounded(&self.current_spectators, self.max_spectators)?;
        Some(SpectatorLease {
            book: Arc::clone(self),
        })
    }

    pub(crate) fn spectator_count(&self) -> usize {
        self.current_spectators.load(Ordering::Acquire)
    }

    fn release_operator(&self, id: u64) {
        self.current_operators.fetch_sub(1, Ordering::AcqRel);
        self.operator_order
            .lock()
            .expect("operator order mutex must not be poisoned")
            .retain(|candidate| *candidate != id);
    }

    fn current_resize_operator(&self) -> Option<u64> {
        self.operator_order
            .lock()
            .expect("operator order mutex must not be poisoned")
            .last()
            .copied()
    }
}

fn increment_bounded(count: &AtomicUsize, max: Option<usize>) -> Option<()> {
    let mut current = count.load(Ordering::Acquire);
    loop {
        if max.is_some_and(|max| current >= max) {
            return None;
        }
        match count.compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(()),
            Err(observed) => current = observed,
        }
    }
}

#[derive(Debug)]
pub(crate) struct SpectatorLease {
    book: Arc<LeaseBook>,
}

impl Drop for SpectatorLease {
    fn drop(&mut self) {
        self.book.current_spectators.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) struct OperatorLease {
    book: Arc<LeaseBook>,
    id: u64,
}

impl OperatorLease {
    pub(crate) fn is_resize_authority(&self) -> bool {
        self.book.current_resize_operator() == Some(self.id)
    }
}

impl Drop for OperatorLease {
    fn drop(&mut self) {
        self.book.release_operator(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::LeaseBook;

    #[test]
    fn spectator_lease_tracks_count_until_drop() {
        let book = LeaseBook::new(Some(1), None);
        let spectator = book.try_spectator().expect("spectator slot should be free");

        assert_eq!(book.spectator_count(), 1);
        assert!(book.try_spectator().is_none());

        drop(spectator);
        assert_eq!(book.spectator_count(), 0);
        assert!(book.try_spectator().is_some());
    }

    #[test]
    fn uncapped_spectator_lease_has_no_limit() {
        let book = LeaseBook::new(None, None);

        let _first = book.try_spectator().expect("uncapped slot");
        let _second = book.try_spectator().expect("uncapped slot");

        assert_eq!(book.spectator_count(), 2);
    }

    #[test]
    fn operator_lease_tracks_count_until_drop() {
        let book = LeaseBook::new(None, Some(1));
        let operator = book.try_operator().expect("operator slot should be free");

        assert_eq!(book.operator_count(), 1);
        assert!(book.try_operator().is_none());

        drop(operator);
        assert_eq!(book.operator_count(), 0);
        assert!(book.try_operator().is_some());
    }

    #[test]
    fn newest_operator_is_resize_authority_until_it_drops() {
        let book = LeaseBook::new(None, None);
        let first = book.try_operator().expect("first operator");
        assert!(first.is_resize_authority());

        let second = book.try_operator().expect("second operator");
        assert!(!first.is_resize_authority());
        assert!(second.is_resize_authority());

        drop(second);
        assert!(first.is_resize_authority());
    }
}
