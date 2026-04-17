use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use rmux_proto::RmuxError;
use tokio::sync::oneshot;

#[derive(Debug, Default)]
pub(crate) struct WaitForStore {
    channels: HashMap<String, WaitForChannel>,
    next_waiter_id: u64,
    shutting_down: bool,
}

#[derive(Debug, Default)]
struct WaitForChannel {
    signal_waiters: VecDeque<Waiter>,
    locked: bool,
    woken: bool,
    lock_waiters: VecDeque<Waiter>,
    granted_lock_waiter: Option<u64>,
}

#[derive(Debug)]
struct Waiter {
    id: u64,
    wake: oneshot::Sender<WaitForWake>,
}

#[derive(Debug)]
pub(crate) enum WaitForRegistration {
    Ready,
    Shutdown,
    Waiting {
        channel: String,
        waiter_id: u64,
        receiver: oneshot::Receiver<WaitForWake>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitForWake {
    Ready,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WaitForWaiterKind {
    Signal,
    Lock,
}

pub(crate) struct WaitForCleanupGuard<'a> {
    store: &'a Mutex<WaitForStore>,
    channel: String,
    waiter_id: u64,
    kind: WaitForWaiterKind,
    active: bool,
}

impl<'a> WaitForCleanupGuard<'a> {
    pub(crate) fn new(
        store: &'a Mutex<WaitForStore>,
        channel: String,
        waiter_id: u64,
        kind: WaitForWaiterKind,
    ) -> Self {
        Self {
            store,
            channel,
            waiter_id,
            kind,
            active: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for WaitForCleanupGuard<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        let Ok(mut store) = self.store.lock() else {
            return;
        };

        match self.kind {
            WaitForWaiterKind::Signal => {
                store.cancel_signal_waiter(&self.channel, self.waiter_id);
            }
            WaitForWaiterKind::Lock => {
                store.cancel_lock_waiter(&self.channel, self.waiter_id);
            }
        }
    }
}

impl WaitForStore {
    pub(crate) fn register_wait(&mut self, channel: String) -> WaitForRegistration {
        if self.shutting_down {
            return WaitForRegistration::Shutdown;
        }

        if self
            .channels
            .get_mut(&channel)
            .is_some_and(|state| state.woken)
        {
            if let Some(state) = self.channels.get_mut(&channel) {
                state.woken = false;
            }
            self.remove_idle_channel(&channel);
            return WaitForRegistration::Ready;
        }

        let (waiter, receiver) = self.new_waiter();
        let waiter_id = waiter.id;
        self.channels
            .entry(channel.clone())
            .or_default()
            .signal_waiters
            .push_back(waiter);

        WaitForRegistration::Waiting {
            channel,
            waiter_id,
            receiver,
        }
    }

    pub(crate) fn signal(&mut self, channel: &str) -> Result<(), RmuxError> {
        if self.shutting_down {
            return Err(RmuxError::Server(
                "wait-for store is shutting down".to_owned(),
            ));
        }

        let Some(state) = self.channels.get_mut(channel) else {
            let state = self.channels.entry(channel.to_owned()).or_default();
            state.woken = true;
            return Ok(());
        };

        if state.signal_waiters.is_empty() && !state.woken {
            state.woken = true;
            return Ok(());
        }

        for waiter in state.signal_waiters.drain(..) {
            let _ = waiter.wake.send(WaitForWake::Ready);
        }
        self.remove_idle_channel(channel);
        Ok(())
    }

    pub(crate) fn register_lock(&mut self, channel: String) -> WaitForRegistration {
        if self.shutting_down {
            return WaitForRegistration::Shutdown;
        }

        if !self
            .channels
            .get(&channel)
            .is_some_and(|state| state.locked)
        {
            self.channels.entry(channel.clone()).or_default().locked = true;
            return WaitForRegistration::Ready;
        }

        let (waiter, receiver) = self.new_waiter();
        let waiter_id = waiter.id;
        self.channels
            .entry(channel.clone())
            .or_default()
            .lock_waiters
            .push_back(waiter);

        WaitForRegistration::Waiting {
            channel,
            waiter_id,
            receiver,
        }
    }

    pub(crate) fn accept_lock(&mut self, channel: &str, waiter_id: u64) -> bool {
        if let Some(state) = self.channels.get_mut(channel) {
            if state.granted_lock_waiter == Some(waiter_id) {
                state.granted_lock_waiter = None;
                return true;
            }
        }

        false
    }

    pub(crate) fn unlock(&mut self, channel: &str) -> Result<(), RmuxError> {
        if self.shutting_down {
            return Err(RmuxError::Server(
                "wait-for store is shutting down".to_owned(),
            ));
        }

        // Cluster I: tmux emits `channel {name} not locked` to stderr with no
        // prefix, so the user-facing wording goes through `RmuxError::Message`
        // (bare) rather than `RmuxError::Server` (prefixed).
        let Some(state) = self.channels.get_mut(channel) else {
            return Err(RmuxError::Message(format!("channel {channel} not locked")));
        };
        if !state.locked {
            return Err(RmuxError::Message(format!("channel {channel} not locked")));
        }

        if !grant_next_lock_waiter(state) {
            state.locked = false;
            state.granted_lock_waiter = None;
        }
        self.remove_idle_channel(channel);
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) {
        self.shutting_down = true;
        for state in self.channels.values_mut() {
            for waiter in state.signal_waiters.drain(..) {
                let _ = waiter.wake.send(WaitForWake::Shutdown);
            }
            state.woken = true;
            for waiter in state.lock_waiters.drain(..) {
                let _ = waiter.wake.send(WaitForWake::Shutdown);
            }
            state.locked = false;
            state.granted_lock_waiter = None;
        }
        self.channels.clear();
    }

    #[cfg(test)]
    pub(crate) fn waiter_counts(&self, channel: &str) -> (usize, usize, bool) {
        self.channels
            .get(channel)
            .map(|state| {
                (
                    state.signal_waiters.len(),
                    state.lock_waiters.len(),
                    state.locked || state.woken,
                )
            })
            .unwrap_or((0, 0, false))
    }

    fn new_waiter(&mut self) -> (Waiter, oneshot::Receiver<WaitForWake>) {
        let id = self.next_waiter_id;
        self.next_waiter_id = self.next_waiter_id.wrapping_add(1);
        let (wake, receiver) = oneshot::channel();
        (Waiter { id, wake }, receiver)
    }

    fn cancel_signal_waiter(&mut self, channel: &str, waiter_id: u64) {
        if let Some(state) = self.channels.get_mut(channel) {
            remove_waiter(&mut state.signal_waiters, waiter_id);
        }
        self.remove_idle_channel(channel);
    }

    fn cancel_lock_waiter(&mut self, channel: &str, waiter_id: u64) {
        if let Some(state) = self.channels.get_mut(channel) {
            if remove_waiter(&mut state.lock_waiters, waiter_id) {
                self.remove_idle_channel(channel);
                return;
            }

            if state.granted_lock_waiter == Some(waiter_id) {
                state.granted_lock_waiter = None;
                if !grant_next_lock_waiter(state) {
                    state.locked = false;
                }
            }
        }
        self.remove_idle_channel(channel);
    }

    fn remove_idle_channel(&mut self, channel: &str) {
        let should_remove = self.channels.get(channel).is_some_and(|state| {
            !state.locked
                && !state.woken
                && state.signal_waiters.is_empty()
                && state.lock_waiters.is_empty()
                && state.granted_lock_waiter.is_none()
        });

        if should_remove {
            self.channels.remove(channel);
        }
    }
}

fn remove_waiter(waiters: &mut VecDeque<Waiter>, waiter_id: u64) -> bool {
    let Some(index) = waiters.iter().position(|waiter| waiter.id == waiter_id) else {
        return false;
    };

    waiters.remove(index);
    true
}

fn grant_next_lock_waiter(state: &mut WaitForChannel) -> bool {
    while let Some(waiter) = state.lock_waiters.pop_front() {
        let waiter_id = waiter.id;
        if waiter.wake.send(WaitForWake::Ready).is_ok() {
            state.locked = true;
            state.granted_lock_waiter = Some(waiter_id);
            return true;
        }
    }

    false
}
