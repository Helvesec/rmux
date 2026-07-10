use std::sync::{Condvar, Mutex};

#[derive(Debug, Default)]
pub(super) struct AttachLockState {
    inner: Mutex<State>,
    changed: Condvar,
}

impl AttachLockState {
    pub(super) fn lock(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.locked = true;
        self.changed.notify_all();
    }

    pub(super) fn unlock(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.locked = false;
        self.changed.notify_all();
    }

    pub(super) fn close(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.closed = true;
        self.changed.notify_all();
    }

    pub(super) fn is_locked(&self) -> bool {
        self.inner
            .lock()
            .expect("attach lock state poisoned")
            .locked
    }

    pub(super) fn is_closed(&self) -> bool {
        self.inner
            .lock()
            .expect("attach lock state poisoned")
            .closed
    }

    pub(super) fn begin_input_read(&self) -> bool {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        if state.closed || state.locked {
            return false;
        }
        state.input_read_active = true;
        true
    }

    pub(super) fn finish_input_read(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        state.input_read_active = false;
        self.changed.notify_all();
    }

    pub(super) fn wait_until_input_idle(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        while state.input_read_active && !state.closed {
            state = self
                .changed
                .wait(state)
                .expect("attach lock state poisoned");
        }
    }

    pub(super) fn wait_while_locked(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        while state.locked && !state.closed {
            state = self
                .changed
                .wait(state)
                .expect("attach lock state poisoned");
        }
    }

    pub(super) fn wait_until_closed(&self) {
        let mut state = self.inner.lock().expect("attach lock state poisoned");
        while !state.closed {
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
}

#[cfg(test)]
mod tests {
    use super::AttachLockState;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn exclusive_action_waits_for_inflight_console_read() {
        let state = Arc::new(AttachLockState::default());
        assert!(state.begin_input_read());
        state.lock();

        let waiter_state = Arc::clone(&state);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            waiter_state.wait_until_input_idle();
            done_tx.send(()).expect("signal idle");
        });

        assert!(done_rx.recv_timeout(Duration::from_millis(25)).is_err());
        state.finish_input_read();
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("exclusive action unblocks after read completion");
        waiter.join().expect("waiter joins");
        assert!(
            !state.begin_input_read(),
            "locked input cannot start a read"
        );
        state.unlock();
        assert!(state.begin_input_read());
        state.finish_input_read();
    }
}
