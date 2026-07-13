use std::collections::HashMap;
use std::time::{Duration, Instant};

use rmux_core::events::PaneOutputSubscriptionKey;
use rmux_proto::{PaneTarget, SessionId, SessionName};

use crate::pane_io::PaneOutputSender;
use crate::pane_terminals::HandlerState;

use super::RequestHandler;

/// How long an exited, removed pane keeps its output ring available for a
/// late `Oldest` SDK subscription.
pub(in crate::handler) const EXITED_PANE_OUTPUT_RETENTION_TTL: Duration = Duration::from_secs(5);

/// Stable session identities for the user-facing target alias and the pane's
/// runtime owner. Grouped/linked sessions can make these two sessions differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct RetainedExitedPaneIdentities {
    target_session_id: SessionId,
    runtime_session_id: SessionId,
}

impl RetainedExitedPaneIdentities {
    pub(in crate::handler) const fn new(
        target_session_id: SessionId,
        runtime_session_id: SessionId,
    ) -> Self {
        Self {
            target_session_id,
            runtime_session_id,
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct RetainedExitedPaneOutput {
    pane: PaneOutputSubscriptionKey,
    output: PaneOutputSender,
    expires_at: Instant,
    identities: RetainedExitedPaneIdentities,
}

impl RetainedExitedPaneOutput {
    fn new(
        pane: PaneOutputSubscriptionKey,
        output: PaneOutputSender,
        identities: RetainedExitedPaneIdentities,
        now: Instant,
        ttl: Duration,
    ) -> Self {
        Self {
            pane,
            output,
            expires_at: now + ttl,
            identities,
        }
    }

    pub(in crate::handler) fn pane(&self) -> &PaneOutputSubscriptionKey {
        &self.pane
    }

    pub(in crate::handler) fn output(&self) -> &PaneOutputSender {
        &self.output
    }

    fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }
}

#[derive(Debug, Default)]
pub(in crate::handler) struct RetainedExitedPaneOutputs {
    by_target: HashMap<PaneTarget, PaneOutputSubscriptionKey>,
    by_pane: HashMap<PaneOutputSubscriptionKey, (PaneTarget, RetainedExitedPaneOutput)>,
}

impl RetainedExitedPaneOutputs {
    pub(in crate::handler) fn insert(
        &mut self,
        target: PaneTarget,
        pane: PaneOutputSubscriptionKey,
        output: PaneOutputSender,
        identities: RetainedExitedPaneIdentities,
        now: Instant,
        ttl: Duration,
    ) {
        self.cleanup_expired(now);
        let retained = RetainedExitedPaneOutput::new(pane.clone(), output, identities, now, ttl);
        self.insert_entry(target, pane, retained, true);
    }

    fn insert_entry(
        &mut self,
        target: PaneTarget,
        pane: PaneOutputSubscriptionKey,
        retained: RetainedExitedPaneOutput,
        current_for_slot: bool,
    ) {
        if let Some((previous_target, _)) = self.by_pane.get(&pane) {
            let previous_target = previous_target.clone();
            if self
                .by_target
                .get(&previous_target)
                .is_some_and(|current| current == &pane)
            {
                self.by_target.remove(&previous_target);
            }
        }
        self.by_pane
            .insert(pane.clone(), (target.clone(), retained));
        if current_for_slot {
            self.by_target.insert(target, pane);
        }
    }

    pub(in crate::handler) fn rekey_session(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
        now: Instant,
    ) {
        self.cleanup_expired(now);
        let previous_slot_owners = std::mem::take(&mut self.by_target);
        let mut unchanged = Vec::new();
        let mut changed = Vec::new();

        for (previous_pane, (previous_target, mut retained)) in self.by_pane.drain() {
            let was_current_slot = previous_slot_owners
                .get(&previous_target)
                .is_some_and(|current| current == &previous_pane);
            let mut target = previous_target.clone();
            let mut pane = previous_pane.clone();
            let mut did_change = false;

            if retained.identities.target_session_id == session_id
                && previous_target.session_name() == old_name
            {
                target = PaneTarget::with_window(
                    new_name.clone(),
                    previous_target.window_index(),
                    previous_target.pane_index(),
                );
                did_change = true;
            }
            if retained.identities.runtime_session_id == session_id
                && previous_pane.runtime_session_name() == old_name
            {
                pane = PaneOutputSubscriptionKey::new(new_name.clone(), previous_pane.pane_id());
                retained.pane = pane.clone();
                did_change = true;
            }

            let entry = (pane, target, retained, was_current_slot);
            if did_change {
                changed.push(entry);
            } else {
                unchanged.push(entry);
            }
        }

        // A renamed live identity wins a collision with stale retained output
        // left under a previously used destination name.
        for (pane, target, retained, was_current_slot) in unchanged.into_iter().chain(changed) {
            self.insert_entry(target, pane, retained, was_current_slot);
        }
        self.retain_consistent_slot_indexes();
    }

    pub(in crate::handler) fn get(
        &mut self,
        target: &PaneTarget,
        now: Instant,
    ) -> Option<RetainedExitedPaneOutput> {
        self.cleanup_expired(now);
        let pane = self.by_target.get(target)?;
        self.by_pane
            .get(pane)
            .map(|(_target, retained)| retained.clone())
    }

    pub(in crate::handler) fn get_by_pane(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) -> Option<(PaneTarget, RetainedExitedPaneOutput)> {
        self.cleanup_expired(now);
        self.by_pane.get(pane).cloned()
    }

    #[cfg(test)]
    pub(in crate::handler) fn cleanup_pane_if_expired(
        &mut self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) {
        let Some((target, retained)) = self.by_pane.get(pane) else {
            return;
        };
        if !retained.is_expired(now) {
            return;
        }

        let target = target.clone();
        self.by_pane.remove(pane);
        if self
            .by_target
            .get(&target)
            .is_some_and(|current_pane| current_pane == pane)
        {
            self.by_target.remove(&target);
        }
    }

    pub(in crate::handler) fn is_empty(&mut self, now: Instant) -> bool {
        self.cleanup_expired(now);
        self.by_pane.is_empty()
    }

    pub(in crate::handler) fn clear(&mut self) {
        self.by_target.clear();
        self.by_pane.clear();
    }

    fn cleanup_expired(&mut self, now: Instant) {
        self.by_pane
            .retain(|_, (_target, retained)| !retained.is_expired(now));
        self.retain_consistent_slot_indexes();
    }

    fn retain_consistent_slot_indexes(&mut self) {
        self.by_target.retain(|target, pane| {
            self.by_pane
                .get(pane)
                .is_some_and(|(pane_target, _)| pane_target == target)
        });
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn retain_exited_pane_output(
        &self,
        mut target: PaneTarget,
        mut pane: PaneOutputSubscriptionKey,
        identities: RetainedExitedPaneIdentities,
        output: PaneOutputSender,
    ) {
        let now = Instant::now();
        // Serialize name normalization and insertion with rename-session. Both
        // paths hold state before the retained-output mutex, so a pane-exit
        // plan captured before a rename cannot insert an old-name record after
        // that rename has committed its rekey.
        let state = self.state.lock().await;
        if let Some(current_name) = session_name_for_id(&state, identities.target_session_id) {
            target =
                PaneTarget::with_window(current_name, target.window_index(), target.pane_index());
        }
        if let Some(current_name) = session_name_for_id(&state, identities.runtime_session_id) {
            pane = PaneOutputSubscriptionKey::new(current_name, pane.pane_id());
        }
        self.retained_exited_outputs
            .lock()
            .expect("retained exited output mutex must not be poisoned")
            .insert(
                target,
                pane,
                output,
                identities,
                now,
                EXITED_PANE_OUTPUT_RETENTION_TTL,
            );
        drop(state);
        self.watch_retained_exited_pane_output();
    }

    pub(in crate::handler) fn rekey_retained_exited_pane_outputs(
        &self,
        old_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
    ) {
        self.retained_exited_outputs
            .lock()
            .expect("retained exited output mutex must not be poisoned")
            .rekey_session(old_name, new_name, session_id, Instant::now());
    }

    pub(in crate::handler) fn retained_exited_pane_output(
        &self,
        target: &PaneTarget,
        now: Instant,
    ) -> Option<RetainedExitedPaneOutput> {
        self.retained_exited_outputs
            .lock()
            .expect("retained exited output mutex must not be poisoned")
            .get(target, now)
    }

    pub(in crate::handler) fn retained_exited_pane_output_by_pane(
        &self,
        pane: &PaneOutputSubscriptionKey,
        now: Instant,
    ) -> Option<(PaneTarget, RetainedExitedPaneOutput)> {
        self.retained_exited_outputs
            .lock()
            .expect("retained exited output mutex must not be poisoned")
            .get_by_pane(pane, now)
    }

    fn watch_retained_exited_pane_output(&self) {
        let handler = self.downgrade();
        tokio::spawn(async move {
            tokio::time::sleep(EXITED_PANE_OUTPUT_RETENTION_TTL).await;
            let Some(handler) = handler.upgrade() else {
                return;
            };
            handler
                .retained_exited_outputs
                .lock()
                .expect("retained exited output mutex must not be poisoned")
                .cleanup_expired(Instant::now());
            let _ = handler.request_shutdown_if_pending();
        });
    }
}

fn session_name_for_id(state: &HandlerState, session_id: SessionId) -> Option<SessionName> {
    state
        .sessions
        .iter()
        .find_map(|(name, session)| (session.id() == session_id).then(|| name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_io::pane_output_channel_with_limits;
    use rmux_proto::{PaneId, SessionName};

    #[test]
    fn replacing_target_preserves_pane_lookup_and_updates_slot_lookup() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let ttl = Duration::from_secs(60);
        let target = PaneTarget::new(session_name("alpha"), 0);
        let old_pane = pane_key(34);
        let new_pane = pane_key(35);

        retained.insert(
            target.clone(),
            old_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(1, 1),
            now,
            ttl,
        );
        retained.insert(
            target.clone(),
            new_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(1, 1),
            now,
            ttl,
        );

        let (old_target, old_output) = retained
            .get_by_pane(&old_pane, now)
            .expect("old pane id remains retained by stable identity");
        assert_eq!(old_target, target);
        assert_eq!(old_output.pane(), &old_pane);

        let (retained_target, retained_output) = retained
            .get_by_pane(&new_pane, now)
            .expect("new pane id remains retained");
        assert_eq!(retained_target, target);
        assert_eq!(retained_output.pane(), &new_pane);
        assert_eq!(
            retained.get(&target, now).expect("target retained").pane(),
            &new_pane
        );
    }

    #[test]
    fn cleanup_by_pane_expires_reused_slots_without_dropping_newest_lookup() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let target = PaneTarget::new(session_name("alpha"), 0);
        let old_pane = pane_key(34);
        let new_pane = pane_key(35);

        retained.insert(
            target.clone(),
            old_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(1, 1),
            now,
            Duration::from_secs(1),
        );
        retained.insert(
            target.clone(),
            new_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(1, 1),
            now,
            Duration::from_secs(60),
        );

        retained.cleanup_pane_if_expired(&old_pane, now + Duration::from_secs(2));

        assert!(
            !retained.by_pane.contains_key(&old_pane),
            "expired old pane identity should be removed"
        );
        assert_eq!(
            retained
                .get(&target, now + Duration::from_secs(2))
                .expect("target should still resolve to newest pane")
                .pane(),
            &new_pane
        );
    }

    #[test]
    fn cleanup_by_pane_removes_target_when_current_slot_expires() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let target = PaneTarget::new(session_name("alpha"), 0);
        let pane = pane_key(34);

        retained.insert(
            target.clone(),
            pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(1, 1),
            now,
            Duration::from_secs(1),
        );

        retained.cleanup_pane_if_expired(&pane, now + Duration::from_secs(2));

        assert!(!retained.by_pane.contains_key(&pane));
        assert!(!retained.by_target.contains_key(&target));
        assert!(retained.is_empty(now + Duration::from_secs(2)));
    }

    #[test]
    fn grouped_alias_rekeys_target_and_runtime_names_by_separate_identities() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let peer_target = PaneTarget::new(session_name("peer"), 0);
        let owner_pane = PaneOutputSubscriptionKey::new(session_name("owner"), PaneId::new(34));
        retained.insert(
            peer_target.clone(),
            owner_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(2, 1),
            now,
            Duration::from_secs(60),
        );

        let renamed_peer = session_name("renamed-peer");
        retained.rekey_session(&session_name("peer"), &renamed_peer, SessionId::new(2), now);
        let renamed_target = PaneTarget::new(renamed_peer, 0);
        let target_rekeyed = retained
            .get(&renamed_target, now)
            .expect("target alias follows its stable session identity");
        assert_eq!(target_rekeyed.pane(), &owner_pane);
        assert!(retained.get(&peer_target, now).is_none());

        let renamed_owner = session_name("renamed-owner");
        retained.rekey_session(
            &session_name("owner"),
            &renamed_owner,
            SessionId::new(1),
            now,
        );
        let renamed_pane = PaneOutputSubscriptionKey::new(renamed_owner, PaneId::new(34));
        let (target, output) = retained
            .get_by_pane(&renamed_pane, now)
            .expect("runtime owner follows its distinct stable identity");
        assert_eq!(target, renamed_target);
        assert_eq!(output.pane(), &renamed_pane);
        assert!(retained.get_by_pane(&owner_pane, now).is_none());
    }

    #[test]
    fn rekey_fails_closed_on_name_reuse_and_preserves_expiry() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let old_target = PaneTarget::new(session_name("alpha"), 0);
        let old_pane = pane_key(34);
        retained.insert(
            old_target.clone(),
            old_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(7, 7),
            now,
            Duration::from_secs(1),
        );

        let new_name = session_name("beta");
        retained.rekey_session(&session_name("alpha"), &new_name, SessionId::new(8), now);
        assert!(retained.get(&old_target, now).is_some());
        assert!(retained
            .get(&PaneTarget::new(new_name.clone(), 0), now)
            .is_none());

        retained.rekey_session(&session_name("alpha"), &new_name, SessionId::new(7), now);
        let new_target = PaneTarget::new(new_name.clone(), 0);
        let new_pane = PaneOutputSubscriptionKey::new(new_name, PaneId::new(34));
        assert!(retained.get(&old_target, now).is_none());
        assert!(retained.get(&new_target, now).is_some());

        retained.cleanup_pane_if_expired(&new_pane, now + Duration::from_secs(2));
        assert!(retained.is_empty(now + Duration::from_secs(2)));
    }

    #[test]
    fn rekey_collision_drops_the_losing_target_index() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let losing_target = PaneTarget::new(session_name("destination-slot"), 0);
        let winning_target = PaneTarget::new(session_name("visible-alias"), 1);
        let destination_pane =
            PaneOutputSubscriptionKey::new(session_name("beta"), PaneId::new(34));
        let source_pane = PaneOutputSubscriptionKey::new(session_name("alpha"), PaneId::new(34));

        retained.insert(
            losing_target.clone(),
            destination_pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(9, 9),
            now,
            Duration::from_secs(60),
        );
        retained.insert(
            winning_target.clone(),
            source_pane,
            pane_output_channel_with_limits(8, 1024),
            identities(3, 1),
            now,
            Duration::from_secs(60),
        );

        retained.rekey_session(
            &session_name("alpha"),
            &session_name("beta"),
            SessionId::new(1),
            now,
        );

        assert!(
            retained.get(&losing_target, now).is_none(),
            "a target index must not point through a collided pane key to another target"
        );
        let winner = retained
            .get(&winning_target, now)
            .expect("changed identity wins the destination pane-key collision");
        assert_eq!(winner.pane(), &destination_pane);
        let (indexed_target, _) = retained
            .get_by_pane(&destination_pane, now)
            .expect("winning pane identity remains indexed");
        assert_eq!(indexed_target, winning_target);
    }

    #[test]
    fn reinserting_one_pane_identity_removes_its_previous_target_index() {
        let mut retained = RetainedExitedPaneOutputs::default();
        let now = Instant::now();
        let pane = pane_key(34);
        let old_target = PaneTarget::new(session_name("alpha"), 0);
        let new_target = PaneTarget::new(session_name("beta"), 1);
        retained.insert(
            old_target.clone(),
            pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(1, 1),
            now,
            Duration::from_secs(60),
        );
        retained.insert(
            new_target.clone(),
            pane.clone(),
            pane_output_channel_with_limits(8, 1024),
            identities(2, 1),
            now,
            Duration::from_secs(60),
        );

        assert!(retained.get(&old_target, now).is_none());
        assert_eq!(
            retained
                .get(&new_target, now)
                .expect("new target owns the reinserted pane key")
                .pane(),
            &pane
        );
    }

    fn pane_key(pane_id: u32) -> PaneOutputSubscriptionKey {
        PaneOutputSubscriptionKey::new(session_name("alpha"), PaneId::new(pane_id))
    }

    fn identities(target: u32, runtime: u32) -> RetainedExitedPaneIdentities {
        RetainedExitedPaneIdentities::new(SessionId::new(target), SessionId::new(runtime))
    }

    fn session_name(name: &str) -> SessionName {
        SessionName::new(name).expect("valid test session name")
    }
}
