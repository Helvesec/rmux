use std::collections::{HashMap, HashSet};

use rmux_proto::{TerminalSize, WindowId};

/// Declarations belong to one control registration, never to a session or slot.
/// Remember removals until destruction so repeated `@id:` echoes remain idempotent.
#[derive(Debug, Default, Clone)]
pub(in crate::handler) struct ControlWindowSizes {
    declared: bool,
    windows: HashMap<WindowId, Option<TerminalSize>>,
}

impl ControlWindowSizes {
    pub(in crate::handler) fn declared(&self) -> bool {
        self.declared
    }

    pub(in crate::handler) fn get(&self, id: WindowId) -> Option<TerminalSize> {
        self.windows.get(&id).copied().flatten()
    }

    pub(in crate::handler) fn set(&mut self, id: WindowId, size: TerminalSize) {
        self.declared = true;
        self.windows.insert(id, Some(size));
    }

    pub(in crate::handler) fn remove(&mut self, id: WindowId) -> bool {
        if let Some(size) = self.windows.get_mut(&id) {
            *size = None;
            true
        } else {
            false
        }
    }

    pub(in crate::handler) fn retain_live(&mut self, live: &HashSet<WindowId>) {
        self.windows.retain(|id, _| live.contains(id));
    }
}
