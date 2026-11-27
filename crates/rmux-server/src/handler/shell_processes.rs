use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use rmux_os::process_tree::{ProcessTreeChild, ProcessTreeController};

const MAX_SHELL_PROCESS_GROUPS: usize = 1024;
const SHELL_PROCESS_FORCE_TERMINATION_WAIT: Duration = Duration::from_millis(100);
/// How long server shutdown waits for SIGKILLed trees to stop. A loaded host
/// may take far longer than a rejection's short wait to run a killed member's
/// exit; this bounds only a member that never exits.
const SHELL_PROCESS_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

pub(in crate::handler) struct ShellProcessRegistry {
    inner: StdMutex<ShellProcessRegistryInner>,
    closing: AtomicBool,
    limit: usize,
}

#[derive(Default)]
struct ShellProcessRegistryInner {
    closing: bool,
    next_id: u64,
    processes: HashMap<u64, ProcessTreeController>,
}

#[derive(Debug)]
pub(in crate::handler) enum ShellProcessRegistrationError {
    Closing,
    LimitReached { limit: usize },
}

pub(in crate::handler) struct ShellProcessGuard {
    id: u64,
    registry: Arc<ShellProcessRegistry>,
}

impl ShellProcessRegistry {
    pub(in crate::handler) fn new() -> Self {
        Self {
            inner: StdMutex::new(ShellProcessRegistryInner::default()),
            closing: AtomicBool::new(false),
            limit: MAX_SHELL_PROCESS_GROUPS,
        }
    }

    pub(in crate::handler) fn register_spawned(
        self: &Arc<Self>,
        child: &mut ProcessTreeChild,
    ) -> Result<ShellProcessGuard, ShellProcessRegistrationError> {
        let registration = self.register_controller(child.controller());
        if registration.is_err() {
            terminate_and_reap_shell_process(child);
        }
        registration
    }

    fn register_controller(
        self: &Arc<Self>,
        controller: ProcessTreeController,
    ) -> Result<ShellProcessGuard, ShellProcessRegistrationError> {
        let mut inner = self
            .inner
            .lock()
            .expect("shell process registry mutex must not be poisoned");
        if inner.closing {
            return Err(ShellProcessRegistrationError::Closing);
        }
        if inner.processes.len() >= self.limit {
            return Err(ShellProcessRegistrationError::LimitReached { limit: self.limit });
        }

        let id = inner.next_id;
        inner.next_id = inner.next_id.wrapping_add(1);
        inner.processes.insert(id, controller);
        Ok(ShellProcessGuard {
            id,
            registry: self.clone(),
        })
    }

    /// Rejects new registrations and takes every registered tree, which the
    /// returned shutdown force-terminates even if it is dropped unsettled.
    pub(in crate::handler) fn close(&self) -> ShellProcessShutdown {
        let mut inner = self
            .inner
            .lock()
            .expect("shell process registry mutex must not be poisoned");
        inner.closing = true;
        self.closing.store(true, Ordering::SeqCst);
        ShellProcessShutdown {
            controllers: inner
                .processes
                .drain()
                .map(|(_, controller)| controller)
                .collect(),
        }
    }

    fn unregister(&self, id: u64) {
        self.inner
            .lock()
            .expect("shell process registry mutex must not be poisoned")
            .processes
            .remove(&id);
    }
}

pub(in crate::handler) fn terminate_and_reap_shell_process(child: &mut ProcessTreeChild) {
    // `wait` disarms the process-group or Job Object identity. Settle every
    // descendant first so shutdown cannot return while a child is still live.
    let tree_stopped = child
        .controller()
        .terminate_and_wait(SHELL_PROCESS_FORCE_TERMINATION_WAIT)
        .unwrap_or(false);
    if !tree_stopped {
        let _ = child.terminate();
    }
    let _ = child.wait();
}

/// The trees a closed registry still owned, awaiting force-termination.
pub(in crate::handler) struct ShellProcessShutdown {
    controllers: Vec<ProcessTreeController>,
}

impl ShellProcessShutdown {
    /// Force-terminates every tree and reports whether all of them stopped
    /// within the shared shutdown grace. Blocks, so async callers run it off
    /// their worker.
    pub(in crate::handler) fn terminate(mut self) -> bool {
        let deadline = Instant::now() + SHELL_PROCESS_SHUTDOWN_GRACE;
        let mut all_stopped = true;
        while let Some(controller) = self.controllers.pop() {
            // On Linux the group identity stays locked from SIGKILL through the
            // descendant observation. The copy-pipe worker must not reap the
            // leader and disarm it while a descendant is still live.
            all_stopped &= controller.terminate_for_shutdown(deadline);
        }
        all_stopped
    }
}

impl Drop for ShellProcessShutdown {
    fn drop(&mut self) {
        // A shutdown worker that never ran still must not leave a tree running.
        for controller in self.controllers.drain(..) {
            let _ = controller.terminate();
        }
    }
}

impl Default for ShellProcessRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ShellProcessRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner.lock().map_err(|_| fmt::Error)?;
        formatter
            .debug_struct("ShellProcessRegistry")
            .field("closing", &inner.closing)
            .field("active_processes", &inner.processes.len())
            .field("limit", &self.limit)
            .finish()
    }
}

impl ShellProcessGuard {
    pub(in crate::handler) fn shutdown_started(&self) -> bool {
        self.registry.closing.load(Ordering::SeqCst)
    }
}

impl Drop for ShellProcessGuard {
    fn drop(&mut self) {
        self.registry.unregister(self.id);
    }
}
