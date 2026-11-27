//! Deterministic deadline crossing at the optimized capture return boundary.
use std::cell::Cell;
use std::time::{Duration, Instant};

thread_local! {
    static CLOCK: Cell<Option<Instant>> = const { Cell::new(None) };
    static EXPIRE_ON_SELECTION: Cell<bool> = const { Cell::new(false) };
    static EXPIRE_AFTER_CAPTURE: Cell<bool> = const { Cell::new(false) };
}

pub(crate) struct CaptureExpiry;

impl CaptureExpiry {
    pub(crate) fn arm_on_selection() -> Self {
        let guard = Self::arm();
        EXPIRE_AFTER_CAPTURE.with(|armed| armed.set(false));
        EXPIRE_ON_SELECTION.with(|armed| armed.set(true));
        guard
    }

    pub(crate) fn arm() -> Self {
        CLOCK.with(|clock| clock.set(Some(Instant::now())));
        EXPIRE_AFTER_CAPTURE.with(|armed| armed.set(true));
        Self
    }
}

impl Drop for CaptureExpiry {
    fn drop(&mut self) {
        CLOCK.with(|clock| clock.set(None));
        EXPIRE_ON_SELECTION.with(|armed| armed.set(false));
        EXPIRE_AFTER_CAPTURE.with(|armed| armed.set(false));
    }
}

pub(crate) fn now() -> Option<Instant> {
    let now = CLOCK.with(Cell::get);
    if EXPIRE_ON_SELECTION.with(|armed| armed.replace(false)) {
        CLOCK.with(|clock| clock.set(now.map(|now| now + Duration::from_secs(2))));
    }
    now
}

pub(crate) fn after_optimized_capture() {
    if EXPIRE_AFTER_CAPTURE.with(|armed| armed.replace(false)) {
        CLOCK.with(|clock| clock.set(clock.get().map(|now| now + Duration::from_secs(2))));
    }
}
