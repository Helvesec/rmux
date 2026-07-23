use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, Weak};

use rmux_core::{LifecycleEvent, PaneId, WindowId};
use rmux_proto::{PaneTarget, SessionId, Target, WindowTarget};

use crate::pane_terminals::{HandlerState, WindowLinkOccurrenceId};
use crate::terminal::TerminalProfile;

const PHASE_LIVE: u8 = 0;
const PHASE_RETIRED: u8 = 1;
const PHASE_INVALIDATED: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum RetainableLifecycleAnchor {
    Pane {
        session_id: SessionId,
        window_id: WindowId,
        pane_id: PaneId,
        occurrence_id: Option<WindowLinkOccurrenceId>,
        original: PaneTarget,
    },
    Window {
        session_id: SessionId,
        window_id: WindowId,
        occurrence_id: Option<WindowLinkOccurrenceId>,
        original: WindowTarget,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct RetainedTargetSnapshot {
    original: Target,
    terminal_profile: Option<TerminalProfile>,
}

impl RetainedTargetSnapshot {
    pub(in crate::handler) fn original(&self) -> &Target {
        &self.original
    }

    pub(in crate::handler) fn terminal_profile(&self) -> Option<&TerminalProfile> {
        self.terminal_profile.as_ref()
    }
}

#[derive(Debug)]
pub(in crate::handler) struct LifecycleTargetLease {
    anchor: RetainableLifecycleAnchor,
    snapshot: Arc<RetainedTargetSnapshot>,
    phase: AtomicU8,
    last_live_target: Mutex<Target>,
}

impl PartialEq for LifecycleTargetLease {
    fn eq(&self, other: &Self) -> bool {
        self.anchor == other.anchor
            && self.snapshot == other.snapshot
            && self.phase.load(Ordering::Acquire) == other.phase.load(Ordering::Acquire)
    }
}

impl Eq for LifecycleTargetLease {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) enum LeaseResolution {
    Live(Target),
    Retired(Arc<RetainedTargetSnapshot>),
    Replaced,
}

impl LifecycleTargetLease {
    fn new(anchor: RetainableLifecycleAnchor, snapshot: RetainedTargetSnapshot) -> Arc<Self> {
        let last_live_target = snapshot.original.clone();
        Arc::new(Self {
            anchor,
            snapshot: Arc::new(snapshot),
            phase: AtomicU8::new(PHASE_LIVE),
            last_live_target: Mutex::new(last_live_target),
        })
    }

    pub(in crate::handler) fn resolve(&self, state: &HandlerState) -> LeaseResolution {
        match self.phase.load(Ordering::Acquire) {
            PHASE_RETIRED => return LeaseResolution::Retired(Arc::clone(&self.snapshot)),
            PHASE_INVALIDATED => return LeaseResolution::Replaced,
            _ => {}
        }

        if let Some(target) = self.resolve_live(state) {
            return LeaseResolution::Live(target);
        }

        // Absence without a state-locked retirement witness is replacement or
        // an unsupported destructive path. It must never reacquire a later
        // object that happens to reuse the same numeric slot.
        let _ = self.phase.compare_exchange(
            PHASE_LIVE,
            PHASE_INVALIDATED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        match self.phase.load(Ordering::Acquire) {
            PHASE_RETIRED => LeaseResolution::Retired(Arc::clone(&self.snapshot)),
            _ => LeaseResolution::Replaced,
        }
    }

    fn resolve_live(&self, state: &HandlerState) -> Option<Target> {
        let target = match &self.anchor {
            RetainableLifecycleAnchor::Pane {
                session_id,
                window_id,
                pane_id,
                occurrence_id,
                original,
            } => resolve_pane_anchor(
                state,
                *session_id,
                *window_id,
                *pane_id,
                *occurrence_id,
                original,
            )
            .map(Target::Pane),
            RetainableLifecycleAnchor::Window {
                session_id,
                window_id,
                occurrence_id,
                original,
            } => resolve_window_anchor(state, *session_id, *window_id, *occurrence_id, original)
                .map(Target::Window),
        };
        if let Some(target) = target.as_ref() {
            *self
                .last_live_target
                .lock()
                .expect("retained lifecycle live target must not be poisoned") = target.clone();
        }
        target
    }

    fn retire_or_invalidate(&self, state: &HandlerState) {
        let next = if self.last_live_slot_was_replaced(state) {
            PHASE_INVALIDATED
        } else {
            PHASE_RETIRED
        };
        let _ = self
            .phase
            .compare_exchange(PHASE_LIVE, next, Ordering::AcqRel, Ordering::Acquire);
    }

    fn retire_stable_lifetime(&self) {
        let _ = self.phase.compare_exchange(
            PHASE_LIVE,
            PHASE_RETIRED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn last_live_slot_was_replaced(&self, state: &HandlerState) -> bool {
        let last_live_target = self
            .last_live_target
            .lock()
            .expect("retained lifecycle live target must not be poisoned")
            .clone();
        match (&self.anchor, last_live_target) {
            (
                RetainableLifecycleAnchor::Pane {
                    window_id, pane_id, ..
                },
                Target::Pane(last_live),
            ) => state
                .sessions
                .session(last_live.session_name())
                .and_then(|session| session.window_at(last_live.window_index()))
                .is_some_and(|window| {
                    window.id() != *window_id
                        || window
                            .pane(last_live.pane_index())
                            .is_some_and(|pane| pane.id() != *pane_id)
                }),
            (RetainableLifecycleAnchor::Window { window_id, .. }, Target::Window(last_live)) => {
                state
                    .sessions
                    .session(last_live.session_name())
                    .and_then(|session| session.window_at(last_live.window_index()))
                    .is_some_and(|window| window.id() != *window_id)
            }
            _ => true,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct RetainedLifecycleTargetRegistry {
    panes: HashMap<PaneId, Vec<Weak<LifecycleTargetLease>>>,
    windows: HashMap<WindowId, Vec<Weak<LifecycleTargetLease>>>,
}

impl RetainedLifecycleTargetRegistry {
    pub(in crate::handler) fn capture_pane(
        &mut self,
        state: &HandlerState,
        target: &PaneTarget,
    ) -> Option<Arc<LifecycleTargetLease>> {
        let session = state.sessions.session(target.session_name())?;
        let window = session.window_at(target.window_index())?;
        let pane = window.pane(target.pane_index())?;
        let pane_id = pane.id();
        let lease = LifecycleTargetLease::new(
            RetainableLifecycleAnchor::Pane {
                session_id: session.id(),
                window_id: window.id(),
                pane_id,
                occurrence_id: state
                    .window_link_occurrence_id(target.session_name(), target.window_index()),
                original: target.clone(),
            },
            RetainedTargetSnapshot {
                original: Target::Pane(target.clone()),
                terminal_profile: state
                    .pane_profile_in_window(
                        target.session_name(),
                        target.window_index(),
                        target.pane_index(),
                    )
                    .ok()
                    .cloned(),
            },
        );
        register_weak(&mut self.panes, pane_id, &lease);
        Some(lease)
    }

    pub(in crate::handler) fn capture_window(
        &mut self,
        state: &HandlerState,
        target: &WindowTarget,
    ) -> Option<Arc<LifecycleTargetLease>> {
        let session = state.sessions.session(target.session_name())?;
        let window = session.window_at(target.window_index())?;
        let window_id = window.id();
        let lease = LifecycleTargetLease::new(
            RetainableLifecycleAnchor::Window {
                session_id: session.id(),
                window_id,
                occurrence_id: state
                    .window_link_occurrence_id(target.session_name(), target.window_index()),
                original: target.clone(),
            },
            RetainedTargetSnapshot {
                original: Target::Window(target.clone()),
                terminal_profile: None,
            },
        );
        register_weak(&mut self.windows, window_id, &lease);
        Some(lease)
    }

    pub(in crate::handler) fn retire_absent(&mut self, state: &HandlerState) {
        retire_absent_from(&mut self.panes, state);
        retire_absent_from(&mut self.windows, state);
    }

    pub(in crate::handler) fn retire_panes(&mut self, pane_ids: &[PaneId]) {
        for pane_id in pane_ids {
            let Some(leases) = self.panes.remove(pane_id) else {
                continue;
            };
            for lease in leases.into_iter().filter_map(|lease| lease.upgrade()) {
                lease.retire_stable_lifetime();
            }
        }
    }
}

impl HandlerState {
    pub(in crate::handler) fn capture_retained_pane_lifecycle_target(
        &self,
        target: &PaneTarget,
    ) -> Option<Arc<LifecycleTargetLease>> {
        self.retained_lifecycle_targets
            .lock()
            .expect("retained lifecycle target registry must not be poisoned")
            .capture_pane(self, target)
    }

    pub(in crate::handler) fn capture_retained_window_lifecycle_target(
        &self,
        target: &WindowTarget,
    ) -> Option<Arc<LifecycleTargetLease>> {
        self.retained_lifecycle_targets
            .lock()
            .expect("retained lifecycle target registry must not be poisoned")
            .capture_window(self, target)
    }

    pub(in crate::handler) fn retire_removed_lifecycle_targets(&self) {
        self.retained_lifecycle_targets
            .lock()
            .expect("retained lifecycle target registry must not be poisoned")
            .retire_absent(self);
    }

    pub(in crate::handler) fn retire_respawned_lifecycle_panes(&self, pane_ids: &[PaneId]) {
        self.retained_lifecycle_targets
            .lock()
            .expect("retained lifecycle target registry must not be poisoned")
            .retire_panes(pane_ids);
    }
}

pub(super) fn capture_for_event(
    state: &HandlerState,
    event: &LifecycleEvent,
) -> Option<Arc<LifecycleTargetLease>> {
    match event {
        LifecycleEvent::PaneTitleChanged { target }
        | LifecycleEvent::PaneSetClipboard { target } => {
            state.capture_retained_pane_lifecycle_target(target)
        }
        LifecycleEvent::AlertBell { target }
        | LifecycleEvent::AlertActivity { target }
        | LifecycleEvent::AlertSilence { target } => {
            state.capture_retained_window_lifecycle_target(target)
        }
        _ => None,
    }
}

fn register_weak<K>(
    registry: &mut HashMap<K, Vec<Weak<LifecycleTargetLease>>>,
    key: K,
    lease: &Arc<LifecycleTargetLease>,
) where
    K: Eq + std::hash::Hash,
{
    let leases = registry.entry(key).or_default();
    leases.retain(|candidate| candidate.strong_count() > 0);
    leases.push(Arc::downgrade(lease));
}

fn retire_absent_from<K>(
    registry: &mut HashMap<K, Vec<Weak<LifecycleTargetLease>>>,
    state: &HandlerState,
) where
    K: Eq + std::hash::Hash,
{
    registry.retain(|_, leases| {
        leases.retain(|candidate| {
            let Some(lease) = candidate.upgrade() else {
                return false;
            };
            if lease.resolve_live(state).is_none() {
                lease.retire_or_invalidate(state);
                return false;
            }
            true
        });
        !leases.is_empty()
    });
}

fn resolve_window_anchor(
    state: &HandlerState,
    session_id: SessionId,
    window_id: WindowId,
    occurrence_id: Option<WindowLinkOccurrenceId>,
    original: &WindowTarget,
) -> Option<WindowTarget> {
    if let Some(occurrence_id) = occurrence_id {
        if let Some(target) =
            state.window_link_occurrence_target(occurrence_id, session_id, window_id)
        {
            return Some(target);
        }

        // A winlink occurrence can disappear while the same WindowId remains
        // live through another alias. Preserve the window lifetime in that
        // case; the stable id still prevents a numeric-slot replacement from
        // being acquired.
        return super::resolve_global_window_target(state, window_id);
    }

    state
        .sessions
        .iter()
        .filter_map(|(session_name, session)| {
            session
                .windows()
                .iter()
                .filter(|(_, window)| window.id() == window_id)
                .min_by_key(|(index, _)| {
                    (
                        session.id() != session_id,
                        session_name != original.session_name(),
                        **index != original.window_index(),
                        **index,
                    )
                })
                .map(|(index, _)| {
                    (
                        session.id() != session_id,
                        session_name != original.session_name(),
                        *index != original.window_index(),
                        session.id(),
                        WindowTarget::with_window(session_name.clone(), *index),
                    )
                })
        })
        .min_by_key(|(other_session, other_name, other_index, session_id, _)| {
            (*other_session, *other_name, *other_index, *session_id)
        })
        .map(|(_, _, _, _, target)| target)
}

fn resolve_pane_anchor(
    state: &HandlerState,
    session_id: SessionId,
    window_id: WindowId,
    pane_id: PaneId,
    occurrence_id: Option<WindowLinkOccurrenceId>,
    original: &PaneTarget,
) -> Option<PaneTarget> {
    let original_window =
        WindowTarget::with_window(original.session_name().clone(), original.window_index());
    let window = resolve_window_anchor(
        state,
        session_id,
        window_id,
        occurrence_id,
        &original_window,
    );
    if let Some(window) = window {
        if let Some(pane_index) = state
            .sessions
            .session(window.session_name())
            .and_then(|session| session.window_at(window.window_index()))
            .and_then(|window| {
                window
                    .panes()
                    .iter()
                    .find(|pane| pane.id() == pane_id)
                    .map(rmux_core::Pane::index)
            })
        {
            return Some(PaneTarget::with_window(
                window.session_name().clone(),
                window.window_index(),
                pane_index,
            ));
        }
    }

    super::resolve_global_pane_target(state, pane_id)
}

#[cfg(test)]
#[path = "retained_target_tests.rs"]
mod tests;
