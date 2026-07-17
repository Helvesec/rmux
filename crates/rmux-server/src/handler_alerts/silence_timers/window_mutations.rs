//! Silence-timer plans for structural window mutations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use rmux_core::WINLINK_SILENCE;
use rmux_proto::{SessionId, SessionName, WindowId, WindowTarget};

use super::super::super::RequestHandler;
use super::super::SilenceTimerState;
use super::{desired_silence_timers, monitor_silence_seconds, DesiredSilenceTimer};
use crate::pane_terminals::HandlerState;

#[path = "window_mutations/fanout.rs"]
mod fanout;

use fanout::resolve_silence_timer_target;

#[derive(Clone)]
pub(in crate::handler) struct SilenceTimerWindowMutation {
    timers: Vec<PlannedSilenceTimer>,
    windows: Vec<PlannedWindowIdentity>,
    target_overrides: HashMap<WindowTarget, Option<WindowTarget>>,
    fanout_targets: HashMap<WindowTarget, Vec<WindowTarget>>,
}

#[derive(Clone)]
struct PlannedSilenceTimer {
    source: WindowTarget,
    session_id: SessionId,
    window_id: WindowId,
    generation: u64,
    occurrence: Option<StableWindowOccurrence>,
}

#[derive(Clone)]
struct PlannedWindowIdentity {
    source: WindowTarget,
    session_id: SessionId,
    window_id: WindowId,
    occurrence: StableWindowOccurrence,
    was_monitored: bool,
    had_matching_timer: bool,
}

#[derive(Clone, Copy)]
struct StableWindowOccurrence {
    ordinal: usize,
    occurrence_count: usize,
}

impl SilenceTimerWindowMutation {
    pub(in crate::handler) fn map_target(
        &mut self,
        source: WindowTarget,
        destination: WindowTarget,
    ) {
        self.target_overrides.insert(source, Some(destination));
    }

    pub(in crate::handler) fn remove_target(&mut self, source: WindowTarget) {
        self.target_overrides.insert(source, None);
    }
}

impl RequestHandler {
    pub(in crate::handler) fn plan_all_window_mutation_silence_timers_locked(
        &self,
        state: &HandlerState,
    ) -> SilenceTimerWindowMutation {
        let session_names = state
            .sessions
            .iter()
            .map(|(session_name, _)| session_name.clone())
            .collect();
        self.plan_window_mutation_silence_timers_locked(state, session_names)
    }

    pub(in crate::handler) fn apply_window_mutation_silence_timers_and_arm_all_locked(
        &self,
        state: &HandlerState,
        mutation: SilenceTimerWindowMutation,
        removed_targets: Vec<WindowTarget>,
        reindexed_windows: &[(SessionName, BTreeMap<u32, u32>)],
    ) {
        let arm_targets = state
            .sessions
            .iter()
            .flat_map(|(session_name, session)| {
                session.windows().keys().copied().map(|window_index| {
                    WindowTarget::with_window(session_name.clone(), window_index)
                })
            })
            .collect();
        self.apply_window_mutation_silence_timers_locked(
            state,
            mutation,
            removed_targets,
            reindexed_windows,
            arm_targets,
        );
    }

    pub(in crate::handler) fn plan_window_mutation_silence_timers_locked(
        &self,
        state: &HandlerState,
        session_names: Vec<SessionName>,
    ) -> SilenceTimerWindowMutation {
        let session_names = session_names.into_iter().collect::<HashSet<_>>();
        let mut windows: Vec<PlannedWindowIdentity> = session_names
            .iter()
            .filter_map(|session_name| {
                state
                    .sessions
                    .session(session_name)
                    .map(|session| (session_name, session))
            })
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .iter()
                    .filter_map(|(window_index, window)| {
                        let source = WindowTarget::with_window(session_name.clone(), *window_index);
                        stable_window_occurrence(state, &source, session.id(), window.id()).map(
                            |occurrence| PlannedWindowIdentity {
                                source,
                                session_id: session.id(),
                                window_id: window.id(),
                                occurrence,
                                was_monitored: monitor_silence_seconds(
                                    &state.options,
                                    session_name,
                                    *window_index,
                                ) > 0,
                                had_matching_timer: false,
                            },
                        )
                    })
            })
            .collect();
        let timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        for planned in &mut windows {
            planned.had_matching_timer = timers.get(&planned.source).is_some_and(|timer| {
                timer.session_id == planned.session_id && timer.window_id == planned.window_id
            });
        }
        let timers = timers
            .iter()
            .filter(|(target, _)| session_names.contains(target.session_name()))
            .map(|(target, timer)| PlannedSilenceTimer {
                source: target.clone(),
                session_id: timer.session_id,
                window_id: timer.window_id,
                generation: timer.generation,
                occurrence: stable_window_occurrence(
                    state,
                    target,
                    timer.session_id,
                    timer.window_id,
                ),
            })
            .collect();
        SilenceTimerWindowMutation {
            timers,
            windows,
            target_overrides: HashMap::new(),
            fanout_targets: HashMap::new(),
        }
    }

    pub(in crate::handler) fn apply_window_mutation_silence_timers_locked(
        &self,
        state: &HandlerState,
        mutation: SilenceTimerWindowMutation,
        removed_targets: Vec<WindowTarget>,
        reindexed_windows: &[(SessionName, BTreeMap<u32, u32>)],
        arm_targets: Vec<WindowTarget>,
    ) {
        let removed_targets = removed_targets.into_iter().collect::<HashSet<_>>();
        let SilenceTimerWindowMutation {
            timers: planned_timers,
            windows: planned_windows,
            target_overrides,
            fanout_targets,
        } = mutation;
        let mut represented_targets = HashSet::new();
        let mut expired_targets = HashSet::new();
        for planned in planned_windows {
            if !planned.was_monitored || removed_targets.contains(&planned.source) {
                continue;
            }
            let Some(destination) = resolve_planned_window_target(
                state,
                &planned.source,
                planned.session_id,
                planned.window_id,
                Some(planned.occurrence),
                &target_overrides,
                reindexed_windows,
            ) else {
                continue;
            };
            if !planned.had_matching_timer {
                expired_targets.insert(destination.clone());
            }
            represented_targets.insert(destination);
        }
        let desired_arm_targets = desired_silence_timers(state, &arm_targets)
            .into_iter()
            .filter(|desired| !represented_targets.contains(&desired.target))
            .filter(|desired| !target_has_silence_flag(state, &desired.target))
            .filter(|desired| !target_group_contains_any(state, &desired.target, &expired_targets))
            .collect::<Vec<_>>();
        let primary_resolved = planned_timers
            .into_iter()
            .map(|planned| {
                let destination = (!removed_targets.contains(&planned.source))
                    .then(|| {
                        resolve_planned_window_target(
                            state,
                            &planned.source,
                            planned.session_id,
                            planned.window_id,
                            planned.occurrence,
                            &target_overrides,
                            reindexed_windows,
                        )
                    })
                    .flatten()
                    .and_then(|target| {
                        resolve_silence_timer_target(state, target, planned.window_id)
                    });
                (planned, destination)
            })
            .collect::<Vec<_>>();
        let primary_targets = primary_resolved
            .iter()
            .filter_map(|(_, destination)| {
                destination
                    .as_ref()
                    .map(|destination| destination.target.clone())
            })
            .collect::<HashSet<_>>();
        let resolved = primary_resolved
            .into_iter()
            .map(|(planned, primary)| {
                let mut destinations = primary.into_iter().collect::<Vec<_>>();
                if let Some(fanout) = fanout_targets.get(&planned.source) {
                    for target in fanout {
                        if destinations
                            .iter()
                            .any(|destination| destination.target == *target)
                            || primary_targets.contains(target)
                        {
                            continue;
                        }
                        if let Some(destination) =
                            resolve_silence_timer_target(state, target.clone(), planned.window_id)
                        {
                            destinations.push(destination);
                        }
                    }
                }
                (planned, destinations)
            })
            .collect::<Vec<_>>();

        #[cfg(test)]
        self.pause_before_silence_timer_apply();

        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        let mut original_generations = HashMap::new();
        for (planned, destinations) in &resolved {
            if let Some(timer) = timers.get(&planned.source) {
                let _ = original_generations.insert(planned.source.clone(), timer.generation);
            }
            for destination in destinations {
                if let Some(timer) = timers.get(&destination.target) {
                    let _ =
                        original_generations.insert(destination.target.clone(), timer.generation);
                }
            }
        }
        for desired in &desired_arm_targets {
            if let Some(timer) = timers.get(&desired.target) {
                let _ = original_generations.insert(desired.target.clone(), timer.generation);
            }
        }

        // Remove every changed key before inserting a moved timer. Renumbering
        // chains overlap (for example 2->1 while the removed window occupied
        // 1), so incremental moves would overwrite a still-live timer.
        let mut extracted = Vec::new();
        for (planned, destinations) in resolved {
            if destinations.len() == 1
                && destinations.first().is_some_and(|destination| {
                    destination.target == planned.source
                        && destination.session_id == planned.session_id
                })
            {
                continue;
            }
            let matches_plan = timers.get(&planned.source).is_some_and(|timer| {
                timer.session_id == planned.session_id
                    && timer.window_id == planned.window_id
                    && timer.generation == planned.generation
            });
            if !matches_plan {
                continue;
            }
            let timer = timers
                .remove(&planned.source)
                .expect("validated silence timer must still exist");
            timer.task.abort();
            extracted.push((planned, destinations, timer));
        }

        for (planned, destinations, timer) in extracted {
            for destination in destinations {
                if timers.contains_key(&destination.target) {
                    continue;
                }
                let generation = timer
                    .generation
                    .max(
                        original_generations
                            .get(&destination.target)
                            .copied()
                            .unwrap_or(0),
                    )
                    .saturating_add(1);
                let deadline = timer.deadline;
                let task = self.spawn_silence_timer_task(
                    destination.target.clone(),
                    destination.session_id,
                    planned.window_id,
                    generation,
                    deadline,
                );
                let previous = timers.insert(
                    destination.target,
                    SilenceTimerState {
                        session_id: destination.session_id,
                        window_id: planned.window_id,
                        generation,
                        deadline,
                        task,
                    },
                );
                debug_assert!(
                    previous.is_none(),
                    "silence timer fanout destinations must be injective"
                );
            }
        }

        // A structural window mutation arms only missing post-mutation targets.
        // Existing timers keep their absolute deadlines; explicit SetOption
        // synchronization deliberately takes the normal rearm path.
        for desired in desired_arm_targets {
            arm_missing_timer(self, &mut timers, &original_generations, desired);
        }
    }

    #[cfg(test)]
    pub(in crate::handler) fn replace_silence_timer_deadline_for_test(
        &self,
        target: &WindowTarget,
        deadline: tokio::time::Instant,
    ) {
        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        let previous = timers
            .remove(target)
            .expect("silence timer must exist before replacing its deadline");
        previous.task.abort();
        let generation = previous.generation.saturating_add(1);
        let task = self.spawn_silence_timer_task(
            target.clone(),
            previous.session_id,
            previous.window_id,
            generation,
            deadline,
        );
        timers.insert(
            target.clone(),
            SilenceTimerState {
                session_id: previous.session_id,
                window_id: previous.window_id,
                generation,
                deadline,
                task,
            },
        );
    }
}

fn arm_missing_timer(
    handler: &RequestHandler,
    timers: &mut HashMap<WindowTarget, SilenceTimerState>,
    original_generations: &HashMap<WindowTarget, u64>,
    desired: DesiredSilenceTimer,
) {
    let target = desired.target;
    if desired.seconds == 0 {
        if let Some(previous) = timers.remove(&target) {
            previous.task.abort();
        }
        return;
    }
    let existing_matches = timers.get(&target).is_some_and(|timer| {
        timer.session_id == desired.session_id && timer.window_id == desired.window_id
    });
    if existing_matches {
        return;
    }
    let previous_generation = timers
        .remove(&target)
        .map(|previous| {
            let generation = previous.generation;
            previous.task.abort();
            generation
        })
        .unwrap_or(0)
        .max(original_generations.get(&target).copied().unwrap_or(0));
    let generation = previous_generation.saturating_add(1).max(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(desired.seconds);
    let task = handler.spawn_silence_timer_task(
        target.clone(),
        desired.session_id,
        desired.window_id,
        generation,
        deadline,
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
}

fn stable_window_occurrence(
    state: &HandlerState,
    target: &WindowTarget,
    session_id: SessionId,
    window_id: WindowId,
) -> Option<StableWindowOccurrence> {
    let session = state
        .sessions
        .session(target.session_name())
        .filter(|session| session.id() == session_id)?;
    let matching_indices = session
        .windows()
        .iter()
        .filter_map(|(window_index, window)| (window.id() == window_id).then_some(*window_index))
        .collect::<Vec<_>>();
    let ordinal = matching_indices
        .iter()
        .position(|window_index| *window_index == target.window_index())?;
    Some(StableWindowOccurrence {
        ordinal,
        occurrence_count: matching_indices.len(),
    })
}

fn resolve_planned_window_target(
    state: &HandlerState,
    source: &WindowTarget,
    session_id: SessionId,
    window_id: WindowId,
    occurrence: Option<StableWindowOccurrence>,
    target_overrides: &HashMap<WindowTarget, Option<WindowTarget>>,
    reindexed_windows: &[(SessionName, BTreeMap<u32, u32>)],
) -> Option<WindowTarget> {
    if let Some(destination) = target_overrides.get(source) {
        return destination
            .as_ref()
            .filter(|destination| target_has_window_id(state, destination, window_id))
            .cloned();
    }

    let session = state
        .sessions
        .session(source.session_name())
        .filter(|session| session.id() == session_id)?;

    let explicit_index_map = reindexed_windows
        .iter()
        .find_map(|(session_name, index_map)| {
            (session_name == source.session_name()).then_some(index_map)
        });
    if let Some(destination_index) =
        explicit_index_map.and_then(|index_map| index_map.get(&source.window_index()).copied())
    {
        if session
            .window_at(destination_index)
            .is_some_and(|window| window.id() == window_id)
        {
            return Some(WindowTarget::with_window(
                source.session_name().clone(),
                destination_index,
            ));
        }
    }

    let matching_indices = session
        .windows()
        .iter()
        .filter_map(|(window_index, window)| (window.id() == window_id).then_some(*window_index))
        .collect::<Vec<_>>();

    // A supplied operation map is authoritative for every changed slot. An
    // unmapped slot therefore stayed put even if another duplicate alias moved
    // around it.
    if explicit_index_map.is_some()
        && session
            .window_at(source.window_index())
            .is_some_and(|window| window.id() == window_id)
    {
        return Some(source.clone());
    }

    // Pure insertion and renumber operations preserve duplicate occurrence
    // order. Resolve by that order before consulting the old numeric slot,
    // which may now contain a different occurrence of the same WindowId.
    if let Some(occurrence) = occurrence {
        if matching_indices.len() == occurrence.occurrence_count {
            return matching_indices
                .get(occurrence.ordinal)
                .copied()
                .map(|window_index| {
                    WindowTarget::with_window(source.session_name().clone(), window_index)
                });
        }
    }

    // Count changes are expected when a non-target alias is removed. In that
    // case, retaining the same index is stronger evidence than an ordinal that
    // may have shifted.
    if session
        .window_at(source.window_index())
        .is_some_and(|window| window.id() == window_id)
    {
        return Some(source.clone());
    }

    None
}

fn target_has_window_id(state: &HandlerState, target: &WindowTarget, window_id: WindowId) -> bool {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .is_some_and(|window| window.id() == window_id)
}

fn target_has_silence_flag(state: &HandlerState, target: &WindowTarget) -> bool {
    state
        .sessions
        .session(target.session_name())
        .is_some_and(|session| {
            session
                .winlink_alert_flags(target.window_index())
                .contains(WINLINK_SILENCE)
        })
}

fn target_group_contains_any(
    state: &HandlerState,
    target: &WindowTarget,
    candidates: &HashSet<WindowTarget>,
) -> bool {
    state
        .sessions
        .session_group_members(target.session_name())
        .into_iter()
        .any(|session_name| {
            candidates.contains(&WindowTarget::with_window(
                session_name,
                target.window_index(),
            ))
        })
}
