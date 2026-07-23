//! Silence alert timer synchronization and expiry handling.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use rmux_proto::{types::OptionScopeSelector, SessionId, SessionName, WindowId, WindowTarget};

use super::super::RequestHandler;
use super::{monitor_silence_seconds, SilenceTimerState};
use crate::pane_terminals::HandlerState;

#[path = "silence_timers/deadline_fanout.rs"]
mod deadline_fanout;
#[path = "silence_timers/lifecycle.rs"]
mod lifecycle;
#[path = "silence_timers/new_session.rs"]
mod new_session;
#[path = "silence_timers/window_mutations.rs"]
mod window_mutations;

pub(in crate::handler) use deadline_fanout::SilenceTimerDeadlineFanout;

#[derive(Clone)]
struct DesiredSilenceTimer {
    target: WindowTarget,
    session_id: SessionId,
    window_id: WindowId,
    seconds: u64,
}

impl RequestHandler {
    pub(in crate::handler) async fn sync_session_silence_timers(&self, session_name: &SessionName) {
        let state = self.state.lock().await;
        let Some(session) = state.sessions.session(session_name) else {
            return;
        };
        let targets = session
            .windows()
            .keys()
            .copied()
            .map(|window_index| WindowTarget::with_window(session_name.clone(), window_index))
            .collect::<Vec<_>>();
        let desired = desired_silence_timers(&state, &targets);
        self.reconcile_silence_timers_for_session(session_name, desired);
        drop(state);
    }

    pub(in crate::handler) async fn sync_all_silence_timers(&self) {
        let state = self.state.lock().await;
        let targets = state
            .sessions
            .iter()
            .flat_map(|(session_name, session)| {
                session.windows().keys().copied().map(|window_index| {
                    WindowTarget::with_window(session_name.clone(), window_index)
                })
            })
            .collect::<Vec<_>>();
        let desired = desired_silence_timers(&state, &targets);

        let existing = {
            let timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            timers.keys().cloned().collect::<Vec<_>>()
        };
        for target in existing {
            if !desired.iter().any(|candidate| candidate.target == target) {
                self.remove_silence_timer(&target);
            }
        }
        for desired in desired {
            self.configure_silence_timer(desired);
        }
        drop(state);
    }

    pub(in crate::handler) async fn cancel_session_silence_timers(
        &self,
        session_name: &SessionName,
    ) {
        // Serialize cancellation with every state-derived timer producer.
        let state = self.state.lock().await;
        let current_session_id = state
            .sessions
            .session(session_name)
            .map(|session| session.id());
        let existing = {
            let timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            timers
                .iter()
                .filter(|(target, timer)| {
                    target.session_name() == session_name
                        && current_session_id != Some(timer.session_id)
                })
                .map(|(target, _)| target.clone())
                .collect::<Vec<_>>()
        };
        for target in existing {
            self.remove_silence_timer(&target);
        }
        drop(state);
    }

    pub(in crate::handler) async fn sync_alert_timers_for_option_scope(
        &self,
        scope: &OptionScopeSelector,
    ) {
        match scope {
            OptionScopeSelector::Session(session_name) => {
                self.sync_session_silence_timers(session_name).await;
            }
            OptionScopeSelector::Window(target) => {
                self.sync_window_family_silence_timers(
                    target.session_name(),
                    target.window_index(),
                )
                .await;
            }
            OptionScopeSelector::Pane(target) => {
                self.sync_window_family_silence_timers(
                    target.session_name(),
                    target.window_index(),
                )
                .await;
            }
            OptionScopeSelector::ServerGlobal
            | OptionScopeSelector::SessionGlobal
            | OptionScopeSelector::WindowGlobal => {
                self.sync_all_silence_timers().await;
            }
        }
    }

    async fn sync_window_family_silence_timers(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) {
        let state = self.state.lock().await;
        let targets = state.window_linked_window_targets(session_name, window_index);
        self.sync_window_silence_timers_locked(&state, targets);
        drop(state);
    }

    pub(in crate::handler) fn sync_window_silence_timers_locked(
        &self,
        state: &HandlerState,
        targets: Vec<WindowTarget>,
    ) {
        let desired = desired_silence_timers(state, &targets);
        for target in targets {
            if !desired.iter().any(|candidate| candidate.target == target) {
                self.remove_silence_timer(&target);
            }
        }
        for desired in desired {
            self.configure_silence_timer(desired);
        }
    }

    pub(in crate::handler) fn sync_inserted_window_silence_timers_locked(
        &self,
        state: &HandlerState,
        destination_targets: Vec<WindowTarget>,
        reindexed_session_names: Vec<SessionName>,
        index_map: BTreeMap<u32, u32>,
        deadline_fanout: Option<SilenceTimerDeadlineFanout>,
    ) {
        let desired_destinations = desired_silence_timers(state, &destination_targets);

        #[cfg(test)]
        self.pause_before_silence_timer_apply();

        let mut seen_sessions = HashSet::new();
        let mut reindexed_targets = Vec::new();
        for session_name in reindexed_session_names {
            if !seen_sessions.insert(session_name.clone()) {
                continue;
            }
            reindexed_targets.extend(
                index_map
                    .iter()
                    .filter(|(source_index, target_index)| source_index != target_index)
                    .map(|(source_index, target_index)| {
                        (
                            WindowTarget::with_window(session_name.clone(), *source_index),
                            WindowTarget::with_window(session_name.clone(), *target_index),
                        )
                    }),
            );
        }

        self.apply_silence_timer_reindex(
            state,
            reindexed_targets,
            destination_targets,
            desired_destinations,
            deadline_fanout,
        );
    }

    pub(in crate::handler) fn rekey_session_silence_timers_locked(
        &self,
        state: &HandlerState,
        previous_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
    ) {
        let reindexed_targets = {
            let timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            timers
                .iter()
                .filter(|(target, timer)| {
                    target.session_name() == previous_name && timer.session_id == session_id
                })
                .map(|(target, _)| {
                    (
                        target.clone(),
                        WindowTarget::with_window(new_name.clone(), target.window_index()),
                    )
                })
                .collect::<Vec<_>>()
        };
        self.apply_silence_timer_reindex(state, reindexed_targets, Vec::new(), Vec::new(), None);
    }

    fn apply_silence_timer_reindex(
        &self,
        state: &HandlerState,
        reindexed_targets: Vec<(WindowTarget, WindowTarget)>,
        removed_targets: Vec<WindowTarget>,
        desired_destinations: Vec<DesiredSilenceTimer>,
        deadline_fanout: Option<SilenceTimerDeadlineFanout>,
    ) {
        let admission_count = reindexed_targets.len().saturating_add(
            desired_destinations
                .iter()
                .filter(|desired| desired.seconds != 0)
                .count(),
        );
        let Some(mut timer_reservations) = self.reserve_silence_timer_tasks(admission_count) else {
            return;
        };
        let mut touched_targets = removed_targets.into_iter().collect::<HashSet<_>>();
        touched_targets.extend(
            desired_destinations
                .iter()
                .map(|desired| desired.target.clone()),
        );
        for (source, destination) in &reindexed_targets {
            let _ = touched_targets.insert(source.clone());
            let _ = touched_targets.insert(destination.clone());
        }

        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        // Extract every source and destination before inserting any moved
        // timer. Adjacent index moves overlap, so incremental rekeying would
        // otherwise overwrite the next timer in the chain.
        let mut extracted = HashMap::new();
        let mut original_generations = HashMap::new();
        for target in touched_targets {
            if let Some(timer) = timers.remove(&target) {
                timer.task.abort();
                let _ = original_generations.insert(target.clone(), timer.generation);
                let _ = extracted.insert(target, timer);
            }
        }

        let mut moved_timers = Vec::new();
        for (source, destination) in reindexed_targets {
            let Some(timer) = extracted.remove(&source) else {
                continue;
            };
            let generation = timer
                .generation
                .max(original_generations.get(&destination).copied().unwrap_or(0))
                .saturating_add(1);
            // The absolute deadline follows the winlink. Bump past both old
            // generations so an aborted task already waiting on this mutex
            // cannot expire the replacement at its new key.
            let session_id = timer.session_id;
            let window_id = timer.window_id;
            let identity_matches = state
                .sessions
                .session(destination.session_name())
                .filter(|session| session.id() == session_id)
                .and_then(|session| session.window_at(destination.window_index()))
                .is_some_and(|window| window.id() == window_id);
            if !identity_matches {
                continue;
            }
            let deadline = timer.deadline;
            let task = self.spawn_silence_timer_task(
                destination.clone(),
                session_id,
                window_id,
                generation,
                deadline,
                timer_reservations.take(),
            );
            moved_timers.push((
                destination,
                SilenceTimerState {
                    session_id,
                    window_id,
                    generation,
                    deadline,
                    task,
                },
            ));
        }
        for (target, timer) in moved_timers {
            if let Some(previous) = timers.insert(target, timer) {
                previous.task.abort();
                debug_assert!(false, "silence timer rekey must be injective");
            }
        }

        for desired in desired_destinations {
            let target = desired.target;
            let generation = original_generations
                .get(&target)
                .copied()
                .unwrap_or(0)
                .saturating_add(1);
            if desired.seconds == 0 {
                continue;
            }
            let deadline = match deadline_fanout
                .filter(|fanout| fanout.window_id == desired.window_id)
                .map(|fanout| fanout.deadline)
            {
                Some(Some(deadline)) => deadline,
                Some(None) => continue,
                None => tokio::time::Instant::now() + Duration::from_secs(desired.seconds),
            };
            let task = self.spawn_silence_timer_task(
                target.clone(),
                desired.session_id,
                desired.window_id,
                generation,
                deadline,
                timer_reservations.take(),
            );
            if let Some(previous) = timers.insert(
                target,
                SilenceTimerState {
                    session_id: desired.session_id,
                    window_id: desired.window_id,
                    generation,
                    deadline,
                    task,
                },
            ) {
                previous.task.abort();
                debug_assert!(
                    false,
                    "new link destination must not collide with a shifted timer"
                );
            }
        }
        drop(timers);
        drop(timer_reservations);
    }

    pub(super) fn configure_silence_timer_locked(
        &self,
        state: &HandlerState,
        target: WindowTarget,
        seconds: u64,
    ) {
        let Some(desired) = desired_silence_timer(state, target.clone(), seconds) else {
            self.remove_silence_timer(&target);
            return;
        };
        self.configure_silence_timer(desired);
    }

    fn configure_silence_timer(&self, desired: DesiredSilenceTimer) {
        let target = desired.target;
        if desired.seconds == 0 {
            self.remove_silence_timer(&target);
            return;
        }
        let Some(mut timer_reservations) = self.reserve_silence_timer_tasks(1) else {
            return;
        };
        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        let generation = timers
            .get(&target)
            .map_or(1, |state| state.generation.saturating_add(1));
        if let Some(previous) = timers.remove(&target) {
            previous.task.abort();
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(desired.seconds);
        let task = self.spawn_silence_timer_task(
            target.clone(),
            desired.session_id,
            desired.window_id,
            generation,
            deadline,
            timer_reservations.take(),
        );
        timers.insert(
            target,
            SilenceTimerState {
                session_id: desired.session_id,
                window_id: desired.window_id,
                generation,
                deadline,
                task,
            },
        );
        drop(timers);
        drop(timer_reservations);
    }

    #[cfg(test)]
    pub(in crate::handler) fn silence_timer_generation_for_test(
        &self,
        target: &WindowTarget,
    ) -> Option<u64> {
        self.silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned")
            .get(target)
            .map(|state| state.generation)
    }

    #[cfg(test)]
    pub(in crate::handler) fn silence_timer_snapshot_for_test(
        &self,
        target: &WindowTarget,
    ) -> Option<(u64, tokio::time::Instant)> {
        self.silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned")
            .get(target)
            .map(|state| (state.generation, state.deadline))
    }

    #[cfg(test)]
    pub(in crate::handler) fn silence_timer_identity_for_test(
        &self,
        target: &WindowTarget,
    ) -> Option<(SessionId, WindowId, u64)> {
        self.silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned")
            .get(target)
            .map(|state| (state.session_id, state.window_id, state.generation))
    }

    #[cfg(test)]
    pub(in crate::handler) async fn expire_silence_timer_for_test(
        &self,
        target: WindowTarget,
        session_id: SessionId,
        window_id: WindowId,
        generation: u64,
    ) {
        self.handle_silence_timer_expired(target, session_id, window_id, generation)
            .await;
    }

    fn reconcile_silence_timers_for_session(
        &self,
        session_name: &SessionName,
        desired: Vec<DesiredSilenceTimer>,
    ) {
        let existing = {
            let timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            timers
                .keys()
                .filter(|target| target.session_name() == session_name)
                .cloned()
                .collect::<Vec<_>>()
        };
        for target in existing {
            if !desired.iter().any(|candidate| candidate.target == target) {
                self.remove_silence_timer(&target);
            }
        }
        for desired in desired {
            self.configure_silence_timer(desired);
        }
    }
}

fn desired_silence_timers(
    state: &HandlerState,
    targets: &[WindowTarget],
) -> Vec<DesiredSilenceTimer> {
    targets
        .iter()
        .filter_map(|target| {
            desired_silence_timer(
                state,
                target.clone(),
                monitor_silence_seconds(
                    &state.options,
                    target.session_name(),
                    target.window_index(),
                ),
            )
        })
        .collect()
}

fn desired_silence_timer(
    state: &HandlerState,
    target: WindowTarget,
    seconds: u64,
) -> Option<DesiredSilenceTimer> {
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    Some(DesiredSilenceTimer {
        target,
        session_id: session.id(),
        window_id: window.id(),
        seconds,
    })
}
