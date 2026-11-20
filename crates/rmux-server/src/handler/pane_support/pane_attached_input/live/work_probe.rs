//! Counts the work the live input path does, step by step, so a test bounds the work one frame
//! causes instead of the wall-clock time a loaded host needs to do it.

use std::sync::atomic::{AtomicUsize, Ordering};

/// What one handler's live input steps have done since the last [`LiveInputWorkProbe::take`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct LiveInputWork {
    /// Live input steps taken.
    pub(in crate::handler) steps: usize,
    /// Bytes the steps were handed, retained input included: the bytes they decode and may copy.
    pub(in crate::handler) step_bytes: usize,
    /// The most bytes one step was handed.
    pub(in crate::handler) largest_step_bytes: usize,
    /// How far apart on the thread's stack the steps started: steps taken one after another
    /// start at the same few depths, steps nested by recursion start ever deeper.
    pub(in crate::handler) stack_span: usize,
}

#[derive(Debug)]
pub(in crate::handler) struct LiveInputWorkProbe {
    steps: AtomicUsize,
    step_bytes: AtomicUsize,
    largest_step_bytes: AtomicUsize,
    lowest_stack: AtomicUsize,
    highest_stack: AtomicUsize,
}

impl Default for LiveInputWorkProbe {
    fn default() -> Self {
        Self {
            steps: AtomicUsize::new(0),
            step_bytes: AtomicUsize::new(0),
            largest_step_bytes: AtomicUsize::new(0),
            lowest_stack: AtomicUsize::new(usize::MAX),
            highest_stack: AtomicUsize::new(0),
        }
    }
}

impl LiveInputWorkProbe {
    /// Counts one live input step handed `input_bytes` bytes.
    #[inline(never)]
    pub(in crate::handler) fn enter_step(&self, input_bytes: usize) {
        let marker = 0_u8;
        let stack = std::hint::black_box(std::ptr::addr_of!(marker)) as usize;
        self.steps.fetch_add(1, Ordering::Relaxed);
        self.step_bytes.fetch_add(input_bytes, Ordering::Relaxed);
        self.largest_step_bytes
            .fetch_max(input_bytes, Ordering::Relaxed);
        self.lowest_stack.fetch_min(stack, Ordering::Relaxed);
        self.highest_stack.fetch_max(stack, Ordering::Relaxed);
    }

    /// Returns the work done so far and starts counting afresh.
    pub(in crate::handler) fn take(&self) -> LiveInputWork {
        let lowest_stack = self.lowest_stack.swap(usize::MAX, Ordering::Relaxed);
        let highest_stack = self.highest_stack.swap(0, Ordering::Relaxed);
        LiveInputWork {
            steps: self.steps.swap(0, Ordering::Relaxed),
            step_bytes: self.step_bytes.swap(0, Ordering::Relaxed),
            largest_step_bytes: self.largest_step_bytes.swap(0, Ordering::Relaxed),
            stack_span: highest_stack.saturating_sub(lowest_stack),
        }
    }
}
