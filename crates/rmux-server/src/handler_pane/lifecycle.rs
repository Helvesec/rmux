use std::time::Duration;

use rmux_core::events::{OutputCursorItem, PaneOutputSubscriptionKey};
use rmux_core::LifecycleEvent;
use rmux_proto::{OptionName, PaneStateClosedReason, PaneTarget, RmuxError, Target, WindowTarget};

use super::super::{
    attach_support::SessionDetachOnDestroy, exited_output_support::RetainedExitedPaneIdentities,
    prepare_lifecycle_event, scripting_support::format_context_for_target, RequestHandler,
};
use super::pane_kill_effects::KillPaneLifecycleBatch;
use crate::format_runtime::render_runtime_template;
use crate::pane_io::{PaneExitCallback, PaneExitEvent, PaneOutputReceiver, PaneOutputSender};
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminals::{session_not_found, HandlerState, PaneExitMetadata};
use tracing::warn;

const PANE_EXIT_STATUS_RETRY_DELAY: Duration = Duration::from_millis(10);
const PANE_EXIT_STATUS_LONG_RETRY_DELAY: Duration = Duration::from_millis(250);
const PANE_EXIT_STATUS_FAST_RETRY_ATTEMPTS: usize = 20;
const DEAD_PANE_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

enum PaneExitPlan {
    Ignore,
    KeepDead {
        prepare_dead: bool,
        output_generation: u64,
    },
    RemovePane {
        runtime_session_name: rmux_proto::SessionName,
        runtime_session_id: rmux_proto::SessionId,
        target_session_id: rmux_proto::SessionId,
        target: PaneTarget,
        affected_sessions: Vec<rmux_proto::SessionName>,
        destroyed_sessions: Vec<(rmux_proto::SessionName, u32)>,
        destroyed_attached_sessions: Vec<(
            rmux_proto::SessionName,
            rmux_proto::SessionId,
            SessionDetachOnDestroy,
        )>,
        removed_pane_ids: Vec<rmux_core::PaneId>,
        pane_event: super::super::QueuedLifecycleEvent,
        lifecycle_events: Vec<super::super::QueuedLifecycleEvent>,
        layout_event: Option<Box<super::super::QueuedLifecycleEvent>>,
        output: ExitedPaneOutput,
        metadata: PaneExitMetadata,
    },
    RemoveSession {
        runtime_session_name: rmux_proto::SessionName,
        runtime_session_id: rmux_proto::SessionId,
        session_name: rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
        target: PaneTarget,
        removed_pane_ids: Vec<rmux_core::PaneId>,
        pane_event: super::super::QueuedLifecycleEvent,
        lifecycle_events: Vec<super::super::QueuedLifecycleEvent>,
        output: ExitedPaneOutput,
        metadata: PaneExitMetadata,
    },
}

struct ExitedPaneOutput {
    receiver: Option<PaneOutputReceiver>,
    sender: Option<PaneOutputSender>,
}

impl ExitedPaneOutput {
    fn capture(
        state: &HandlerState,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
    ) -> Self {
        let (receiver, sender) =
            state.runtime_pane_output_drain_handles(runtime_session_name, pane_id);
        Self { receiver, sender }
    }

    async fn ensure_eof(&mut self, generation: Option<u64>, output_eof_published: bool) -> bool {
        if output_eof_published {
            return true;
        }
        if wait_for_pane_output_eof(self.receiver.take()).await {
            return true;
        }
        if let Some(sender) = self.sender.as_ref() {
            let _ = sender.send_for_generation(generation, Vec::new());
        }
        false
    }

    fn sender(&self) -> Option<PaneOutputSender> {
        self.sender.clone()
    }
}

impl RequestHandler {
    pub(in crate::handler) fn pane_exit_callback(&self) -> PaneExitCallback {
        let handler = self.downgrade();
        let runtime = self.server_task_runtime();
        std::sync::Arc::new(move |event: PaneExitEvent| {
            let Some(handler) = handler.upgrade() else {
                return;
            };
            let task = async move {
                handler.handle_pane_exit_event(event).await;
            };
            if let Some(runtime) = &runtime {
                runtime.spawn(task);
            } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(task);
            } else {
                tracing::warn!("dropping pane exit event because no Tokio runtime is available");
            }
        })
    }

    pub(in crate::handler) async fn handle_pane_exit_event(&self, event: PaneExitEvent) {
        // On Windows the child watcher may report exit before the ConPTY reader
        // publishes its final bytes. Wait for that publication before draining
        // coalesced alerts, otherwise a trailing OSC 52 can be queued after the
        // pane has already been removed and will be discarded by the timer.
        let mut output = {
            let state = self.state.lock().await;
            let Some(runtime_session_name) = state.resolve_pane_event_runtime_session(
                &event.session_name,
                event.pane_id,
                event.generation,
            ) else {
                return;
            };
            ExitedPaneOutput::capture(&state, &runtime_session_name, event.pane_id)
        };
        if !event.output_eof_published() {
            self.notify_pane_exit_output_drain_started();
        }
        let output_eof_observed = output
            .ensure_eof(event.generation, event.output_eof_published())
            .await;
        self.flush_pending_pane_alert_for_exit(event.pane_id, event.generation)
            .await;
        let mut output = Some(output);
        let mut attempts = 0;
        let plan = loop {
            let plan = {
                let mut state = self.state.lock().await;
                let Some(runtime_session_name) = state.resolve_pane_event_runtime_session(
                    &event.session_name,
                    event.pane_id,
                    event.generation,
                ) else {
                    return;
                };
                let Some(target) =
                    state.pane_target_for_runtime_pane(&runtime_session_name, event.pane_id)
                else {
                    return;
                };
                let was_dead = state.pane_is_dead(target.session_name(), event.pane_id);
                let metadata =
                    match state.observe_runtime_pane_exit(&runtime_session_name, event.pane_id) {
                        Ok(Some(metadata)) => Some(metadata),
                        Ok(None) => None,
                        Err(error) => {
                            warn!(
                                session = %runtime_session_name,
                                pane_id = event.pane_id.as_u32(),
                                "failed to observe pane exit: {error}"
                            );
                            return;
                        }
                    };

                if let Some(metadata) = metadata {
                    if should_keep_dead_pane(&state, &target, metadata) {
                        Some(PaneExitPlan::KeepDead {
                            prepare_dead: !was_dead,
                            output_generation: state
                                .pane_output_generation(&runtime_session_name, event.pane_id),
                        })
                    } else {
                        let Some(session) = state.sessions.session(target.session_name()) else {
                            return;
                        };
                        let Some(window) = session.window_at(target.window_index()) else {
                            return;
                        };
                        let target_session_id = session.id();
                        let Some(runtime_session_id) = state
                            .sessions
                            .session(&runtime_session_name)
                            .map(rmux_core::Session::id)
                        else {
                            return;
                        };
                        let only_window_remaining = session.windows().len() == 1;
                        let only_pane_remaining = window.pane_count() == 1;
                        let linked_window = only_pane_remaining
                            && state.window_linked_session_count(
                                target.session_name(),
                                target.window_index(),
                            ) > 1;
                        let pane_id = window
                            .pane(target.pane_index())
                            .map(|pane| pane.id().as_u32())
                            .unwrap_or_else(|| event.pane_id.as_u32());
                        let window_id = window.id();
                        let window_name = window.name().unwrap_or_default().to_owned();
                        let _ = (session, window);
                        let lifecycle_batch =
                            KillPaneLifecycleBatch::capture(&state, &target, false);
                        let pane_event = prepare_lifecycle_event(
                            &mut state,
                            &LifecycleEvent::PaneExited {
                                target: target.clone(),
                                pane_id: Some(pane_id),
                                window_id: Some(window_id.as_u32()),
                                window_name: Some(window_name.clone()),
                            },
                        );
                        let detach_on_destroy = SessionDetachOnDestroy::capture_all(&state);

                        if only_window_remaining && only_pane_remaining && !linked_window {
                            let current_runtime_owner =
                                state.sessions.runtime_owner(target.session_name());
                            let next_runtime_owner = state
                                .sessions
                                .runtime_owner_transfer_target(target.session_name());
                            let timer_mutation =
                                self.plan_all_window_mutation_silence_timers_locked(&state);
                            let removed_session =
                                match state.sessions.remove_session(target.session_name()) {
                                    Ok(removed_session) => removed_session,
                                    Err(error) => {
                                        warn!(
                                            session = %target.session_name(),
                                            pane_id = event.pane_id.as_u32(),
                                            "failed to remove exited pane session: {error}"
                                        );
                                        return;
                                    }
                                };
                            state.retire_removed_lifecycle_targets();
                            let destroyed_sessions = vec![(
                                target.session_name().clone(),
                                removed_session.id().as_u32(),
                            )];
                            let lifecycle_events =
                                lifecycle_batch.prepare_committed(&mut state, &destroyed_sessions);
                            let _ = state.options.remove_session(target.session_name());
                            let _ = state.environment.remove_session(target.session_name());
                            if let Err(error) = state.remove_session_terminals(
                                target.session_name(),
                                current_runtime_owner.as_ref(),
                                next_runtime_owner.as_ref(),
                            ) {
                                warn!(
                                    session = %target.session_name(),
                                    pane_id = event.pane_id.as_u32(),
                                    "failed to remove exited pane runtime state: {error}"
                                );
                            }
                            self.record_pane_state_change(
                                event.pane_id,
                                event.generation,
                                PaneStateChange::Closed {
                                    reason: PaneStateClosedReason::Exited,
                                },
                            );
                            self.prune_web_panes(&[event.pane_id]);
                            self.prune_web_session(Some((
                                target.session_name().clone(),
                                removed_session.id(),
                            )));
                            self.apply_window_mutation_silence_timers_and_arm_all_locked(
                                &state,
                                timer_mutation,
                                Vec::new(),
                                &[],
                            );
                            Some(PaneExitPlan::RemoveSession {
                                runtime_session_name,
                                runtime_session_id,
                                session_name: target.session_name().clone(),
                                session_id: removed_session.id(),
                                detach_on_destroy: detach_on_destroy
                                    .get(&removed_session.id())
                                    .copied()
                                    .unwrap_or(SessionDetachOnDestroy::Detach),
                                target,
                                removed_pane_ids: vec![event.pane_id],
                                pane_event,
                                lifecycle_events,
                                output: output
                                    .take()
                                    .expect("pane exit output is consumed by one committed plan"),
                                metadata,
                            })
                        } else {
                            let timer_mutation =
                                self.plan_all_window_mutation_silence_timers_locked(&state);
                            match state.kill_pane(target.clone()) {
                                Ok(result) => {
                                    state.retire_removed_lifecycle_targets();
                                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                                        &state,
                                        timer_mutation,
                                        Vec::new(),
                                        &[],
                                    );
                                    if result.response.window_destroyed {
                                        let _ =
                                            state.hooks.remove_window(&WindowTarget::with_window(
                                                target.session_name().clone(),
                                                target.window_index(),
                                            ));
                                    } else {
                                        let _ = state.hooks.remove_pane(&target);
                                    }
                                    self.record_pane_state_change(
                                        event.pane_id,
                                        event.generation,
                                        PaneStateChange::Closed {
                                            reason: PaneStateClosedReason::Exited,
                                        },
                                    );
                                    self.prune_web_panes(&result.removed_pane_ids);
                                    let lifecycle_events = lifecycle_batch
                                        .prepare_committed(&mut state, &result.destroyed_sessions);
                                    let layout_event =
                                        (!result.response.window_destroyed).then(|| {
                                            prepare_lifecycle_event(
                                                &mut state,
                                                &LifecycleEvent::WindowLayoutChanged {
                                                    target: WindowTarget::with_window(
                                                        target.session_name().clone(),
                                                        target.window_index(),
                                                    ),
                                                },
                                            )
                                        });
                                    for (destroyed_session, session_id) in
                                        &result.destroyed_sessions
                                    {
                                        self.prune_web_session(Some((
                                            destroyed_session.clone(),
                                            rmux_core::SessionId::new(*session_id),
                                        )));
                                    }
                                    let mut affected_sessions = result.affected_sessions;
                                    state.expand_with_active_window_linked_session_families(
                                        &mut affected_sessions,
                                    );
                                    let destroyed_attached_sessions = result
                                        .destroyed_sessions
                                        .iter()
                                        .filter_map(|(session_name, session_id)| {
                                            let session_id =
                                                rmux_proto::SessionId::new(*session_id);
                                            detach_on_destroy.get(&session_id).copied().map(
                                                |policy| (session_name.clone(), session_id, policy),
                                            )
                                        })
                                        .collect();
                                    Some(PaneExitPlan::RemovePane {
                                        runtime_session_name,
                                        runtime_session_id,
                                        target_session_id,
                                        target,
                                        affected_sessions,
                                        destroyed_sessions: result.destroyed_sessions,
                                        destroyed_attached_sessions,
                                        removed_pane_ids: result.removed_pane_ids,
                                        pane_event,
                                        lifecycle_events,
                                        layout_event: layout_event.map(Box::new),
                                        output: output.take().expect(
                                            "pane exit output is consumed by one committed plan",
                                        ),
                                        metadata,
                                    })
                                }
                                Err(error) => {
                                    warn!(
                                        session = %target.session_name(),
                                        pane_id = event.pane_id.as_u32(),
                                        "failed to remove exited pane: {error}"
                                    );
                                    Some(PaneExitPlan::Ignore)
                                }
                            }
                        }
                    }
                } else {
                    None
                }
            };

            match plan {
                Some(plan) => break plan,
                None => {
                    // Linux can publish PTY EOF while the session leader keeps
                    // running with all three standard descriptors redirected.
                    // The reader owns Unix's only pane-exit notification, so a
                    // fixed retry window would discard that event permanently
                    // and leave the later child exit unreaped. Keep this
                    // generation-scoped task alive until the child exits or
                    // target resolution above proves the pane was replaced or
                    // removed. Preserve the low-latency retry window for normal
                    // exits, then poll slowly for deliberately detached PTYs.
                    if !cfg!(unix) && attempts >= PANE_EXIT_STATUS_FAST_RETRY_ATTEMPTS {
                        return;
                    }
                    let delay = if attempts < PANE_EXIT_STATUS_FAST_RETRY_ATTEMPTS {
                        PANE_EXIT_STATUS_RETRY_DELAY
                    } else {
                        PANE_EXIT_STATUS_LONG_RETRY_DELAY
                    };
                    attempts = attempts.saturating_add(1);
                    tokio::time::sleep(delay).await;
                }
            }
        };

        {
            let mut state = self.state.lock().await;
            crate::handler::prune_dead_hook_identities(&mut state);
        }
        self.pause_after_pane_exit_commit().await;

        match plan {
            PaneExitPlan::Ignore => {}
            PaneExitPlan::KeepDead {
                prepare_dead,
                output_generation,
            } => {
                let Some((target, pane_event)) = self
                    .commit_kept_dead_pane(
                        &event,
                        output_generation,
                        prepare_dead,
                        output_eof_observed,
                    )
                    .await
                else {
                    return;
                };
                self.emit_prepared(pane_event).await;
                let session_names = if self.attached_count(target.session_name()).await == 0 {
                    let mut state = self.state.lock().await;
                    let Some((_, current_target)) =
                        current_kept_dead_pane(&state, &event, output_generation)
                    else {
                        return;
                    };
                    match apply_dead_pane_automatic_window_name(&mut state, &current_target) {
                        Ok(session_names) => session_names,
                        Err(error) => {
                            warn!(
                                session = %current_target.session_name(),
                                pane_index = current_target.pane_index(),
                                "failed to update dead pane automatic window name: {error}"
                            );
                            vec![current_target.session_name().clone()]
                        }
                    }
                } else {
                    vec![target.session_name().clone()]
                };
                for session_name in session_names {
                    self.refresh_attached_session(&session_name).await;
                    self.refresh_control_session(&session_name).await;
                }
            }
            PaneExitPlan::RemovePane {
                runtime_session_name,
                runtime_session_id,
                target_session_id,
                target,
                affected_sessions,
                destroyed_sessions,
                destroyed_attached_sessions,
                removed_pane_ids,
                pane_event,
                lifecycle_events,
                layout_event,
                output,
                metadata,
            } => {
                self.retain_removed_pane_output(
                    &runtime_session_name,
                    event.pane_id,
                    &target,
                    RetainedExitedPaneIdentities::new(target_session_id, runtime_session_id),
                    &output,
                    metadata,
                )
                .await;
                self.forget_pane_snapshot_coalescers(&removed_pane_ids);
                self.cleanup_exited_pane_output_subscription(&runtime_session_name, event.pane_id)
                    .await;
                let mut prepared_attached_switches = std::collections::HashMap::new();
                let mut prepared_rehome_order = Vec::new();
                for (session_name, session_id, detach_on_destroy) in &destroyed_attached_sessions {
                    let prepared = self
                        .prepare_destroy_session_rehome(
                            session_name,
                            *session_id,
                            *detach_on_destroy,
                        )
                        .await;
                    prepared_rehome_order.push(*session_id);
                    prepared_attached_switches.insert(*session_id, prepared);
                }
                self.emit_prepared(pane_event).await;
                for lifecycle_event in lifecycle_events {
                    self.emit_prepared(lifecycle_event).await;
                }
                if let Some(layout_event) = layout_event {
                    self.emit_prepared(*layout_event).await;
                }
                for session_id in prepared_rehome_order {
                    if let Some(prepared) = prepared_attached_switches.get_mut(&session_id) {
                        for event in prepared.control_lifecycle_events.drain(..) {
                            self.emit_prepared(event).await;
                        }
                    }
                }
                let destroyed_names = destroyed_sessions
                    .iter()
                    .map(|(destroyed_session, _)| destroyed_session.clone())
                    .collect::<Vec<_>>();
                let destroyed_identities = destroyed_sessions
                    .iter()
                    .map(|(session_name, session_id)| {
                        (
                            session_name.clone(),
                            rmux_proto::SessionId::new(*session_id),
                        )
                    })
                    .collect::<Vec<_>>();
                self.remove_session_leases(&destroyed_identities);
                for (session_name, session_id, _detach_on_destroy) in destroyed_attached_sessions {
                    let prepared = prepared_attached_switches
                        .remove(&session_id)
                        .expect("pane exit destroy rehome must be prepared before publication");
                    self.exit_prepared_attached_session_identity(prepared).await;
                    self.cancel_session_silence_timers(&session_name).await;
                    self.refresh_control_session(&session_name).await;
                }
                for affected_session in affected_sessions {
                    if destroyed_names.contains(&affected_session) {
                        continue;
                    }
                    let _ = self
                        .reconcile_attached_session_size_and_emit(&affected_session)
                        .await;
                    self.refresh_attached_session(&affected_session).await;
                    self.refresh_control_session(&affected_session).await;
                }
                if !destroyed_names.is_empty() {
                    let _ = self.request_shutdown_if_server_empty().await;
                }
            }
            PaneExitPlan::RemoveSession {
                runtime_session_name,
                runtime_session_id,
                session_name,
                session_id,
                detach_on_destroy,
                target,
                removed_pane_ids,
                pane_event,
                lifecycle_events,
                output,
                metadata,
            } => {
                self.retain_removed_pane_output(
                    &runtime_session_name,
                    event.pane_id,
                    &target,
                    RetainedExitedPaneIdentities::new(session_id, runtime_session_id),
                    &output,
                    metadata,
                )
                .await;
                self.remove_session_leases(std::slice::from_ref(&(
                    session_name.clone(),
                    session_id,
                )));
                self.forget_pane_snapshot_coalescers(&removed_pane_ids);
                self.cleanup_exited_pane_output_subscription(&runtime_session_name, event.pane_id)
                    .await;
                let mut prepared = self
                    .prepare_destroy_session_rehome(&session_name, session_id, detach_on_destroy)
                    .await;
                self.emit_prepared(pane_event).await;
                for lifecycle_event in lifecycle_events {
                    self.emit_prepared(lifecycle_event).await;
                }
                for event in prepared.control_lifecycle_events.drain(..) {
                    self.emit_prepared(event).await;
                }
                self.exit_prepared_attached_session_identity(prepared).await;
                self.cancel_session_silence_timers(&session_name).await;
                self.refresh_control_session(&session_name).await;
                let _ = self.request_shutdown_if_server_empty().await;
            }
        }
    }

    async fn retain_removed_pane_output(
        &self,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
        target: &PaneTarget,
        identities: RetainedExitedPaneIdentities,
        output: &ExitedPaneOutput,
        metadata: PaneExitMetadata,
    ) {
        self.retain_exited_pane(
            target.clone(),
            PaneOutputSubscriptionKey::new(runtime_session_name.clone(), pane_id),
            identities,
            output.sender(),
            metadata,
        )
        .await;
    }

    async fn cleanup_exited_pane_output_subscription(
        &self,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
    ) {
        let key = PaneOutputSubscriptionKey::new(runtime_session_name.clone(), pane_id);
        self.drain_exited_pane_output_subscriptions(key).await;
    }

    async fn commit_kept_dead_pane(
        &self,
        event: &PaneExitEvent,
        output_generation: u64,
        prepare_dead: bool,
        output_eof_observed: bool,
    ) -> Option<(PaneTarget, super::super::QueuedLifecycleEvent)> {
        let output_rx = if prepare_dead && !output_eof_observed {
            let state = self.state.lock().await;
            let (runtime_session_name, _) =
                current_kept_dead_pane(&state, event, output_generation)?;
            state.subscribe_runtime_pane_output(&runtime_session_name, event.pane_id)
        } else {
            None
        };

        if output_rx.is_some() {
            // On Windows the child-exit watcher can beat the ConPTY reader.
            // Wait for the reader's EOF marker so a final echoed command can be
            // stripped before the dead-pane message is appended.
            wait_for_pane_output_eof(output_rx).await;
        }

        let mut state = self.state.lock().await;
        let (runtime_session_name, target) =
            current_kept_dead_pane(&state, event, output_generation)?;
        if prepare_dead {
            if let Err(error) =
                state.strip_attached_submitted_line(&runtime_session_name, event.pane_id)
            {
                warn!(
                    session = %runtime_session_name,
                    pane_id = event.pane_id.as_u32(),
                    "failed to strip attached submitted line for dead pane: {error}"
                );
            }
            if let Err(error) =
                append_remain_on_exit_message(&mut state, &runtime_session_name, &target)
            {
                warn!(
                    session = %runtime_session_name,
                    pane_id = event.pane_id.as_u32(),
                    "failed to append remain-on-exit message: {error}"
                );
            }
        }

        let (pane_id, window_id, window_name) =
            pane_lifecycle_identifiers(&state, &target, event.pane_id);
        let pane_event = prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::PaneDied {
                target: target.clone(),
                pane_id: Some(pane_id),
                window_id,
                window_name,
            },
        );
        self.record_pane_state_change(
            event.pane_id,
            Some(output_generation),
            PaneStateChange::Closed {
                reason: PaneStateClosedReason::DiedKept,
            },
        );
        Some((target, pane_event))
    }
}

fn current_kept_dead_pane(
    state: &HandlerState,
    event: &PaneExitEvent,
    output_generation: u64,
) -> Option<(rmux_proto::SessionName, PaneTarget)> {
    let runtime_session_name = state.resolve_pane_event_runtime_session(
        &event.session_name,
        event.pane_id,
        Some(output_generation),
    )?;
    let target = state.pane_target_for_runtime_pane(&runtime_session_name, event.pane_id)?;
    state
        .pane_is_dead(target.session_name(), event.pane_id)
        .then_some((runtime_session_name, target))
}

async fn wait_for_pane_output_eof(output_rx: Option<PaneOutputReceiver>) -> bool {
    let Some(mut output_rx) = output_rx else {
        return false;
    };
    tokio::time::timeout(DEAD_PANE_OUTPUT_DRAIN_TIMEOUT, async move {
        loop {
            match output_rx.recv().await {
                OutputCursorItem::Event(event) if event.bytes().is_empty() => break,
                OutputCursorItem::Event(_) | OutputCursorItem::Gap(_) => {}
            }
        }
    })
    .await
    .is_ok()
}

fn should_keep_dead_pane(
    state: &HandlerState,
    target: &PaneTarget,
    metadata: PaneExitMetadata,
) -> bool {
    match state
        .options
        .resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::RemainOnExit,
        )
        .unwrap_or("off")
    {
        "on" | "key" => true,
        "failed" => metadata.signal.is_some() || metadata.status.is_some_and(|status| status != 0),
        _ => false,
    }
}

fn pane_lifecycle_identifiers(
    state: &HandlerState,
    target: &PaneTarget,
    fallback_pane_id: rmux_core::PaneId,
) -> (u32, Option<u32>, Option<String>) {
    let Some(session) = state.sessions.session(target.session_name()) else {
        return (fallback_pane_id.as_u32(), None, None);
    };
    let Some(window) = session.window_at(target.window_index()) else {
        return (fallback_pane_id.as_u32(), None, None);
    };
    let pane_id = window
        .pane(target.pane_index())
        .map(|pane| pane.id().as_u32())
        .unwrap_or_else(|| fallback_pane_id.as_u32());
    (
        pane_id,
        Some(window.id().as_u32()),
        Some(window.name().unwrap_or_default().to_owned()),
    )
}

fn append_remain_on_exit_message(
    state: &mut HandlerState,
    runtime_session_name: &rmux_proto::SessionName,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    let template = state
        .options
        .resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::RemainOnExitFormat,
        )
        .unwrap_or_default();
    if template.is_empty() {
        return Ok(());
    }

    let runtime = format_context_for_target(state, &Target::Pane(target.clone()), 0)?;
    let rendered = render_runtime_template(template, &runtime, false);
    if rendered.is_empty() {
        return Ok(());
    }

    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            missing_pane_terminal(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
        })?;
    let rows = state
        .transcript_handle(target)?
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .clone_screen()
        .size()
        .rows
        .max(1);
    let mut bytes = format!("\x1b[{rows};1H\n\x1b[2K").into_bytes();
    bytes.extend_from_slice(rendered.as_bytes());
    state.append_bytes_to_runtime_pane_transcript(runtime_session_name, pane_id, &bytes)
}

fn apply_dead_pane_automatic_window_name(
    state: &mut HandlerState,
    target: &PaneTarget,
) -> Result<Vec<rmux_proto::SessionName>, RmuxError> {
    let rendered = state
        .pane_runtime_window_name_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.ends_with("[dead]") {
                value
            } else {
                format!("{value}[dead]")
            }
        })
        .unwrap_or_default();
    if rendered.is_empty() {
        return Ok(vec![target.session_name().clone()]);
    }

    let tracked = state.tracks_auto_named_window(target.session_name(), target.window_index());
    let should_update = {
        let session = state
            .sessions
            .session(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))?;
        let window = session.window_at(target.window_index()).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{}", target.session_name(), target.window_index()),
                "window index does not exist in session",
            )
        })?;
        window.name() != Some(rendered.as_str())
            && crate::automatic_rename::window_allows_automatic_rename(
                &state.options,
                target.session_name(),
                target.window_index(),
                window,
                tracked,
            )
    };
    if !should_update {
        return Ok(vec![target.session_name().clone()]);
    }

    state
        .sessions
        .session_mut(target.session_name())
        .expect("existing session must accept automatic rename update")
        .window_at_mut(target.window_index())
        .expect("existing window must accept automatic rename update")
        .set_automatic_name(rendered);
    state.mark_auto_named_window(target.session_name(), target.window_index());
    state.synchronize_linked_window_from_slot(target.session_name(), target.window_index())?;
    Ok(state
        .synchronize_session_group_from(target.session_name())
        .unwrap_or_else(|_| vec![target.session_name().clone()]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_eof_skips_timeout_when_exit_event_already_published_eof() {
        let sender = crate::pane_io::pane_output_channel();
        let generation = None;
        sender
            .send_for_generation(generation, Vec::new())
            .expect("matching generation should accept EOF marker");

        let receiver = sender.subscribe();
        let mut output = ExitedPaneOutput {
            receiver: Some(receiver),
            sender: Some(sender),
        };

        tokio::time::timeout(
            Duration::from_millis(25),
            output.ensure_eof(generation, true),
        )
        .await
        .expect("published EOF should not wait for the drain timeout");
    }
}
