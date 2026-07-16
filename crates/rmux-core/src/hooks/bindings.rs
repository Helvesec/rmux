use std::collections::{BTreeMap, HashMap};

use rmux_proto::{HookLifecycle, HookName};

use super::rules::hook_inventory;
use super::types::{HookBindingView, HookDispatch, HookSetOptions};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct HookBindings {
    hooks: HashMap<HookName, HookArray>,
}

impl HookBindings {
    pub(super) fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub(super) fn set(
        &mut self,
        hook: HookName,
        command: String,
        lifecycle: HookLifecycle,
        options: HookSetOptions,
    ) -> u32 {
        let array = self.hooks.entry(hook).or_default();
        let registered = RegisteredHook { command, lifecycle };
        array.set(registered, options)
    }

    pub(super) fn unset(&mut self, hook: HookName, index: Option<u32>) {
        let remove_hook = if let Some(array) = self.hooks.get_mut(&hook) {
            array.unset(index);
            array.is_empty()
        } else {
            false
        };
        if remove_hook {
            self.hooks.remove(&hook);
        }
    }

    pub(super) fn command(&self, hook: HookName) -> Option<&str> {
        self.hooks.get(&hook).and_then(HookArray::command)
    }

    pub(super) fn command_at(&self, hook: HookName, index: u32) -> Option<&str> {
        self.hooks
            .get(&hook)
            .and_then(|array| array.command_at(index))
    }

    pub(super) fn lifecycle(&self, hook: HookName) -> Option<HookLifecycle> {
        self.hooks.get(&hook).and_then(HookArray::lifecycle)
    }

    pub(super) fn lifecycle_at(&self, hook: HookName, index: u32) -> Option<HookLifecycle> {
        self.hooks
            .get(&hook)
            .and_then(|array| array.lifecycle_at(index))
    }

    pub(super) fn dispatch(&mut self, hook: HookName) -> Vec<HookDispatch> {
        let (dispatches, remove_hook) = if let Some(array) = self.hooks.get_mut(&hook) {
            let dispatches = array.dispatch();
            let should_remove = array.is_empty();
            (dispatches, should_remove)
        } else {
            (Vec::new(), false)
        };
        if remove_hook {
            self.hooks.remove(&hook);
        }
        dispatches
    }

    pub(super) fn one_shot_indices(&self, hook: HookName) -> Vec<u32> {
        self.hooks.get(&hook).map_or_else(Vec::new, |array| {
            array
                .entries
                .iter()
                .filter_map(|(index, registered)| {
                    (registered.lifecycle == HookLifecycle::OneShot).then_some(*index)
                })
                .collect()
        })
    }

    pub(super) fn views(&self, filter: Option<HookName>) -> Vec<HookBindingView> {
        hook_inventory()
            .into_iter()
            .filter(|hook| filter.map(|expected| *hook == expected).unwrap_or(true))
            .flat_map(|hook| {
                self.hooks
                    .get(&hook)
                    .into_iter()
                    .flat_map(move |array| array.views(hook))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct HookArray {
    entries: BTreeMap<u32, RegisteredHook>,
}

impl HookArray {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn set(&mut self, registered: RegisteredHook, options: HookSetOptions) -> u32 {
        match (options.append, options.index) {
            (true, None) => {
                let index = self.next_index();
                self.entries.insert(index, registered);
                index
            }
            (true, Some(index)) | (false, Some(index)) => {
                self.entries.insert(index, registered);
                index
            }
            (false, None) => {
                self.entries.clear();
                self.entries.insert(0, registered);
                0
            }
        }
    }

    fn unset(&mut self, index: Option<u32>) {
        match index {
            Some(index) => {
                self.entries.remove(&index);
            }
            None => self.entries.clear(),
        }
    }

    fn command(&self) -> Option<&str> {
        self.entries
            .values()
            .next()
            .map(|registered| registered.command.as_str())
    }

    fn command_at(&self, index: u32) -> Option<&str> {
        self.entries
            .get(&index)
            .map(|registered| registered.command.as_str())
    }

    fn lifecycle(&self) -> Option<HookLifecycle> {
        self.entries
            .values()
            .next()
            .map(|registered| registered.lifecycle)
    }

    fn lifecycle_at(&self, index: u32) -> Option<HookLifecycle> {
        self.entries
            .get(&index)
            .map(|registered| registered.lifecycle)
    }

    fn dispatch(&mut self) -> Vec<HookDispatch> {
        let mut dispatches = Vec::with_capacity(self.entries.len());
        let mut one_shots = Vec::new();

        for (index, registered) in &self.entries {
            dispatches.push(HookDispatch {
                command: registered.command.clone(),
                lifecycle: registered.lifecycle,
            });
            if registered.lifecycle == HookLifecycle::OneShot {
                one_shots.push(*index);
            }
        }

        for index in one_shots {
            self.entries.remove(&index);
        }

        dispatches
    }

    fn next_index(&self) -> u32 {
        match self.entries.keys().next_back() {
            None => 0,
            Some(&max_key) => {
                if let Some(next) = max_key.checked_add(1) {
                    return next;
                }
                // Max key is u32::MAX - scan for the first unused index.
                (0..u32::MAX)
                    .find(|index| !self.entries.contains_key(index))
                    .unwrap_or(u32::MAX)
            }
        }
    }

    fn views(&self, hook: HookName) -> Vec<HookBindingView> {
        self.entries
            .iter()
            .map(|(index, registered)| HookBindingView {
                hook,
                index: *index,
                command: registered.command.clone(),
                lifecycle: registered.lifecycle,
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredHook {
    command: String,
    lifecycle: HookLifecycle,
}
