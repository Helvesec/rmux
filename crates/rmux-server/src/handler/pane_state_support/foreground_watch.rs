//! Lifecycle ownership and polling for pane foreground subscriptions.

use std::sync::atomic::Ordering;

use rmux_core::PaneId;
use rmux_proto::{ForegroundStateDto, RmuxError};

use crate::foreground_probe::{
    capture_foreground_probe_seed, probe_foreground, ForegroundProbeSeed,
};

use super::super::lifecycle_producer_tasks::{
    begin_current_lifecycle_mutation, LifecycleProducerLane, LifecycleProducerRegistration,
};
use super::super::{RequestHandler, WeakRequestHandler};
use super::{
    foreground_change_from_previous, foreground_poll_batch, pane_target_for_pane_id,
    FOREGROUND_POLL_INTERVAL,
};

const FOREGROUND_WATCH_TASK_NAME: &str = "rmux-pane-foreground-watch";

impl RequestHandler {
    pub(super) fn reserve_foreground_watch(
        &self,
    ) -> Result<LifecycleProducerRegistration, RmuxError> {
        self.lifecycle_producers
            .try_register_in_lane(LifecycleProducerLane::Normal)
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "lifecycle producer '{FOREGROUND_WATCH_TASK_NAME}' rejected during server shutdown"
                ))
            })
    }

    pub(super) fn start_foreground_watch_if_needed(
        &self,
        registration: LifecycleProducerRegistration,
    ) {
        if self
            .foreground_watch_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            drop(registration);
            return;
        }

        let watch_handler = self.downgrade();
        let cleanup_handler = watch_handler.clone();
        self.spawn_pre_admitted_lifecycle_producer_task_with_cleanup(
            FOREGROUND_WATCH_TASK_NAME,
            registration,
            run_foreground_watch(watch_handler),
            async move {
                if let Some(handler) = cleanup_handler.upgrade() {
                    handler.stop_foreground_watch_for_shutdown();
                }
            },
        );
    }

    /// Returns the currently watched panes, or atomically retires an idle watcher.
    ///
    /// The journal lock spans the zero-subscriber decision and the `started = false`
    /// publication. A new subscription therefore either keeps this watcher alive or
    /// observes `false` and starts its own pre-admitted watcher.
    fn foreground_panes_or_stop(&self) -> Option<Vec<PaneId>> {
        let journal = self.lock_pane_state_journal();
        if journal.foreground_subscription_count() != 0 {
            return Some(journal.pane_ids_with_foreground_subscriptions());
        }

        #[cfg(test)]
        self.pause_before_foreground_watch_idle_reset();

        let _mutation = begin_current_lifecycle_mutation()?;
        self.foreground_watch_started
            .store(false, Ordering::Release);
        self.clear_foreground_state_cache();
        None
    }

    /// Cancellation cleanup runs only after the normal producer lane has closed, so
    /// no new foreground subscription can have reserved a replacement watcher.
    fn stop_foreground_watch_for_shutdown(&self) {
        let _journal = self.lock_pane_state_journal();
        self.foreground_watch_started
            .store(false, Ordering::Release);
        self.clear_foreground_state_cache();
    }

    /// Captures every OS-derived field before the lifecycle mutation begins. Process
    /// metadata reads can block on the host; only the cache/journal commit is guarded.
    async fn collect_foreground_observation(
        &self,
        pane_id: PaneId,
    ) -> Option<(ForegroundProbeSeed, ForegroundStateDto)> {
        #[cfg(debug_assertions)]
        debug_assert!(
            !super::super::lifecycle_producer_tasks::current_lifecycle_producer_is_mutating(),
            "foreground OS probing must run outside a lifecycle mutation"
        );

        let seed = {
            let state = self.state.lock().await;
            let target = pane_target_for_pane_id(&state, pane_id)?;
            capture_foreground_probe_seed(&state, &target).ok()?
        };
        let observation = probe_foreground(&seed);
        Some((seed, observation))
    }

    fn commit_foreground_observation(
        &self,
        pane_id: PaneId,
        seed: ForegroundProbeSeed,
        next: ForegroundStateDto,
    ) -> bool {
        let Some(_mutation) = begin_current_lifecycle_mutation() else {
            return false;
        };
        let previous =
            self.replace_foreground_state_cache(pane_id, seed.generation(), next.clone());
        if let Some(previous) = foreground_change_from_previous(previous, seed.generation(), &next)
        {
            self.record_pane_state_change(
                pane_id,
                Some(seed.generation()),
                super::PaneStateChange::ForegroundChanged {
                    old: previous,
                    new: next,
                },
            );
        }
        true
    }

    async fn poll_foreground_subscriptions_once(&self, cursor: &mut usize) -> bool {
        let Some(pane_ids) = self.foreground_panes_or_stop() else {
            return false;
        };
        for pane_id in foreground_poll_batch(&pane_ids, cursor) {
            let Some((seed, next)) = self.collect_foreground_observation(pane_id).await else {
                continue;
            };
            if !self.commit_foreground_observation(pane_id, seed, next) {
                return false;
            }
        }
        true
    }
}

async fn run_foreground_watch(handler: WeakRequestHandler) {
    let mut cursor = 0;
    loop {
        let Some(handler) = handler.upgrade() else {
            return;
        };
        if !handler
            .poll_foreground_subscriptions_once(&mut cursor)
            .await
        {
            return;
        }
        drop(handler);
        tokio::time::sleep(FOREGROUND_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod test_support {
    use std::sync::{Arc, Condvar, Mutex};

    use tokio::sync::Notify;

    use super::RequestHandler;

    #[derive(Debug, Default)]
    pub(in crate::handler) struct ForegroundWatchIdlePause {
        pub(in crate::handler) reached: Notify,
        released: Mutex<bool>,
        release: Condvar,
    }

    impl ForegroundWatchIdlePause {
        pub(in crate::handler) fn release(&self) {
            *self
                .released
                .lock()
                .expect("foreground watcher idle pause lock") = true;
            self.release.notify_one();
        }

        fn wait_in_watcher(&self) {
            self.reached.notify_one();
            let mut released = self
                .released
                .lock()
                .expect("foreground watcher idle pause lock");
            while !*released {
                released = self
                    .release
                    .wait(released)
                    .expect("foreground watcher idle pause lock");
            }
        }
    }

    static FOREGROUND_WATCH_IDLE_PAUSES: Mutex<Vec<(usize, Arc<ForegroundWatchIdlePause>)>> =
        Mutex::new(Vec::new());

    impl RequestHandler {
        pub(in crate::handler) fn install_foreground_watch_idle_pause(
            &self,
        ) -> Arc<ForegroundWatchIdlePause> {
            let handler_key = Arc::as_ptr(&self.pane_state_journal) as usize;
            let pause = Arc::new(ForegroundWatchIdlePause::default());
            let mut pauses = FOREGROUND_WATCH_IDLE_PAUSES
                .lock()
                .expect("foreground watcher idle pause registry lock");
            assert!(
                !pauses.iter().any(|(key, _)| *key == handler_key),
                "foreground watcher idle pause already installed"
            );
            pauses.push((handler_key, Arc::clone(&pause)));
            pause
        }

        pub(super) fn pause_before_foreground_watch_idle_reset(&self) {
            let handler_key = Arc::as_ptr(&self.pane_state_journal) as usize;
            let pause = {
                let mut pauses = FOREGROUND_WATCH_IDLE_PAUSES
                    .lock()
                    .expect("foreground watcher idle pause registry lock");
                pauses
                    .iter()
                    .position(|(key, _)| *key == handler_key)
                    .map(|position| pauses.swap_remove(position).1)
            };
            if let Some(pause) = pause {
                pause.wait_in_watcher();
            }
        }
    }
}
