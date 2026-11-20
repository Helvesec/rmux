use std::collections::{BTreeMap, BTreeSet, HashSet};

use rmux_core::events::PaneOutputSubscriptionKey;
use rmux_proto::{PaneId, SessionName};

use crate::pane_terminals::HandlerState;

use super::pane_stream_support::PaneStreamSource;
use super::subscription_support::capture_pane_stream_sources;

/// Canonical pane-output subscription keys and immutable stream sources before
/// a state mutation that may move or destroy pane runtimes.
///
/// The snapshot expands the source and destination sessions through both
/// grouped-session and linked-window families. Comparing by stable `PaneId`
/// after the mutation captures only the runtime owners that actually changed,
/// including owner transfers caused by removing an emptied source session.
pub(in crate::handler) struct PaneOutputSubscriptionKeySnapshot {
    keys_by_pane: BTreeMap<PaneId, PaneOutputSubscriptionKey>,
    stream_sources_by_pane: BTreeMap<PaneId, PaneStreamSource>,
}

/// Registry changes required to make a pre-mutation key snapshot match the
/// committed handler state.
///
/// Keeping rekeys and removals in one value lets the handler apply both under
/// one subscriptions lock while it still owns the state lock. A pane is only
/// removed when its stable `PaneId` has no canonical key after the mutation,
/// so linked/grouped aliases that preserve the runtime are rekeyed instead.
/// Each removal carries only its own pre-mutation stream source; sources for
/// surviving panes are dropped with the snapshot.
pub(in crate::handler) struct PaneOutputSubscriptionReconciliation {
    rekeys: Vec<(PaneOutputSubscriptionKey, PaneOutputSubscriptionKey)>,
    removals: Vec<PaneOutputSubscriptionRemoval>,
}

pub(in crate::handler) struct PaneOutputSubscriptionRemoval {
    pub(in crate::handler) key: PaneOutputSubscriptionKey,
    pub(in crate::handler) stream_source: Option<PaneStreamSource>,
}

impl PaneOutputSubscriptionKeySnapshot {
    pub(in crate::handler) fn capture_related(state: &HandlerState, roots: &[SessionName]) -> Self {
        let related_sessions = related_session_family(state, roots);
        Self::capture_sessions(state, related_sessions)
    }

    pub(in crate::handler) fn capture_all(state: &HandlerState) -> Self {
        Self::capture_sessions(
            state,
            state
                .sessions
                .iter()
                .map(|session| (session.name(), session))
                .map(|(session_name, _)| session_name.clone()),
        )
    }

    fn capture_sessions(
        state: &HandlerState,
        session_names: impl IntoIterator<Item = SessionName>,
    ) -> Self {
        let pane_ids = session_names
            .into_iter()
            .filter_map(|session_name| state.sessions.session(&session_name))
            .flat_map(|session| {
                session
                    .windows()
                    .map(|(_, window)| window)
                    .flat_map(|window| window.panes().iter().map(rmux_core::Pane::id))
            })
            .collect::<BTreeSet<_>>();
        let keys_by_pane: BTreeMap<PaneId, PaneOutputSubscriptionKey> = pane_ids
            .into_iter()
            .filter_map(|pane_id| {
                state
                    .pane_output_subscription_key_for_pane_id(pane_id)
                    .map(|key| (pane_id, key))
            })
            .collect();
        let stream_sources_by_pane = capture_pane_stream_sources(state, keys_by_pane.values())
            .into_iter()
            .map(|source| (source.key.pane_id(), source))
            .collect();
        Self {
            keys_by_pane,
            stream_sources_by_pane,
        }
    }

    pub(in crate::handler) fn reconcile_after(
        mut self,
        state: &HandlerState,
    ) -> PaneOutputSubscriptionReconciliation {
        let mut rekeys = Vec::new();
        let mut removals = Vec::new();
        for (pane_id, previous) in self.keys_by_pane {
            match state.pane_output_subscription_key_for_pane_id(pane_id) {
                Some(current) if current != previous => rekeys.push((previous, current)),
                Some(_) => {}
                None => removals.push(PaneOutputSubscriptionRemoval {
                    key: previous,
                    stream_source: self.stream_sources_by_pane.remove(&pane_id),
                }),
            }
        }
        PaneOutputSubscriptionReconciliation { rekeys, removals }
    }

    pub(in crate::handler) fn rekeys_after(
        self,
        state: &HandlerState,
    ) -> Vec<(PaneOutputSubscriptionKey, PaneOutputSubscriptionKey)> {
        self.keys_by_pane
            .into_iter()
            .filter_map(|(pane_id, previous)| {
                state
                    .pane_output_subscription_key_for_pane_id(pane_id)
                    .filter(|current| current != &previous)
                    .map(|current| (previous, current))
            })
            .collect()
    }
}

impl PaneOutputSubscriptionReconciliation {
    pub(in crate::handler) fn into_parts(
        self,
    ) -> (
        Vec<(PaneOutputSubscriptionKey, PaneOutputSubscriptionKey)>,
        Vec<PaneOutputSubscriptionRemoval>,
    ) {
        (self.rekeys, self.removals)
    }
}

fn related_session_family(state: &HandlerState, roots: &[SessionName]) -> HashSet<SessionName> {
    let mut related = HashSet::new();
    let mut pending = roots.to_vec();
    while let Some(session_name) = pending.pop() {
        if !related.insert(session_name.clone()) {
            continue;
        }
        pending.extend(state.sessions.session_group_members(&session_name));
        let window_indices = state
            .sessions
            .session(&session_name)
            .map(|session| {
                session
                    .windows()
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for window_index in window_indices {
            pending.extend(state.window_linked_session_family_list(&session_name, window_index));
        }
    }
    related
}
