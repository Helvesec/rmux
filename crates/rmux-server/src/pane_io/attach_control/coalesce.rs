use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::super::types::{AttachTarget, OverlayFrame};
use super::{
    release_attach_control_backlog, reserve_attach_control_backlog, reserve_pending_deep_switch,
};

#[derive(Debug)]
pub(crate) struct PendingDeepSwitchReservation {
    count: Arc<AtomicUsize>,
}

impl PendingDeepSwitchReservation {
    pub(super) fn new(count: Arc<AtomicUsize>) -> Self {
        Self { count }
    }
}

impl Drop for PendingDeepSwitchReservation {
    fn drop(&mut self) {
        let _ = self
            .count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_sub(1)
            });
    }
}

#[derive(Debug)]
pub(crate) struct CoalescedAttachTarget {
    state: Mutex<CoalescedAttachTargetState>,
    backlog: Arc<AtomicUsize>,
    deep_count: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct CoalescedAttachTargetState {
    target: Option<Box<AttachTarget>>,
    units: usize,
    switch_count: u64,
    deep_reservation: Option<PendingDeepSwitchReservation>,
}

pub(super) enum CoalescedAttachTargetReplace {
    Replaced,
    Consumed(Box<AttachTarget>),
    Full(Box<AttachTarget>),
}

impl CoalescedAttachTarget {
    pub(super) fn new(
        target: Box<AttachTarget>,
        units: usize,
        backlog: Arc<AtomicUsize>,
        deep_count: Arc<AtomicUsize>,
        deep_reservation: Option<PendingDeepSwitchReservation>,
    ) -> Self {
        Self {
            state: Mutex::new(CoalescedAttachTargetState {
                target: Some(target),
                units,
                switch_count: 1,
                deep_reservation,
            }),
            backlog,
            deep_count,
        }
    }

    pub(super) fn with_target<T>(&self, inspect: impl FnOnce(&AttachTarget) -> T) -> Option<T> {
        let state = self
            .state
            .lock()
            .expect("coalesced attach target mutex must not be poisoned");
        state.target.as_deref().map(inspect)
    }

    pub(super) fn replace(
        &self,
        target: Box<AttachTarget>,
        units: usize,
        backlog_limit: usize,
    ) -> CoalescedAttachTargetReplace {
        let mut state = self
            .state
            .lock()
            .expect("coalesced attach target mutex must not be poisoned");
        if state.target.is_none() {
            return CoalescedAttachTargetReplace::Consumed(target);
        }

        let needs_deep_reservation = target.live_pane.is_some();
        let added_deep_reservation = if needs_deep_reservation && state.deep_reservation.is_none() {
            if !reserve_pending_deep_switch(&self.deep_count) {
                return CoalescedAttachTargetReplace::Full(target);
            }
            Some(PendingDeepSwitchReservation::new(Arc::clone(
                &self.deep_count,
            )))
        } else {
            None
        };
        let previous_units = state.units;
        if units > previous_units
            && !reserve_attach_control_backlog(
                &self.backlog,
                units.saturating_sub(previous_units),
                backlog_limit,
            )
        {
            drop(added_deep_reservation);
            return CoalescedAttachTargetReplace::Full(target);
        }

        let previous_target = state.target.replace(target);
        state.units = units;
        state.switch_count = state.switch_count.saturating_add(1);
        let released_deep_reservation = if needs_deep_reservation {
            if state.deep_reservation.is_none() {
                state.deep_reservation = added_deep_reservation;
            }
            None
        } else {
            state.deep_reservation.take()
        };
        drop(state);
        if previous_units > units {
            release_attach_control_backlog(&self.backlog, previous_units - units);
        }
        drop(previous_target);
        drop(released_deep_reservation);
        CoalescedAttachTargetReplace::Replaced
    }

    pub(super) fn take(&self) -> Option<(Box<AttachTarget>, u64)> {
        let (target, units, switch_count, deep_reservation) = {
            let mut state = self
                .state
                .lock()
                .expect("coalesced attach target mutex must not be poisoned");
            (
                state.target.take(),
                std::mem::take(&mut state.units),
                std::mem::take(&mut state.switch_count),
                state.deep_reservation.take(),
            )
        };
        release_attach_control_backlog(&self.backlog, units);
        drop(deep_reservation);
        target.map(|target| (target, switch_count))
    }
}

impl Drop for CoalescedAttachTarget {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .expect("coalesced attach target mutex must not be poisoned");
        release_attach_control_backlog(&self.backlog, std::mem::take(&mut state.units));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PopupOverlayKey {
    popup_id: u64,
    render_generation: u64,
}

impl PopupOverlayKey {
    pub(super) fn new(popup_id: u64, render_generation: u64) -> Self {
        Self {
            popup_id,
            render_generation,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CoalescedPopupOverlay {
    key: PopupOverlayKey,
    state: Mutex<CoalescedPopupOverlayState>,
    backlog: Arc<AtomicUsize>,
    #[cfg(test)]
    replace_capacity_high_water: AtomicUsize,
}

#[derive(Debug)]
struct CoalescedPopupOverlayState {
    overlay: Option<OverlayFrame>,
    units: usize,
}

pub(super) enum CoalescedPopupOverlayReplace {
    Replaced,
    IgnoredOlder,
    Consumed(OverlayFrame),
    Full(OverlayFrame),
}

impl CoalescedPopupOverlay {
    pub(super) fn new(
        key: PopupOverlayKey,
        overlay: OverlayFrame,
        units: usize,
        backlog: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            key,
            state: Mutex::new(CoalescedPopupOverlayState {
                overlay: Some(overlay),
                units,
            }),
            backlog,
            #[cfg(test)]
            replace_capacity_high_water: AtomicUsize::new(0),
        }
    }

    pub(super) fn matches(&self, key: PopupOverlayKey) -> bool {
        self.key == key
    }

    pub(super) fn replace(
        &self,
        overlay: OverlayFrame,
        units: usize,
        backlog_limit: usize,
    ) -> CoalescedPopupOverlayReplace {
        let mut state = self
            .state
            .lock()
            .expect("coalesced popup overlay mutex must not be poisoned");
        let Some(current) = state.overlay.as_ref() else {
            return CoalescedPopupOverlayReplace::Consumed(overlay);
        };
        // Generations are assigned before the independently serialized send,
        // so cloned producers can reach this slot in the opposite order.
        if overlay.overlay_generation < current.overlay_generation {
            return CoalescedPopupOverlayReplace::IgnoredOlder;
        }

        #[cfg(test)]
        self.replace_capacity_high_water.fetch_max(
            current.frame.capacity() + overlay.frame.capacity(),
            Ordering::Relaxed,
        );
        let previous_units = state.units;
        if units > previous_units
            && !reserve_attach_control_backlog(
                &self.backlog,
                units.saturating_sub(previous_units),
                backlog_limit,
            )
        {
            return CoalescedPopupOverlayReplace::Full(overlay);
        }

        let previous_overlay = state.overlay.replace(overlay);
        state.units = units;
        drop(state);
        if previous_units > units {
            release_attach_control_backlog(&self.backlog, previous_units - units);
        }
        drop(previous_overlay);
        CoalescedPopupOverlayReplace::Replaced
    }

    pub(super) fn take(&self) -> Option<OverlayFrame> {
        let (overlay, units) = {
            let mut state = self
                .state
                .lock()
                .expect("coalesced popup overlay mutex must not be poisoned");
            (state.overlay.take(), std::mem::take(&mut state.units))
        };
        overlay.map(|mut overlay| {
            // Transfer, rather than release, the reservation: the actual attach
            // writer may remain blocked while the sender replaces the next frame.
            overlay.backlog_reservation = Some(super::AttachControlBacklogReservation {
                backlog: Arc::clone(&self.backlog),
                units,
            });
            overlay
        })
    }

    #[cfg(test)]
    pub(super) fn retained_allocation_for_test(&self) -> (usize, usize, usize, usize) {
        let state = self.state.lock().unwrap();
        let (len, capacity) = state
            .overlay
            .as_ref()
            .map(|overlay| (overlay.frame.len(), overlay.frame.capacity()))
            .unwrap_or_default();
        (
            len,
            capacity,
            state.units,
            self.replace_capacity_high_water.load(Ordering::Relaxed),
        )
    }

    pub(super) fn retained_frame_len(&self) -> usize {
        let state = self
            .state
            .lock()
            .expect("coalesced popup overlay mutex must not be poisoned");
        state
            .overlay
            .as_ref()
            .map(|overlay| overlay.frame.len())
            .unwrap_or_default()
    }
}

impl Drop for CoalescedPopupOverlay {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .expect("coalesced popup overlay mutex must not be poisoned");
        release_attach_control_backlog(&self.backlog, std::mem::take(&mut state.units));
    }
}
