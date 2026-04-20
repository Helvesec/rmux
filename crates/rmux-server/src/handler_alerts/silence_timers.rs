//! Silence alert timer synchronization and expiry handling.

use std::time::Duration;

use rmux_core::WINDOW_SILENCE;
use rmux_proto::{types::OptionScopeSelector, SessionName, WindowTarget};

use super::super::RequestHandler;
use super::{monitor_silence_seconds, SilenceTimerState};

impl RequestHandler {
    pub(in crate::handler) async fn sync_session_silence_timers(&self, session_name: &SessionName) {
        let desired = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return;
            };
            session
                .windows()
                .keys()
                .copied()
                .map(|window_index| {
                    (
                        WindowTarget::with_window(session_name.clone(), window_index),
                        monitor_silence_seconds(&state.options, session_name, window_index),
                    )
                })
                .collect::<Vec<_>>()
        };

        self.reconcile_silence_timers_for_session(session_name, desired);
    }

    pub(in crate::handler) async fn sync_all_silence_timers(&self) {
        let desired = {
            let state = self.state.lock().await;
            state
                .sessions
                .iter()
                .flat_map(|(session_name, session)| {
                    session.windows().keys().copied().map(|window_index| {
                        (
                            WindowTarget::with_window(session_name.clone(), window_index),
                            monitor_silence_seconds(&state.options, session_name, window_index),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };

        let existing = {
            let timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            timers.keys().cloned().collect::<Vec<_>>()
        };
        for target in existing {
            if !desired.iter().any(|(candidate, _)| candidate == &target) {
                self.remove_silence_timer(&target);
            }
        }
        for (target, seconds) in desired {
            self.configure_silence_timer(target, seconds);
        }
    }

    pub(in crate::handler) async fn cancel_session_silence_timers(
        &self,
        session_name: &SessionName,
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
            self.remove_silence_timer(&target);
        }
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
                self.sync_session_silence_timers(target.session_name())
                    .await;
            }
            OptionScopeSelector::Pane(target) => {
                self.sync_session_silence_timers(target.session_name())
                    .await;
            }
            OptionScopeSelector::ServerGlobal
            | OptionScopeSelector::SessionGlobal
            | OptionScopeSelector::WindowGlobal => {
                self.sync_all_silence_timers().await;
            }
        }
    }

    pub(super) fn configure_silence_timer(&self, target: WindowTarget, seconds: u64) {
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
        if seconds == 0 {
            return;
        }

        let handler = self.clone();
        let target_clone = target.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            handler
                .handle_silence_timer_expired(target_clone, generation)
                .await;
        });
        timers.insert(target, SilenceTimerState { generation, task });
    }

    fn remove_silence_timer(&self, target: &WindowTarget) {
        let mut timers = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned");
        if let Some(previous) = timers.remove(target) {
            previous.task.abort();
        }
    }

    async fn handle_silence_timer_expired(&self, target: WindowTarget, generation: u64) {
        let should_fire = {
            let mut timers = self
                .silence_timers
                .lock()
                .expect("silence timer mutex must not be poisoned");
            match timers.get(&target) {
                Some(state) if state.generation == generation => {
                    // Remove without aborting — we are inside the expired task itself.
                    timers.remove(&target);
                    true
                }
                _ => false,
            }
        };
        if should_fire {
            self.alerts_queue_window(target, WINDOW_SILENCE).await;
        }
    }

    fn reconcile_silence_timers_for_session(
        &self,
        session_name: &SessionName,
        desired: Vec<(WindowTarget, u64)>,
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
            if !desired.iter().any(|(candidate, _)| candidate == &target) {
                self.remove_silence_timer(&target);
            }
        }
        for (target, seconds) in desired {
            self.configure_silence_timer(target, seconds);
        }
    }
}
