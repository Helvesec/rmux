//! Stable pane-output alert preparation and deferred dispatch.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rmux_core::{AlertFlags, PaneId, WINDOW_ACTIVITY, WINDOW_BELL};
use rmux_proto::{PaneTarget, SessionId, SessionName, WindowId, WindowTarget};

use super::super::{
    sequence_prepared_lifecycle_event, QueuedLifecycleEvent, RequestHandler,
    UnsequencedLifecycleEvent,
};
use super::pane_alert_coalescer::{flush_delay, pause_before_flush_dispatch};
use crate::pane_io::{PaneAlertCallback, PaneAlertEvent, PaneEffectSink};
use crate::pane_state_journal::PaneStateChange;

mod prepare;
mod window_renames;

#[derive(Debug, Clone, PartialEq, Eq)]
struct StablePaneAlertIdentity {
    event_runtime_session: SessionName,
    pane_id: PaneId,
    generation: Option<u64>,
    window_id: WindowId,
}

#[derive(Debug)]
pub(in crate::handler) struct PreparedPaneAlertEvent {
    identity: StablePaneAlertIdentity,
    inactive_refresh_sessions: Vec<SessionId>,
    clipboard_writes: Vec<Vec<u8>>,
    lifecycle_events: Vec<UnsequencedLifecycleEvent>,
    alert_flags: Option<AlertFlags>,
}

/// What one locked rename transaction produced.
///
/// The names are already applied and the lifecycle events already sequenced
/// when this value exists: everything that decides an order happened inside
/// the transaction, and only the publication and the client refreshes are left
/// to do outside it.
#[derive(Debug, Default)]
pub(in crate::handler) struct AppliedWindowRenames {
    refresh_sessions: HashSet<SessionName>,
    lifecycle_events: Vec<QueuedLifecycleEvent>,
}

impl AppliedWindowRenames {
    fn is_empty(&self) -> bool {
        self.refresh_sessions.is_empty() && self.lifecycle_events.is_empty()
    }
}

#[derive(Debug, Default)]
struct PendingStableWindowAlert {
    events: Vec<(StablePaneAlertIdentity, AlertFlags)>,
}

impl StablePaneAlertIdentity {
    fn capture(
        state: &crate::pane_terminals::HandlerState,
        event_runtime_session: SessionName,
        pane_target: &PaneTarget,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Option<Self> {
        let session = state.sessions.session(pane_target.session_name())?;
        let window = session.window_at(pane_target.window_index())?;
        (window.pane(pane_target.pane_index())?.id() == pane_id).then_some(Self {
            event_runtime_session,
            pane_id,
            generation,
            window_id: window.id(),
        })
    }

    fn resolve(&self, state: &crate::pane_terminals::HandlerState) -> Option<PaneTarget> {
        let runtime_session = state.resolve_pane_event_runtime_session(
            &self.event_runtime_session,
            self.pane_id,
            self.generation,
        )?;
        let pane_target = state.pane_target_for_runtime_pane(&runtime_session, self.pane_id)?;
        let window = state
            .sessions
            .session(pane_target.session_name())?
            .window_at(pane_target.window_index())?;
        (window.id() == self.window_id
            && window.pane(pane_target.pane_index())?.id() == self.pane_id)
            .then_some(pane_target)
    }
}

impl RequestHandler {
    pub(in crate::handler) fn pane_alert_callback(&self) -> PaneAlertCallback {
        let handler = self.downgrade();
        let runtime = tokio::runtime::Handle::current();
        let pending_alerts = Arc::clone(&self.pane_alert_coalescer);
        let pane_renames = Arc::clone(&self.pane_renames);
        Arc::new(PaneEffectSink::new(
            move |mut event: PaneAlertEvent| {
                let Some(handler) = handler.upgrade() else {
                    return;
                };
                let clipboard_queries = std::mem::take(&mut event.clipboard_queries);
                if !clipboard_queries.is_empty()
                    && handler.enqueue_pane_clipboard_queries(
                        event.session_name.clone(),
                        event.pane_id,
                        event.generation,
                        clipboard_queries,
                    )
                {
                    let query_handler = handler.clone();
                    runtime.spawn(async move {
                        query_handler.drain_pane_clipboard_queries().await;
                    });
                }
                if let Some((old, new)) = event
                    .title_change
                    .clone()
                    .filter(|_| handler.pane_state_has_title_subscriptions())
                {
                    handler.record_pane_state_change(
                        event.pane_id,
                        event.generation,
                        PaneStateChange::TitleChanged { old, new },
                    );
                }
                let disconnected = handler.try_relay_visible_inactive_pane_clipboard(&event);
                if !disconnected.is_empty() {
                    let cleanup_handler = handler.clone();
                    runtime.spawn(async move {
                        for identity in disconnected {
                            cleanup_handler
                                .finish_attach(identity.attach_pid(), identity.attach_id())
                                .await;
                        }
                    });
                }
                let should_spawn = {
                    let mut pending_alerts = pending_alerts
                        .lock()
                        .expect("pane alert coalescer mutex must not be poisoned");
                    pending_alerts.push(event)
                };
                if should_spawn {
                    let delay = flush_delay();
                    let pending_alerts = Arc::clone(&pending_alerts);
                    runtime.spawn(async move {
                        delay.await;
                        pause_before_flush_dispatch(&pending_alerts).await;
                        let _dispatch = handler.pane_alert_dispatch.lock().await;
                        let (prepared_events, renames) = handler
                            .take_and_prepare_pending_pane_alert_events(&pending_alerts)
                            .await;
                        let inactive_output_refreshes = handler
                            .apply_prepared_pane_alert_events(prepared_events, renames)
                            .await;
                        for session_name in inactive_output_refreshes {
                            handler
                                .refresh_attached_session_for_pane_output(&session_name)
                                .await;
                        }
                    });
                }
            },
            pane_renames,
        ))
    }

    #[cfg(test)]
    pub(in crate::handler) async fn handle_pane_alert_event(&self, mut event: PaneAlertEvent) {
        let clipboard_queries = std::mem::take(&mut event.clipboard_queries);
        if !clipboard_queries.is_empty() {
            self.handle_pane_clipboard_queries(
                event.session_name.clone(),
                event.pane_id,
                event.generation,
                clipboard_queries,
            )
            .await;
        }
        for identity in self.try_relay_visible_inactive_pane_clipboard(&event) {
            self.finish_attach(identity.attach_pid(), identity.attach_id())
                .await;
        }
        for session_name in self
            .handle_pane_alert_events_deferred_refresh(vec![event])
            .await
        {
            self.refresh_attached_session_for_pane_output(&session_name)
                .await;
        }
    }

    #[cfg(test)]
    pub(in crate::handler) fn pane_rename_queue_for_test(
        &self,
    ) -> &Arc<crate::pane_rename_queue::PaneRenameQueue> {
        &self.pane_renames
    }

    #[cfg(test)]
    async fn handle_pane_alert_events_deferred_refresh(
        &self,
        events: Vec<PaneAlertEvent>,
    ) -> HashSet<SessionName> {
        let mut prepared_events = Vec::new();
        for event in events {
            if let Some(prepared) = self.prepare_pane_alert_event(event).await {
                prepared_events.push(prepared);
            }
        }
        let applied = {
            let mut state = self.state.lock().await;
            let requests = self.pane_renames.drain();
            Self::apply_window_rename_sequence_locked(&mut state, requests)
        };
        self.apply_prepared_pane_alert_events(prepared_events, applied)
            .await
    }

    async fn take_and_prepare_pending_pane_alert_events(
        &self,
        pending_alerts: &std::sync::Mutex<super::pane_alert_coalescer::PaneAlertCoalescer>,
    ) -> (Vec<PreparedPaneAlertEvent>, AppliedWindowRenames) {
        // Take the state lock before removing coalesced events. Pane exit uses
        // the same ordering, so an event is either prepared against the live
        // pane here or drained into the exit transaction before removal.
        let mut state = self.state.lock().await;
        let events = pending_alerts
            .lock()
            .expect("pane alert coalescer mutex must not be poisoned")
            .take_pending();
        // The whole rename sequence leaves with this batch, in one piece and
        // in order, and is applied here, under the same lock that took it. The
        // caller holds the dispatch lock across this call, so an exit cannot
        // interleave its own prefix with this suffix.
        let requests = self.pane_renames.drain();
        let applied = Self::apply_window_rename_sequence_locked(&mut state, requests);
        let prepared = events
            .into_iter()
            .filter_map(|event| self.prepare_pane_alert_event_locked(&mut state, event))
            .collect();
        (prepared, applied)
    }

    pub(in crate::handler) async fn flush_pending_pane_alert_for_exit(
        &self,
        pane_id: PaneId,
        generation: Option<u64>,
    ) {
        let (prepared, applied, silence_resets) = {
            // EOF publication has already drained every reader event for this
            // pane. Serialize only the coalescer take and state-backed
            // preparation. Reset silence while the observation order is still
            // serialized, then release the dispatch lock before lifecycle
            // hooks that may wait on arbitrary commands.
            let _dispatch = self.pane_alert_dispatch.lock().await;
            let mut state = self.state.lock().await;
            let pending = self
                .pane_alert_coalescer
                .lock()
                .expect("pane alert coalescer mutex must not be poisoned")
                .take_for_pane_generation(pane_id, generation);
            let prepared = pending
                .and_then(|event| self.prepare_pane_alert_event_locked(&mut state, event))
                .into_iter()
                .collect::<Vec<_>>();
            // This pane's own requests cannot leave the sequence alone: taking
            // the prefix through its generation carries every request another
            // pane accepted earlier with it, so an exit never applies the
            // sequence out of the order it was observed in. Anything accepted
            // afterwards stays for the ordinary flush.
            let requests = self.pane_renames.drain_through(pane_id, generation);
            // Extraction and application are one transaction. Releasing the
            // locks between them would let a waiting flush apply the suffix
            // first and take the earlier publication ranks with it, which no
            // later sequencing can repair. The hooks are still waited for
            // outside both locks, below.
            self.pause_at_pane_exit_rename_boundary().await;
            let applied = Self::apply_window_rename_sequence_locked(&mut state, requests);
            let silence_resets =
                self.reset_prepared_pane_alert_silence_locked(&mut state, &prepared);
            (prepared, applied, silence_resets)
        };
        if prepared.is_empty() && applied.is_empty() {
            return;
        }
        let refreshes = self
            .apply_prepared_pane_alert_events_before_exit(prepared, applied, silence_resets)
            .await;
        for session_name in refreshes {
            self.refresh_attached_session_for_pane_output(&session_name)
                .await;
        }
    }

    fn reset_prepared_pane_alert_silence_locked(
        &self,
        state: &mut crate::pane_terminals::HandlerState,
        prepared_events: &[PreparedPaneAlertEvent],
    ) -> HashSet<WindowId> {
        let mut reset_windows = HashSet::new();
        for prepared in prepared_events {
            let has_activity = prepared
                .alert_flags
                .as_ref()
                .is_some_and(|flags| flags.intersects(WINDOW_ACTIVITY.union(WINDOW_BELL)));
            if !has_activity || reset_windows.contains(&prepared.identity.window_id) {
                continue;
            }
            let Some(pane_target) = prepared.identity.resolve(state) else {
                continue;
            };
            let target = WindowTarget::with_window(
                pane_target.session_name().clone(),
                pane_target.window_index(),
            );
            if self.reset_window_family_silence_locked(state, &target) {
                reset_windows.insert(prepared.identity.window_id);
            }
        }
        reset_windows
    }

    pub(in crate::handler) async fn apply_prepared_pane_alert_events(
        &self,
        prepared_events: Vec<PreparedPaneAlertEvent>,
        applied_renames: AppliedWindowRenames,
    ) -> HashSet<SessionName> {
        self.apply_prepared_pane_alert_events_with_dispatch(
            prepared_events,
            applied_renames,
            false,
            HashSet::new(),
        )
        .await
    }

    async fn apply_prepared_pane_alert_events_before_exit(
        &self,
        prepared_events: Vec<PreparedPaneAlertEvent>,
        applied_renames: AppliedWindowRenames,
        silence_resets: HashSet<WindowId>,
    ) -> HashSet<SessionName> {
        self.apply_prepared_pane_alert_events_with_dispatch(
            prepared_events,
            applied_renames,
            true,
            silence_resets,
        )
        .await
    }

    async fn apply_prepared_pane_alert_events_with_dispatch(
        &self,
        prepared_events: Vec<PreparedPaneAlertEvent>,
        applied_renames: AppliedWindowRenames,
        wait_for_lifecycle_hooks: bool,
        silence_resets: HashSet<WindowId>,
    ) -> HashSet<SessionName> {
        // The names are already applied and their events already sequenced,
        // inside the transaction that took them. What is left is to publish
        // those events - outside every lock, because a hook may run arbitrary
        // commands - before the batch's other effects, so a hook that reads
        // `#{window_name}` sees what the pane asked for.
        let AppliedWindowRenames {
            refresh_sessions: mut window_rename_refreshes,
            lifecycle_events: rename_events,
        } = applied_renames;
        for lifecycle_event in rename_events {
            if wait_for_lifecycle_hooks {
                self.emit_prepared_and_wait(lifecycle_event).await;
            } else {
                self.emit_prepared(lifecycle_event).await;
            }
        }
        let mut inactive_refresh_session_ids = HashSet::new();
        let mut window_alerts = HashMap::<WindowId, PendingStableWindowAlert>::new();
        for prepared in prepared_events {
            inactive_refresh_session_ids.extend(prepared.inactive_refresh_sessions);
            // tmux stores an inbound OSC 52 write before queuing the
            // pane-set-clipboard hook, so the hook can observe the new buffer.
            for content in prepared.clipboard_writes {
                let _ = self.store_buffer(None, content).await;
            }
            // Buffer storage publishes its own lifecycle events. Do not reserve
            // pane-alert positions until those writes finish, otherwise the
            // buffer publication can wait behind an event this task has not
            // emitted yet.
            for lifecycle_event in prepared.lifecycle_events {
                let lifecycle_event = {
                    let mut state = self.state.lock().await;
                    sequence_prepared_lifecycle_event(&mut state, lifecycle_event)
                };
                if wait_for_lifecycle_hooks {
                    self.emit_prepared_and_wait(lifecycle_event).await;
                } else {
                    self.emit_prepared(lifecycle_event).await;
                }
            }
            if let Some(flags) = prepared.alert_flags {
                window_alerts
                    .entry(prepared.identity.window_id)
                    .or_default()
                    .events
                    .push((prepared.identity, flags));
            }
        }

        self.pause_before_pane_alert_final_apply().await;

        let attached_counts = self.attached_counts_snapshot().await;
        let (plans, automatic_name_refreshes, automatic_rename_events, inactive_output_refreshes) = {
            let mut state = self.state.lock().await;
            let mut plans = Vec::new();
            let mut automatic_name_refreshes = HashSet::new();
            let mut automatic_rename_events = Vec::new();
            for (window_id, pending) in window_alerts {
                let silence_was_reset = silence_resets.contains(&window_id);
                let mut flags = AlertFlags::empty();
                let mut representative_identity = None;
                for (pane_identity, event_flags) in pending.events {
                    if pane_identity.resolve(&state).is_none() {
                        continue;
                    }
                    flags = flags.union(event_flags);
                    representative_identity.get_or_insert(pane_identity);
                }
                let Some(representative_identity) = representative_identity else {
                    continue;
                };
                let Some(pane_target) = representative_identity.resolve(&state) else {
                    continue;
                };
                let target = WindowTarget::with_window(
                    pane_target.session_name().clone(),
                    pane_target.window_index(),
                );

                if let Some(change) = Self::sync_automatic_window_name_for_window_target_locked(
                    &mut state, &target, window_id,
                ) {
                    automatic_name_refreshes.extend(change.refresh_sessions);
                    automatic_rename_events.push(change.lifecycle_event);
                }

                // Name synchronization may update linked/grouped models. Resolve again before
                // assigning flags or preparing alert hooks.
                let Some(pane_target) = representative_identity.resolve(&state) else {
                    continue;
                };
                let target = WindowTarget::with_window(
                    pane_target.session_name().clone(),
                    pane_target.window_index(),
                );
                let family_targets = state
                    .window_linked_window_targets(target.session_name(), target.window_index())
                    .into_iter()
                    .filter(|family_target| {
                        state
                            .sessions
                            .session(family_target.session_name())
                            .and_then(|session| session.window_at(family_target.window_index()))
                            .is_some_and(|window| window.id() == window_id)
                    })
                    .collect::<Vec<_>>();
                // Each winlink owns alert flags and actions, while activity resets the shared
                // window family's silence timers only once per coalesced pane-output batch.
                for (position, family_target) in family_targets.into_iter().enumerate() {
                    let attached_count = attached_counts
                        .get(family_target.session_name())
                        .copied()
                        .unwrap_or(0);
                    let family_plans = if position == 0 && !silence_was_reset {
                        self.alerts_queue_window_locked(
                            &mut state,
                            family_target,
                            flags,
                            attached_count,
                        )
                    } else {
                        self.alerts_queue_window_locked_without_silence_reset(
                            &mut state,
                            family_target,
                            flags,
                            attached_count,
                        )
                    };
                    plans.extend(family_plans);
                }
            }
            let inactive_output_refreshes = inactive_refresh_session_ids
                .into_iter()
                .filter_map(|session_id| {
                    state
                        .sessions
                        .session_by_id(session_id)
                        .map(|session| session.name().clone())
                })
                .collect::<HashSet<_>>();
            (
                plans,
                automatic_name_refreshes,
                automatic_rename_events,
                inactive_output_refreshes,
            )
        };

        for lifecycle_event in automatic_rename_events {
            let lifecycle_event = {
                let mut state = self.state.lock().await;
                sequence_prepared_lifecycle_event(&mut state, lifecycle_event)
            };
            if wait_for_lifecycle_hooks {
                self.emit_prepared_and_wait(lifecycle_event).await;
            } else {
                self.emit_prepared(lifecycle_event).await;
            }
        }
        window_rename_refreshes.extend(automatic_name_refreshes);
        for session_name in window_rename_refreshes {
            self.refresh_attached_session_for_pane_output(&session_name)
                .await;
        }
        self.execute_alert_plans(plans).await;
        inactive_output_refreshes
    }

    async fn attached_counts_snapshot(&self) -> HashMap<SessionName, usize> {
        let mut counts = HashMap::<SessionName, usize>::new();
        {
            let active_attach = self.active_attach.lock().await;
            for active in active_attach
                .by_pid
                .values()
                .filter(|active| !active.suspended)
            {
                counts
                    .entry(active.session_name.clone())
                    .and_modify(|count| *count = count.saturating_add(1))
                    .or_insert(1);
            }
        }
        {
            let active_control = self.active_control.lock().await;
            for session_name in active_control
                .by_pid
                .values()
                .filter_map(|active| active.session_name.as_ref())
            {
                counts
                    .entry(session_name.clone())
                    .and_modify(|count| *count = count.saturating_add(1))
                    .or_insert(1);
            }
        }
        counts
    }
}
