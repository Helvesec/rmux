//! Shutdown-safe ownership for delayed silence timers and their alert effects.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use rmux_core::WINDOW_SILENCE;
use rmux_proto::{SessionId, WindowId, WindowTarget};

use super::super::super::lifecycle_producer_tasks::{
    begin_current_lifecycle_mutation, LifecycleMutationGuard, LifecycleProducerLane,
    LifecycleProducerRegistration,
};
use super::super::super::RequestHandler;
use super::super::SilenceTimerState;
use super::DesiredSilenceTimer;

pub(super) struct SilenceTimerTaskReservations {
    // Field order is intentional: guards must drop before registrations validate mutation depth.
    _handoffs: Vec<LifecycleMutationGuard>,
    registrations: VecDeque<LifecycleProducerRegistration>,
}

impl SilenceTimerTaskReservations {
    pub(super) fn take(&mut self) -> LifecycleProducerRegistration {
        self.registrations
            .pop_front()
            .expect("silence timer task admission was reserved")
    }
}

impl RequestHandler {
    pub(super) fn reserve_silence_timer_tasks(
        &self,
        count: usize,
    ) -> Option<SilenceTimerTaskReservations> {
        let mut registrations = VecDeque::with_capacity(count);
        for _ in 0..count {
            let registration = self
                .lifecycle_producers
                .try_register_in_lane(LifecycleProducerLane::Normal)?;
            registrations.push_back(registration);
        }

        let mut handoffs = Vec::with_capacity(count);
        for registration in &registrations {
            handoffs.push(registration.try_begin_mutation()?);
        }

        Some(SilenceTimerTaskReservations {
            _handoffs: handoffs,
            registrations,
        })
    }

    pub(super) fn spawn_silence_timer_task(
        &self,
        target: WindowTarget,
        session_id: SessionId,
        window_id: WindowId,
        generation: u64,
        deadline: tokio::time::Instant,
        registration: LifecycleProducerRegistration,
    ) -> tokio::task::JoinHandle<()> {
        let handler = self.downgrade();
        let cleanup_handler = handler.clone();
        let cleanup_target = target.clone();
        let timer = async move {
            tokio::time::sleep_until(deadline).await;
            if let Some(handler) = handler.upgrade() {
                handler
                    .handle_silence_timer_expired(target, session_id, window_id, generation)
                    .await;
            }
        };
        let cleanup = async move {
            let Some(handler) = cleanup_handler.upgrade() else {
                return;
            };
            let mut timers = handler
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            let owns_timer = timers.get(&cleanup_target).is_some_and(|timer| {
                timer.session_id == session_id
                    && timer.window_id == window_id
                    && timer.generation == generation
            });
            if owns_timer {
                let _ = timers.remove(&cleanup_target);
            }
        };
        self.spawn_pre_admitted_lifecycle_producer_task_with_cleanup_handle(
            "rmux-silence-timer",
            registration,
            timer,
            cleanup,
        )
    }

    pub(in crate::handler) fn remove_silence_timer(&self, target: &WindowTarget) {
        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        if let Some(previous) = timers.remove(target) {
            previous.task.abort();
        }
    }

    pub(super) fn arm_missing_silence_timer(
        &self,
        timers: &mut HashMap<WindowTarget, SilenceTimerState>,
        original_generations: &HashMap<WindowTarget, u64>,
        desired: DesiredSilenceTimer,
        timer_reservations: &mut SilenceTimerTaskReservations,
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
    }

    pub(super) async fn handle_silence_timer_expired(
        &self,
        target: WindowTarget,
        session_id: SessionId,
        window_id: WindowId,
        generation: u64,
    ) {
        let attached_count = self.attached_count(target.session_name()).await;
        let Some(mutation) = begin_current_lifecycle_mutation() else {
            return;
        };
        let prepared = {
            let mut state = self.state.lock().await;
            #[cfg(test)]
            self.pause_before_silence_timer_apply();
            let target_identity_matches = state
                .sessions
                .session(target.session_name())
                .filter(|session| session.id() == session_id)
                .and_then(|session| session.window_at(target.window_index()))
                .is_some_and(|window| window.id() == window_id);
            if !target_identity_matches {
                return;
            }

            let mut timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            let timer_matches = timers.get(&target).is_some_and(|timer| {
                timer.session_id == session_id
                    && timer.window_id == window_id
                    && timer.generation == generation
            });
            if !timer_matches {
                return;
            }

            let publication = state.track_unordered_lifecycle_publication();
            debug_assert!(
                publication.is_some(),
                "silence expiry reached a sealed publication boundary"
            );
            let Some(publication) = publication else {
                return;
            };
            // Remove without aborting: this is the task that owns the matching timer.
            let _ = timers.remove(&target);
            drop(timers);
            let plans =
                self.alerts_queue_window_locked(&mut state, target, WINDOW_SILENCE, attached_count);
            (plans, publication)
        };

        let (plans, publication) = prepared;
        if plans.is_empty() {
            drop(publication);
            drop(mutation);
            return;
        }

        let handler = self.clone();
        tokio::spawn(async move {
            handler.execute_alert_plans(plans).await;
            drop(publication);
        });
        // The publication guard now owns every effect from this committed batch.
        drop(mutation);
    }
}
