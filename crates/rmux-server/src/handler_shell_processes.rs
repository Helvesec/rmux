use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use rmux_os::process_tree::ProcessTreeController;

const MAX_SHELL_PROCESS_GROUPS: usize = 1024;

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

    pub(in crate::handler) fn register(
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

    pub(in crate::handler) fn close_and_terminate(&self) {
        let controllers = {
            let mut inner = self
                .inner
                .lock()
                .expect("shell process registry mutex must not be poisoned");
            inner.closing = true;
            self.closing.store(true, Ordering::SeqCst);
            inner
                .processes
                .drain()
                .map(|(_, controller)| controller)
                .collect::<Vec<_>>()
        };

        for controller in controllers {
            let _ = controller.terminate();
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
