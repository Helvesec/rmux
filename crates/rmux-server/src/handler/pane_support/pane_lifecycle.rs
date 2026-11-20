use std::time::Duration;

use rmux_core::events::{OutputCursorItem, PaneOutputSubscriptionKey};
use rmux_core::LifecycleEvent;
use rmux_proto::{PaneStateClosedReason, RmuxError, WindowTarget};

use super::super::{
    attach_support::SessionDetachOnDestroy,
    defer_lifecycle_event,
    exited_output_support::RetainedExitedPaneIdentities,
    focus_support::prepare_server_focus_effects,
    pane_stream_support::{stream_source_for_target, PaneStreamSource},
    prepare_lifecycle_event, RequestHandler, SelectionTransitionSnapshot,
};
use super::pane_kill_effects::KillPaneLifecycleBatch;
use crate::pane_io::{PaneExitCallback, PaneExitEvent, PaneOutputReceiver, PaneOutputSender};
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminals::HandlerState;
use tracing::warn;

mod dead_pane_footer;
mod exit_publication;
mod kept_dead_pane;
use exit_publication::PaneExitPlan;
use kept_dead_pane::should_keep_dead_pane;

const PANE_EXIT_STATUS_RETRY_DELAY: Duration = Duration::from_millis(10);
const PANE_EXIT_STATUS_LONG_RETRY_DELAY: Duration = Duration::from_millis(250);
const PANE_EXIT_STATUS_FAST_RETRY_ATTEMPTS: usize = 20;
const PANE_EXIT_TEARDOWN_RETRY_ATTEMPTS: usize = 32;
const DEAD_PANE_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(250);

/// Outcome of one attempt to plan an exited pane's teardown under the state
/// lock.
enum PaneExitAttempt {
    /// The teardown committed; the plan is published outside the lock.
    Planned(Box<PaneExitPlan>),
    /// The child's exit is not observable yet, so nothing has been torn down.
    ExitPending,
    /// The pane's process is gone but the model teardown failed. The state was
    /// rolled back, so the attempt can be repeated; the payload is the terminal
    /// fallback to commit once the retry budget is spent.
    TeardownFailed { output_generation: u64 },
}

struct ExitedPaneOutput {
    receiver: Option<PaneOutputReceiver>,
    sender: Option<PaneOutputSender>,
    stream_source: Option<PaneStreamSource>,
}

impl ExitedPaneOutput {
    fn capture(
        state: &HandlerState,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
    ) -> Self {
        let (receiver, sender) =
            state.runtime_pane_output_drain_handles(runtime_session_name, pane_id);
        let stream_source = state
            .pane_target_for_runtime_pane(runtime_session_name, pane_id)
            .and_then(|target| stream_source_for_target(state, target).ok());
        Self {
            receiver,
            sender,
            stream_source,
        }
    }

    async fn ensure_eof(&mut self, output_eof_published: bool) -> bool {
        if output_eof_published {
            return true;
        }
        if wait_for_pane_output_eof(self.receiver.take()).await {
            return true;
        }
        false
    }

    fn sender(&self) -> Option<PaneOutputSender> {
        self.sender.clone()
    }

    fn stream_source(&self) -> Option<PaneStreamSource> {
        self.stream_source.clone()
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
        let output_eof_observed = output.ensure_eof(event.output_eof_published()).await;
        self.flush_pending_pane_alert_for_exit(event.pane_id, event.generation)
            .await;
        let mut output = Some(output);
        let mut attempts = 0;
        let mut teardown_failures = 0;
        let plan = loop {
            let attempt = 'attempt: {
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
                    let output_generation =
                        state.pane_output_generation(&runtime_session_name, event.pane_id);
                    if should_keep_dead_pane(&state, &target, metadata) {
                        PaneExitAttempt::Planned(Box::new(PaneExitPlan::KeepDead {
                            output_generation,
                        }))
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
                            .map(|session| session.id())
                        else {
                            return;
                        };
                        let only_window_remaining = session.windows().count() == 1;
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
                        // Snapshot exit formats for the removal event, but keep
                        // the visible pane alive if teardown has to be retried.
                        let (mut lifecycle_batch, deferred_pane_event) = state
                            .with_pane_exit_context(
                                &runtime_session_name,
                                event.pane_id,
                                |state| {
                                    let batch =
                                        KillPaneLifecycleBatch::capture(state, &target, false);
                                    let event = defer_lifecycle_event(
                                        state,
                                        &LifecycleEvent::PaneExited {
                                            target: target.clone(),
                                            pane_id: Some(pane_id),
                                            window_id: Some(window_id.as_u32()),
                                            window_name: Some(window_name.clone()),
                                        },
                                    );
                                    (batch, event)
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
                                        warn_pane_exit_teardown_failure(
                                            teardown_failures,
                                            target.session_name(),
                                            event.pane_id,
                                            &error,
                                        );
                                        break 'attempt PaneExitAttempt::TeardownFailed {
                                            output_generation,
                                        };
                                    }
                                };
                            if let Some(source) =
                                output.as_ref().and_then(ExitedPaneOutput::stream_source)
                            {
                                self.stage_exited_pane_stream_source(
                                    PaneOutputSubscriptionKey::new(
                                        runtime_session_name.clone(),
                                        event.pane_id,
                                    ),
                                    source,
                                );
                            }
                            // Same critical section as the removal: an exit probe sees
                            // the live pane or its retained exit, never neither.
                            self.retain_exited_pane_locked(
                                &state,
                                target.clone(),
                                PaneOutputSubscriptionKey::new(
                                    runtime_session_name.clone(),
                                    event.pane_id,
                                ),
                                RetainedExitedPaneIdentities::new(
                                    removed_session.id(),
                                    runtime_session_id,
                                ),
                                output.as_ref().and_then(ExitedPaneOutput::sender),
                                metadata,
                            );
                            state.retire_removed_lifecycle_targets();
                            let destroyed_sessions = vec![(
                                target.session_name().clone(),
                                removed_session.id().as_u32(),
                            )];
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
                            let mut active_attach = self.active_attach.lock().await;
                            let focus_effects =
                                prepare_server_focus_effects(&mut state, &mut active_attach);
                            let pane_event =
                                lifecycle_batch.prepare_deferred(&mut state, deferred_pane_event);
                            let lifecycle_events =
                                lifecycle_batch.prepare_committed(&mut state, &destroyed_sessions);
                            PaneExitAttempt::Planned(Box::new(PaneExitPlan::RemoveSession {
                                runtime_session_name,
                                session_name: target.session_name().clone(),
                                session_id: removed_session.id(),
                                detach_on_destroy: detach_on_destroy
                                    .get(&removed_session.id())
                                    .copied()
                                    .unwrap_or(SessionDetachOnDestroy::Detach),
                                removed_pane_ids: vec![event.pane_id],
                                focus_effects,
                                pane_event,
                                lifecycle_events,
                                output: output
                                    .take()
                                    .expect("pane exit output is consumed by one committed plan"),
                            }))
                        } else {
                            let timer_mutation =
                                self.plan_all_window_mutation_silence_timers_locked(&state);
                            let selection_before = SelectionTransitionSnapshot::capture(&state);
                            match state.kill_pane(target.clone()) {
                                Ok(result) => {
                                    if let Some(source) =
                                        output.as_ref().and_then(ExitedPaneOutput::stream_source)
                                    {
                                        self.stage_exited_pane_stream_source(
                                            PaneOutputSubscriptionKey::new(
                                                runtime_session_name.clone(),
                                                event.pane_id,
                                            ),
                                            source,
                                        );
                                    }
                                    // Same critical section as the removal: an exit probe sees
                                    // the live pane or its retained exit, never neither.
                                    self.retain_exited_pane_locked(
                                        &state,
                                        target.clone(),
                                        PaneOutputSubscriptionKey::new(
                                            runtime_session_name.clone(),
                                            event.pane_id,
                                        ),
                                        RetainedExitedPaneIdentities::new(
                                            target_session_id,
                                            runtime_session_id,
                                        ),
                                        output.as_ref().and_then(ExitedPaneOutput::sender),
                                        metadata,
                                    );
                                    state.retire_removed_lifecycle_targets();
                                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                                        &state,
                                        timer_mutation,
                                        Vec::new(),
                                        &result.reindexed_windows,
                                    );
                                    if !result.response.window_destroyed {
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
                                    let window_destroyed = result.response.window_destroyed;
                                    let mut active_attach = self.active_attach.lock().await;
                                    let focus_effects = prepare_server_focus_effects(
                                        &mut state,
                                        &mut active_attach,
                                    );
                                    let pane_event = lifecycle_batch
                                        .prepare_deferred(&mut state, deferred_pane_event);
                                    let lifecycle_events = if window_destroyed {
                                        lifecycle_batch.prepare_window_destroyed_committed(
                                            &mut state,
                                            &result.destroyed_sessions,
                                            &selection_before,
                                        )
                                    } else {
                                        lifecycle_batch.prepare_committed(
                                            &mut state,
                                            &result.destroyed_sessions,
                                        )
                                    };
                                    let layout_target = WindowTarget::with_window(
                                        target.session_name().clone(),
                                        target.window_index(),
                                    );
                                    let layout_event = (!window_destroyed).then(|| {
                                        prepare_lifecycle_event(
                                            &mut state,
                                            &LifecycleEvent::WindowLayoutChanged {
                                                target: layout_target.clone(),
                                            },
                                        )
                                    });
                                    let selection_events = if window_destroyed {
                                        Vec::new()
                                    } else {
                                        selection_before.prepare_window_pane_changes(
                                            &mut state,
                                            std::slice::from_ref(&layout_target),
                                        )
                                    };
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
                                    PaneExitAttempt::Planned(Box::new(PaneExitPlan::RemovePane {
                                        runtime_session_name,
                                        affected_sessions,
                                        destroyed_sessions: result.destroyed_sessions,
                                        destroyed_attached_sessions,
                                        removed_pane_ids: result.removed_pane_ids,
                                        focus_effects,
                                        pane_event,
                                        lifecycle_events,
                                        layout_event: layout_event.map(Box::new),
                                        selection_events,
                                        output: output.take().expect(
                                            "pane exit output is consumed by one committed plan",
                                        ),
                                    }))
                                }
                                Err(error) => {
                                    warn_pane_exit_teardown_failure(
                                        teardown_failures,
                                        target.session_name(),
                                        event.pane_id,
                                        &error,
                                    );
                                    PaneExitAttempt::TeardownFailed { output_generation }
                                }
                            }
                        }
                    }
                } else {
                    PaneExitAttempt::ExitPending
                }
            };

            match attempt {
                PaneExitAttempt::Planned(plan) => break *plan,
                PaneExitAttempt::TeardownFailed { output_generation } => {
                    // kill_pane rolls its own mutations back, so the pane is
                    // still whole and the attempt can simply be repeated. The
                    // process is already gone, so giving up would strand the
                    // pane in the model with no terminal event for the journal,
                    // the SDK or lifecycle subscribers. Once the budget is
                    // spent, commit the kept-dead representation instead: the
                    // pane stays visible, but it is honestly marked dead and
                    // every subscriber observes it closing.
                    let delay = pane_exit_retry_delay(teardown_failures);
                    teardown_failures = teardown_failures.saturating_add(1);
                    if teardown_failures >= PANE_EXIT_TEARDOWN_RETRY_ATTEMPTS {
                        break PaneExitPlan::KeepDead { output_generation };
                    }
                    tokio::time::sleep(delay).await;
                }
                PaneExitAttempt::ExitPending => {
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
                    let delay = pane_exit_retry_delay(attempts);
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

        self.publish_pane_exit_plan(event, plan, output_eof_observed)
            .await;
    }
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

fn pane_exit_retry_delay(attempts: usize) -> Duration {
    if attempts < PANE_EXIT_STATUS_FAST_RETRY_ATTEMPTS {
        PANE_EXIT_STATUS_RETRY_DELAY
    } else {
        PANE_EXIT_STATUS_LONG_RETRY_DELAY
    }
}

/// Logs the first failure of a retried teardown, then stays quiet so a pane
/// that keeps failing cannot flood the log for the whole retry budget.
fn warn_pane_exit_teardown_failure(
    teardown_failures: usize,
    session_name: &rmux_proto::SessionName,
    pane_id: rmux_core::PaneId,
    error: &RmuxError,
) {
    if teardown_failures > 0 {
        return;
    }
    warn!(
        session = %session_name,
        pane_id = pane_id.as_u32(),
        "failed to remove exited pane, retrying: {error}"
    );
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
            stream_source: None,
        };

        tokio::time::timeout(Duration::from_millis(25), output.ensure_eof(true))
            .await
            .expect("published EOF should not wait for the drain timeout");
    }

    #[tokio::test]
    async fn ensure_eof_timeout_does_not_precede_late_reader_output() {
        let sender = crate::pane_io::pane_output_channel();
        let generation = None;
        let receiver = sender.subscribe();
        let mut stream_receiver = sender.subscribe();
        let mut output = ExitedPaneOutput {
            receiver: Some(receiver),
            sender: Some(sender.clone()),
            stream_source: None,
        };

        assert!(!output.ensure_eof(false).await);
        sender
            .send_for_generation(generation, b"late-conpty-tail".to_vec())
            .expect("late output must remain publishable");
        sender
            .send_for_generation(generation, Vec::new())
            .expect("reader EOF must remain publishable");

        let OutputCursorItem::Event(tail) = stream_receiver.recv().await else {
            panic!("late output must not be preceded by a synthetic EOF");
        };
        assert_eq!(tail.bytes(), b"late-conpty-tail");
        let OutputCursorItem::Event(eof) = stream_receiver.recv().await else {
            panic!("reader EOF must follow late output");
        };
        assert!(eof.bytes().is_empty());
    }
}
