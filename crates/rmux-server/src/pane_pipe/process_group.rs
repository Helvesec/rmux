use std::io;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rmux_os::process_tree::{ProcessTreeChild, ProcessTreeController};

const PIPE_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);

struct ActivePipeChildGuard<'a>(&'a PipeChildProcessGroup);

impl Drop for ActivePipeChildGuard<'_> {
    fn drop(&mut self) {
        self.0.mark_child_stopped_for_test();
    }
}

pub(super) fn wait_for_pipe_child(
    mut child: ProcessTreeChild,
    stop_flag: Arc<AtomicBool>,
    process_group: Arc<PipeChildProcessGroup>,
) -> ProcessTreeChild {
    let _active_child = ActivePipeChildGuard(&process_group);
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
    #[cfg(test)]
    child_wait_pending: AtomicBool,
}

impl PipeChildProcessGroup {
    pub(super) fn from_controller(target: ProcessTreeController) -> Self {
        Self {
            target,
            armed: AtomicBool::new(true),
            #[cfg(test)]
            termination_count: AtomicUsize::new(0),
            #[cfg(test)]
            child_wait_pending: AtomicBool::new(true),
        }
    }

    fn mark_child_stopped_for_test(&self) {
        #[cfg(test)]
        self.child_wait_pending.store(false, Ordering::SeqCst);
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

    #[cfg(test)]
    pub(super) fn child_wait_pending_for_test(&self) -> bool {
        self.child_wait_pending.load(Ordering::SeqCst)
    }
}
