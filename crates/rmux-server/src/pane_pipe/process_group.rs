use std::io;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rmux_os::process_tree::{ProcessTreeChild, ProcessTreeController};

const PIPE_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(test)]
static ACTIVE_PIPE_CHILDREN: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn active_pipe_child_count_for_test() -> usize {
    ACTIVE_PIPE_CHILDREN.load(Ordering::SeqCst)
}

#[cfg(test)]
pub(super) fn mark_pipe_child_started_for_test() {
    ACTIVE_PIPE_CHILDREN.fetch_add(1, Ordering::SeqCst);
}

#[cfg(not(test))]
pub(super) fn mark_pipe_child_started_for_test() {}

struct ActivePipeChildGuard;

impl Drop for ActivePipeChildGuard {
    fn drop(&mut self) {
        #[cfg(test)]
        ACTIVE_PIPE_CHILDREN.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn wait_for_pipe_child(
    mut child: ProcessTreeChild,
    stop_flag: Arc<AtomicBool>,
    process_group: Arc<PipeChildProcessGroup>,
) -> ProcessTreeChild {
    let _active_child = ActivePipeChildGuard;
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            process_group.terminate();
            let _ = child.terminate();
            let _ = child.wait();
            return child;
        }
        match process_group.child_exited(&mut child) {
            Ok(true) | Err(_) => return child,
            Ok(false) => thread::sleep(PIPE_CHILD_POLL_INTERVAL),
        }
    }
}

pub(super) struct PipeChildProcessGroup {
    target: ProcessTreeController,
    armed: AtomicBool,
    #[cfg(test)]
    termination_count: AtomicUsize,
}

impl PipeChildProcessGroup {
    pub(super) fn from_controller(target: ProcessTreeController) -> Self {
        Self {
            target,
            armed: AtomicBool::new(true),
            #[cfg(test)]
            termination_count: AtomicUsize::new(0),
        }
    }

    pub(super) fn child_exited(&self, child: &mut ProcessTreeChild) -> io::Result<bool> {
        child.has_exited()
    }

    pub(super) fn terminate(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        #[cfg(test)]
        self.termination_count.fetch_add(1, Ordering::SeqCst);
        let _ = self.target.terminate();
    }

    #[cfg(test)]
    pub(super) fn is_armed_for_test(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn termination_count_for_test(&self) -> usize {
        self.termination_count.load(Ordering::SeqCst)
    }
}
