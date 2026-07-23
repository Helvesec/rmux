use rmux_proto::AttachShellCommand;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::mpsc;

use super::types::{AttachTarget, OverlayFrame};

#[derive(Debug)]
pub(crate) enum AttachControl {
    Detach,
    Exited,
    DetachKill,
    DetachExecShellCommand(AttachShellCommand),
    InteractiveInput,
    Refresh,
    Switch(QueuedAttachTarget),
    AdvancePersistentOverlayState(u64),
    Overlay(OverlayFrame),
    Write(Vec<u8>),
    ClipboardWrite {
        bytes: Vec<u8>,
        reservation: Option<AttachControlBacklogReservation>,
    },
    LockShellCommand(AttachShellCommand),
    Suspend,
}

impl AttachControl {
    /// One unit in the shared attach-control memory budget.
    ///
    /// The queue remains message-based, but every producer charges the bytes
    /// retained by its control before enqueueing it. Small controls still cost
    /// one unit, which also bounds the number of zero-payload controls.
    pub(crate) const BACKLOG_UNIT_BYTES: usize = 128 * 1024;

    pub(crate) fn switch(target: AttachTarget) -> Self {
        Self::Switch(QueuedAttachTarget::Direct(Box::new(target)))
    }

    pub(crate) fn is_coalescible_render_switch(&self) -> bool {
        matches!(self, Self::Switch(target) if target.is_coalescible_render_refresh())
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Detach | Self::Exited | Self::DetachKill | Self::DetachExecShellCommand(_)
        )
    }

    pub(crate) fn backlog_units(&self) -> usize {
        let retained_bytes = std::mem::size_of::<Self>().saturating_add(match self {
            Self::Detach
            | Self::Exited
            | Self::DetachKill
            | Self::InteractiveInput
            | Self::Refresh
            | Self::AdvancePersistentOverlayState(_)
            | Self::Suspend => 0,
            Self::DetachExecShellCommand(command) | Self::LockShellCommand(command) => command
                .command()
                .len()
                .saturating_add(command.shell().len())
                .saturating_add(command.cwd().len()),
            // The frame determines the weighted byte charge. A live target
            // also owns a deep renderer snapshot whose size is not cheaply
            // measurable; consecutive replaceable switches are therefore
            // retained in one sender-side slot below.
            Self::Switch(target) => target.retained_render_frame_len(),
            Self::Overlay(overlay) => overlay.frame.len(),
            Self::Write(bytes) | Self::ClipboardWrite { bytes, .. } => bytes.len(),
        });
        retained_bytes.div_ceil(Self::BACKLOG_UNIT_BYTES).max(1)
    }

    fn with_backlog_reservation(self, backlog: Arc<AtomicUsize>, units: usize) -> Self {
        match self {
            Self::ClipboardWrite { bytes, .. } => Self::ClipboardWrite {
                bytes,
                reservation: Some(AttachControlBacklogReservation { backlog, units }),
            },
            other => other,
        }
    }

    pub(crate) fn received_backlog_units(&self) -> usize {
        match self {
            // Clipboard controls can remain in deferred queues. Their RAII
            // reservation follows the bytes until emission or drop, so the
            // generic receiver must not release them a second time.
            Self::ClipboardWrite {
                reservation: Some(_),
                ..
            } => 0,
            // Deep switch reservations remain live while a target sits in a
            // deferred queue, and release on materialization or drop.
            Self::Switch(
                QueuedAttachTarget::Coalesced(_) | QueuedAttachTarget::RetainedDeep { .. },
            ) => 0,
            _ => self.backlog_units(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum QueuedAttachTarget {
    Direct(Box<AttachTarget>),
    Coalesced(Arc<CoalescedAttachTarget>),
    RetainedDeep {
        target: Box<AttachTarget>,
        _deep_reservation: PendingDeepSwitchReservation,
        _backlog_reservation: AttachControlBacklogReservation,
    },
}

impl QueuedAttachTarget {
    pub(crate) fn is_coalescible_render_refresh(&self) -> bool {
        self.with_target(AttachTarget::is_coalescible_render_refresh)
            .unwrap_or(false)
    }

    pub(crate) fn with_target<T>(&self, inspect: impl FnOnce(&AttachTarget) -> T) -> Option<T> {
        match self {
            Self::Direct(target) => Some(inspect(target)),
            Self::Coalesced(slot) => {
                let state = slot
                    .state
                    .lock()
                    .expect("coalesced attach target mutex must not be poisoned");
                state.target.as_deref().map(inspect)
            }
            Self::RetainedDeep { target, .. } => Some(inspect(target)),
        }
    }

    #[cfg(test)]
    pub(crate) fn into_target(self) -> Box<AttachTarget> {
        self.into_target_with_count().0
    }

    pub(crate) fn into_target_with_count(self) -> (Box<AttachTarget>, u64) {
        match self {
            Self::Direct(target) => (target, 1),
            Self::Coalesced(slot) => slot
                .take()
                .expect("a queued coalesced attach target must be consumed exactly once"),
            Self::RetainedDeep { target, .. } => (target, 1),
        }
    }

    fn retained_render_frame_len(&self) -> usize {
        self.with_target(|target| target.render_frame.len())
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub(crate) struct PendingDeepSwitchReservation {
    count: Arc<AtomicUsize>,
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

enum CoalescedAttachTargetReplace {
    Replaced,
    Consumed(Box<AttachTarget>),
    Full(Box<AttachTarget>),
}

impl CoalescedAttachTarget {
    fn new(
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

    fn replace(
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
            Some(PendingDeepSwitchReservation {
                count: Arc::clone(&self.deep_count),
            })
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

    fn take(&self) -> Option<(Box<AttachTarget>, u64)> {
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

#[derive(Debug)]
pub(crate) struct AttachControlBacklogReservation {
    backlog: Arc<AtomicUsize>,
    units: usize,
}

impl Drop for AttachControlBacklogReservation {
    fn drop(&mut self) {
        release_attach_control_backlog(&self.backlog, self.units);
    }
}

/// The only production enqueue path for an active attached client.
///
/// Reserving here makes every control variant share one race-free byte budget;
/// producers cannot accidentally bypass accounting when a new payload-bearing
/// control is added.
#[derive(Clone, Debug)]
pub(crate) struct AttachControlSender {
    inner: mpsc::UnboundedSender<AttachControl>,
    backlog: Arc<AtomicUsize>,
    backlog_limit: usize,
    closing: Arc<AtomicBool>,
    send_state: Arc<Mutex<AttachControlSendState>>,
    pending_deep_switches: Arc<AtomicUsize>,
}

#[derive(Debug, Default)]
struct AttachControlSendState {
    open_coalesced_switch: Option<Weak<CoalescedAttachTarget>>,
}

#[derive(Debug)]
pub(crate) enum AttachControlSendError {
    Full,
    Closed,
}

impl AttachControlSendError {
    pub(crate) const fn is_full(&self) -> bool {
        matches!(self, Self::Full)
    }
}

impl AttachControlSender {
    /// Non-replaceable switches preserve semantic ordering, so they cannot use
    /// the latest-value slot. Bound their deep live-render snapshots with a
    /// small independent count instead of pretending their heap size can be
    /// derived from the rendered frame.
    const MAX_PENDING_DEEP_SWITCHES: usize = 4;

    pub(crate) fn new(
        inner: mpsc::UnboundedSender<AttachControl>,
        backlog: Arc<AtomicUsize>,
        backlog_limit: usize,
        closing: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner,
            backlog,
            backlog_limit,
            closing,
            send_state: Arc::new(Mutex::new(AttachControlSendState::default())),
            pending_deep_switches: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(crate) fn send(&self, control: AttachControl) -> Result<(), AttachControlSendError> {
        // Tokio preserves sender-local ordering, but attached controls have
        // many cloned producers. Serializing the short enqueue section gives
        // coalesced switch slots an explicit cross-producer ordering boundary.
        let mut send_state = self
            .send_state
            .lock()
            .expect("attach control send-state mutex must not be poisoned");
        if self.closing.load(Ordering::SeqCst) {
            return Err(AttachControlSendError::Closed);
        }
        if self.inner.is_closed() {
            // Closing the receiver drops every queued control at once. No
            // reservation can still correspond to retained queue memory.
            self.closing.store(true, Ordering::SeqCst);
            self.backlog.store(0, Ordering::Release);
            send_state.open_coalesced_switch = None;
            return Err(AttachControlSendError::Closed);
        }

        let control = match control {
            AttachControl::Switch(QueuedAttachTarget::Direct(target))
                if target.is_coalescible_render_refresh() =>
            {
                if let Some(slot) = send_state
                    .open_coalesced_switch
                    .as_ref()
                    .and_then(Weak::upgrade)
                {
                    let units = attach_switch_backlog_units(&target);
                    match slot.replace(target, units, self.backlog_limit) {
                        CoalescedAttachTargetReplace::Replaced => {
                            // The receiver can close after the optimistic
                            // check while this sender still keeps the slot Arc
                            // alive. Do not report a replacement into that
                            // orphaned slot as delivered.
                            if self.inner.is_closed() {
                                self.closing.store(true, Ordering::SeqCst);
                                self.backlog.store(0, Ordering::Release);
                                send_state.open_coalesced_switch = None;
                                return Err(AttachControlSendError::Closed);
                            }
                            return Ok(());
                        }
                        CoalescedAttachTargetReplace::Consumed(target) => {
                            send_state.open_coalesced_switch = None;
                            AttachControl::Switch(QueuedAttachTarget::Direct(target))
                        }
                        CoalescedAttachTargetReplace::Full(target) => {
                            send_state.open_coalesced_switch = None;
                            drop(target);
                            self.close_overloaded_attach();
                            return Err(AttachControlSendError::Full);
                        }
                    }
                } else {
                    send_state.open_coalesced_switch = None;
                    AttachControl::Switch(QueuedAttachTarget::Direct(target))
                }
            }
            other => {
                send_state.open_coalesced_switch = None;
                other
            }
        };
        let units = control.backlog_units();
        if !reserve_attach_control_backlog(&self.backlog, units, self.backlog_limit) {
            self.close_overloaded_attach();
            return Err(AttachControlSendError::Full);
        }

        let control = match control {
            AttachControl::Switch(QueuedAttachTarget::Direct(target))
                if target.is_coalescible_render_refresh() =>
            {
                let deep_reservation = if target.live_pane.is_some() {
                    if !reserve_pending_deep_switch(&self.pending_deep_switches) {
                        release_attach_control_backlog(&self.backlog, units);
                        self.close_overloaded_attach();
                        return Err(AttachControlSendError::Full);
                    }
                    Some(PendingDeepSwitchReservation {
                        count: Arc::clone(&self.pending_deep_switches),
                    })
                } else {
                    None
                };
                let slot = Arc::new(CoalescedAttachTarget::new(
                    target,
                    units,
                    Arc::clone(&self.backlog),
                    Arc::clone(&self.pending_deep_switches),
                    deep_reservation,
                ));
                send_state.open_coalesced_switch = Some(Arc::downgrade(&slot));
                AttachControl::Switch(QueuedAttachTarget::Coalesced(slot))
            }
            AttachControl::Switch(QueuedAttachTarget::Direct(target))
                if target.live_pane.is_some() =>
            {
                if !reserve_pending_deep_switch(&self.pending_deep_switches) {
                    release_attach_control_backlog(&self.backlog, units);
                    self.close_overloaded_attach();
                    return Err(AttachControlSendError::Full);
                }
                AttachControl::Switch(QueuedAttachTarget::RetainedDeep {
                    target,
                    _deep_reservation: PendingDeepSwitchReservation {
                        count: Arc::clone(&self.pending_deep_switches),
                    },
                    _backlog_reservation: AttachControlBacklogReservation {
                        backlog: Arc::clone(&self.backlog),
                        units,
                    },
                })
            }
            other => other.with_backlog_reservation(Arc::clone(&self.backlog), units),
        };
        if let Err(error) = self.inner.send(control) {
            // The receiver closed between the optimistic check and send. Its
            // queue has been dropped in full, so clear all reservations rather
            // than releasing only this attempted control.
            self.closing.store(true, Ordering::SeqCst);
            self.backlog.store(0, Ordering::Release);
            send_state.open_coalesced_switch = None;
            drop(error);
            return Err(AttachControlSendError::Closed);
        }
        Ok(())
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    fn close_overloaded_attach(&self) {
        if self
            .closing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        // Preserve the established explicit detach notification. It gets one
        // fixed-size unit of terminal headroom, and is accounted like every
        // other queued control so it cannot consume another control's
        // reservation when received. The closing latch guarantees there can
        // be only one such sentinel.
        let control = AttachControl::Detach;
        let units = control.backlog_units();
        self.backlog.fetch_add(units, Ordering::AcqRel);
        if self.inner.send(control).is_err() {
            // A closed receiver drops the entire queue, including the
            // terminal sentinel and every earlier reservation.
            self.backlog.store(0, Ordering::Release);
        }
    }
}

fn attach_switch_backlog_units(target: &AttachTarget) -> usize {
    std::mem::size_of::<AttachControl>()
        .saturating_add(target.render_frame.len())
        .div_ceil(AttachControl::BACKLOG_UNIT_BYTES)
        .max(1)
}

fn reserve_attach_control_backlog(
    backlog: &AtomicUsize,
    units: usize,
    backlog_limit: usize,
) -> bool {
    backlog
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(units)
                .filter(|next| *next <= backlog_limit)
        })
        .is_ok()
}

fn reserve_pending_deep_switch(count: &AtomicUsize) -> bool {
    count
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current
                .checked_add(1)
                .filter(|next| *next <= AttachControlSender::MAX_PENDING_DEEP_SWITCHES)
        })
        .is_ok()
}

pub(crate) fn release_attach_control_backlog(backlog: &AtomicUsize, units: usize) {
    if units == 0 {
        return;
    }
    let _ = backlog.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        current.checked_sub(units)
    });
}

#[cfg(test)]
mod tests;
