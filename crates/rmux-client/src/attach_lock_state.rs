use std::sync::{Condvar, Mutex};

// Poison is fatal to every acquisition and Condvar reacquisition, including
// close/finish_input_read. An interrupted mutation may leave the input-read,
// exclusive-action generation and closed flags inconsistent; recovering the
// guard cannot establish that terminal input is safe to resume. Do not reuse or
// clear poisoned state. Poison is not a close transition or a wakeup protocol:
// a waiter observes it when notified (or spuriously woken) and reacquiring.
#[derive(Debug, Default)]
pub(crate) struct AttachLockState {
    inner: Mutex<State>,
    changed: Condvar,
}

impl AttachLockState {
    pub(crate) fn lock(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.locked = true;
        state.exclusive_input_generation = state.exclusive_input_generation.wrapping_add(1);
        self.changed.notify_all();
    }

    pub(crate) fn unlock(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.locked = false;
        self.changed.notify_all();
    }

    pub(crate) fn close(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.closed = true;
        self.changed.notify_all();
    }

    pub(crate) fn is_locked(&self) -> bool {
        self.inner
            .lock()
            .expect("attach lock state poisoned")
            .locked
    }

    #[cfg(any(windows, test))]
    pub(crate) fn exclusive_input_generation(&self) -> u64 {
        self.inner
            .lock()
            .expect("attach lock state poisoned")
            .exclusive_input_generation
    }

    #[cfg(windows)]
    pub(crate) fn is_closed(&self) -> bool {
        self.inner
            .lock()
            .expect("attach lock state poisoned")
            .closed
    }

    pub(crate) fn begin_input_read(&self) -> bool {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        if state.closed || state.locked {
            return false;
        }
        state.input_read_active = true;
        true
    }

    pub(crate) fn finish_input_read(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.input_read_active = false;
        self.changed.notify_all();
    }

    pub(crate) fn wait_until_input_idle(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        while state.input_read_active && !state.closed {
            #[cfg(test)]
            state.signal_wait_entry();
            state = self
                .changed
                .wait(state)
                .expect("attach lock state poisoned");
        }
    }

    #[cfg(windows)]
    pub(crate) fn wait_while_locked(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        while state.locked && !state.closed {
            #[cfg(test)]
            state.signal_wait_entry();
            state = self
                .changed
                .wait(state)
                .expect("attach lock state poisoned");
        }
    }

    #[cfg(windows)]
    pub(crate) fn wait_until_closed(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        while !state.closed {
            #[cfg(test)]
            state.signal_wait_entry();
            state = self
                .changed
                .wait(state)
                .expect("attach lock state poisoned");
        }
    }
}

#[derive(Debug, Default)]
struct State {
    locked: bool,
    closed: bool,
    input_read_active: bool,
    exclusive_input_generation: u64,
    #[cfg(test)]
    wait_entered: Option<std::sync::mpsc::Sender<()>>,
}

#[cfg(test)]
impl State {
    fn signal_wait_entry(&self) {
        if let Some(entered) = &self.wait_entered {
            // Sent with the owner guard held, immediately before Condvar::wait.
            entered.send(()).expect("signal attach wait entry");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AttachLockState, State};
    use std::any::Any;
    use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    const DEADLINE: Duration = Duration::from_secs(5);

    fn panic_message(payload: &(dyn Any + Send)) -> &str {
        if let Some(message) = payload.downcast_ref::<&str>() {
            message
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message
        } else {
            panic!("unexpected non-string panic payload");
        }
    }

    fn assert_poison_panic(result: thread::Result<()>) {
        let payload = result.expect_err("owner must reject poisoned state");
        // Check the owner's diagnostic, not Rust's Debug format for PoisonError.
        assert!(
            panic_message(payload.as_ref()).starts_with("attach lock state poisoned:"),
            "unexpected panic: {}",
            panic_message(payload.as_ref())
        );
    }

    fn poison(state: &AttachLockState, mutation: impl FnOnce(&mut State)) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut inner = state.inner.lock().expect("healthy before injection");
            mutation(&mut inner);
            panic!("attach test poison injection");
        }));
        let payload = result.expect_err("injection must unwind with the guard held");
        assert_eq!(
            panic_message(payload.as_ref()),
            "attach test poison injection"
        );
        assert!(state.inner.is_poisoned());
    }

    fn snapshot(state: &AttachLockState) -> (bool, bool, bool, u64) {
        // Test inspection only; this is not a production recovery path.
        let inner = state
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (
            inner.locked,
            inner.closed,
            inner.input_read_active,
            inner.exclusive_input_generation,
        )
    }

    fn with_waiter(
        state: &Arc<AttachLockState>,
        wait: fn(&AttachLockState),
        action: impl FnOnce(&AttachLockState, &mpsc::Receiver<()>),
    ) -> thread::Result<()> {
        let (entered_tx, entered_rx) = mpsc::channel();
        state.inner.lock().unwrap().wait_entered = Some(entered_tx);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter_state = Arc::clone(state);
        let waiter = thread::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| wait(&waiter_state)));
            done_tx.send(()).expect("signal waiter completion");
            result
        });
        let action_result = catch_unwind(AssertUnwindSafe(|| {
            entered_rx.recv_timeout(DEADLINE).expect("wait entry");
            // Acquiring after the signal proves the owner released its guard
            // into the real wait; thread scheduling or elapsed time is no proof.
            let inner = state.inner.lock().unwrap();
            assert_eq!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
            drop(inner);
            action(state, &entered_rx);
        }));
        let completed = done_rx.recv_timeout(DEADLINE);
        if action_result.is_err() || completed.is_err() {
            // Failure cleanup only: satisfy every predicate and explicitly
            // notify even after poison, so an assertion cannot leak a waiter.
            let mut inner = state
                .inner
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            inner.closed = true;
            inner.locked = false;
            inner.input_read_active = false;
            state.changed.notify_all();
        }
        let result = waiter
            .join()
            .unwrap_or_else(|payload| resume_unwind(payload));
        state
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .wait_entered = None;
        if let Err(payload) = action_result {
            resume_unwind(payload);
        }
        completed.expect("waiter completes before failure cleanup");
        result
    }

    #[test]
    fn exclusive_action_waits_for_inflight_console_read() {
        let state = Arc::new(AttachLockState::default());
        assert!(state.begin_input_read());
        state.lock();

        with_waiter(
            &state,
            AttachLockState::wait_until_input_idle,
            |state, _| {
                state.finish_input_read();
            },
        )
        .unwrap_or_else(|payload| resume_unwind(payload));
        assert!(
            !state.begin_input_read(),
            "locked input cannot start a read"
        );
        state.unlock();
        assert!(state.begin_input_read());
        state.finish_input_read();
    }

    #[test]
    fn exclusive_input_generation_changes_when_an_action_starts() {
        let state = AttachLockState::default();
        let initial = state.exclusive_input_generation();

        state.lock();
        let after_lock = state.exclusive_input_generation();
        state.unlock();

        assert_ne!(after_lock, initial);
        assert_eq!(state.exclusive_input_generation(), after_lock);
    }

    #[test]
    fn poison_is_rejected_before_every_acquisition_even_when_closed() {
        type Operation = (&'static str, fn(&AttachLockState));
        let operations: &[Operation] = &[
            ("lock", AttachLockState::lock),
            ("unlock", AttachLockState::unlock),
            ("close", AttachLockState::close),
            ("is_locked", |state| {
                state.is_locked();
            }),
            ("generation", |state| {
                state.exclusive_input_generation();
            }),
            ("begin_input_read", |state| {
                state.begin_input_read();
            }),
            ("finish_input_read", AttachLockState::finish_input_read),
            (
                "wait_until_input_idle",
                AttachLockState::wait_until_input_idle,
            ),
            #[cfg(windows)]
            ("is_closed", |state| {
                state.is_closed();
            }),
            #[cfg(windows)]
            ("wait_while_locked", AttachLockState::wait_while_locked),
            #[cfg(windows)]
            ("wait_until_closed", AttachLockState::wait_until_closed),
        ];
        for (name, operation) in operations {
            let state = AttachLockState::default();
            poison(&state, |inner| {
                // Closed makes all waits immediately ready if wrongly recovered.
                inner.closed = true;
                inner.input_read_active = true;
                inner.exclusive_input_generation = 17;
            });
            let before = snapshot(&state);
            assert_poison_panic(catch_unwind(AssertUnwindSafe(|| operation(&state))));
            assert_eq!(snapshot(&state), before, "{name} must not reuse state");
            assert!(state.inner.is_poisoned(), "{name} must not clear poison");
        }
    }

    fn poisoned_wait(wait: fn(&AttachLockState), close_during_poison: bool) {
        let state = Arc::new(AttachLockState::default());
        assert!(state.begin_input_read());
        state.lock();
        let result = with_waiter(&state, wait, |state, _| {
            poison(state, |inner| {
                inner.closed = close_during_poison;
                // Notify while holding the guard: the waiter cannot reacquire
                // before the unwind has poisoned it. Do not assume poison wakes.
                state.changed.notify_all();
            });
        });
        assert_poison_panic(result);
        assert_eq!(snapshot(&state), (true, close_during_poison, true, 1));
        assert!(state.inner.is_poisoned());
    }

    #[test]
    fn input_idle_wait_rejects_poison_on_wakeup() {
        for closed in [false, true] {
            poisoned_wait(AttachLockState::wait_until_input_idle, closed);
        }
    }

    #[cfg(windows)]
    #[test]
    fn locked_wait_rejects_poison_on_wakeup() {
        for closed in [false, true] {
            poisoned_wait(AttachLockState::wait_while_locked, closed);
        }
    }

    #[cfg(windows)]
    #[test]
    fn closed_wait_rejects_poison_on_wakeup() {
        for closed in [false, true] {
            poisoned_wait(AttachLockState::wait_until_closed, closed);
        }
    }

    #[test]
    fn poisoned_close_and_finish_do_not_complete_shutdown_or_read_cleanup() {
        let state = Arc::new(AttachLockState::default());
        assert!(state.begin_input_read());
        let result = with_waiter(
            &state,
            AttachLockState::wait_until_input_idle,
            |state, _| {
                poison(state, |_| {});
                assert_poison_panic(catch_unwind(|| state.close()));
                assert_poison_panic(catch_unwind(|| state.finish_input_read()));
                assert_eq!(snapshot(state), (false, false, true, 0));
                // Explicit test-only wakeup. Neither poisoning nor a rejected close
                // is a promised notification; a spurious wake remains permissible.
                state.changed.notify_all();
            },
        );
        assert_poison_panic(result);
        assert!(state.inner.is_poisoned());
    }

    #[test]
    fn healthy_close_releases_input_idle_without_completing_the_read() {
        let state = Arc::new(AttachLockState::default());
        assert!(state.begin_input_read());
        with_waiter(
            &state,
            AttachLockState::wait_until_input_idle,
            |state, entered| {
                // An unrelated notification must recheck the still-active predicate.
                state.unlock();
                entered.recv_timeout(DEADLINE).expect("wait reentry");
                state.close();
            },
        )
        .unwrap_or_else(|payload| resume_unwind(payload));
        assert_eq!(snapshot(&state), (false, true, true, 0));
        assert!(!state.begin_input_read());
        state.finish_input_read();
        state.wait_until_input_idle();
        assert_eq!(snapshot(&state), (false, true, false, 0));
    }

    #[cfg(windows)]
    #[test]
    fn healthy_windows_waits_release_on_unlock_or_close() {
        for close in [false, true] {
            let state = Arc::new(AttachLockState::default());
            state.lock();
            with_waiter(
                &state,
                AttachLockState::wait_while_locked,
                |state, entered| {
                    state.finish_input_read();
                    entered.recv_timeout(DEADLINE).expect("locked wait reentry");
                    if close {
                        state.close();
                    } else {
                        state.unlock();
                    }
                },
            )
            .unwrap_or_else(|payload| resume_unwind(payload));
            assert_eq!(state.is_closed(), close);
            assert_eq!(state.is_locked(), close);
            assert_eq!(state.exclusive_input_generation(), 1);
        }
        let state = Arc::new(AttachLockState::default());
        with_waiter(
            &state,
            AttachLockState::wait_until_closed,
            |state, entered| {
                state.unlock();
                entered.recv_timeout(DEADLINE).expect("closed wait reentry");
                state.close();
            },
        )
        .unwrap_or_else(|payload| resume_unwind(payload));
        assert!(state.is_closed());
        state.wait_until_closed();
        state.wait_while_locked();
    }

    #[test]
    fn healthy_generation_wraps_and_close_is_sticky() {
        let state = AttachLockState::default();
        state.inner.lock().unwrap().exclusive_input_generation = u64::MAX;
        state.lock();
        assert_eq!(state.exclusive_input_generation(), 0);
        state.lock();
        assert_eq!(state.exclusive_input_generation(), 1);
        state.close();
        state.unlock();
        assert!(!state.is_locked());
        assert!(!state.begin_input_read());
        state.close();
        state.finish_input_read();
        assert_eq!(snapshot(&state), (false, true, false, 1));
    }
}
