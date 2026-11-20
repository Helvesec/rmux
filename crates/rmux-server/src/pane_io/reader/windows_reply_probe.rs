//! A reply-write pause owned by one native reader thread, never shared globally.
use std::cell::RefCell;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
struct State {
    entered: bool,
    released: bool,
}

#[derive(Default)]
pub(super) struct ReplyPause {
    state: Mutex<State>,
    progress: Condvar,
}

impl ReplyPause {
    pub(super) fn wait_until_entered(&self, timeout: Duration) -> bool {
        let (state, _) = self
            .progress
            .wait_timeout_while(self.state.lock().unwrap(), timeout, |state| !state.entered)
            .unwrap();
        state.entered
    }

    pub(super) fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.progress.notify_all();
    }

    fn pause(&self) {
        let mut state = self.state.lock().unwrap();
        state.entered = true;
        self.progress.notify_all();
        while !state.released {
            state = self.progress.wait(state).unwrap();
        }
    }
}

thread_local! {
    static PAUSE: RefCell<Option<Arc<ReplyPause>>> = const { RefCell::new(None) };
}

pub(super) struct Scope(Option<Arc<ReplyPause>>);
impl Drop for Scope {
    fn drop(&mut self) {
        PAUSE.with(|cell| cell.replace(self.0.take()));
    }
}

pub(super) fn install(pause: Option<Arc<ReplyPause>>) -> Scope {
    Scope(PAUSE.with(|cell| cell.replace(pause)))
}

pub(super) fn before_write() {
    let pause = PAUSE.with(|cell| cell.borrow().clone());
    if let Some(pause) = pause {
        pause.pause();
    }
}
