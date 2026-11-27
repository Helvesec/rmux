use rmux_core::events::PaneOutputSubscriptionKey;

use super::super::super::{
    attach_support::SessionDetachOnDestroy, focus_support::FocusEffects, QueuedLifecycleEvent,
    RequestHandler,
};
use super::kept_dead_pane::{apply_dead_pane_automatic_window_name, current_kept_dead_pane};
use super::ExitedPaneOutput;
use crate::pane_io::PaneExitEvent;
use tracing::warn;

pub(super) enum PaneExitPlan {
    KeepDead {
        output_generation: u64,
    },
    RemovePane {
        runtime_session_name: rmux_proto::SessionName,
        affected_sessions: Vec<rmux_proto::SessionName>,
        destroyed_sessions: Vec<(rmux_proto::SessionName, u32)>,
        destroyed_attached_sessions: Vec<(
            rmux_proto::SessionName,
            rmux_proto::SessionId,
            SessionDetachOnDestroy,
        )>,
        removed_pane_ids: Vec<rmux_core::PaneId>,
        focus_effects: FocusEffects,
        pane_event: QueuedLifecycleEvent,
        lifecycle_events: Vec<QueuedLifecycleEvent>,
        layout_event: Option<Box<QueuedLifecycleEvent>>,
        selection_events: Vec<QueuedLifecycleEvent>,
        output: ExitedPaneOutput,
    },
    RemoveSession {
        runtime_session_name: rmux_proto::SessionName,
        session_name: rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        detach_on_destroy: SessionDetachOnDestroy,
        removed_pane_ids: Vec<rmux_core::PaneId>,
        focus_effects: FocusEffects,
        pane_event: QueuedLifecycleEvent,
        lifecycle_events: Vec<QueuedLifecycleEvent>,
        output: ExitedPaneOutput,
    },
}

impl RequestHandler {
    pub(super) async fn publish_pane_exit_plan(
        &self,
        event: PaneExitEvent,
        plan: PaneExitPlan,
        output_eof_observed: bool,
    ) {
        match plan {
            PaneExitPlan::KeepDead { output_generation } => {
                let Some((target, pane_event)) = self
                    .commit_kept_dead_pane(&event, output_generation, output_eof_observed)
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
                affected_sessions,
                destroyed_sessions,
                destroyed_attached_sessions,
                removed_pane_ids,
                focus_effects,
                pane_event,
                lifecycle_events,
                layout_event,
                selection_events,
                output,
            } => {
                let _ = focus_effects.execute(self).await;
                self.forget_pane_snapshot_coalescers(&removed_pane_ids);
                self.cleanup_exited_pane_output_subscription(
                    &runtime_session_name,
                    event.pane_id,
                    &output,
                )
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
                for selection_event in selection_events {
                    self.emit_prepared(selection_event).await;
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
                session_name,
                session_id,
                detach_on_destroy,
                removed_pane_ids,
                focus_effects,
                pane_event,
                lifecycle_events,
                output,
            } => {
                let _ = focus_effects.execute(self).await;
                self.remove_session_leases(std::slice::from_ref(&(
                    session_name.clone(),
                    session_id,
                )));
                self.forget_pane_snapshot_coalescers(&removed_pane_ids);
                self.cleanup_exited_pane_output_subscription(
                    &runtime_session_name,
                    event.pane_id,
                    &output,
                )
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

    async fn cleanup_exited_pane_output_subscription(
        &self,
        runtime_session_name: &rmux_proto::SessionName,
        pane_id: rmux_core::PaneId,
        output: &ExitedPaneOutput,
    ) {
        let key = PaneOutputSubscriptionKey::new(runtime_session_name.clone(), pane_id);
        self.drain_exited_pane_output_subscriptions(key, output.stream_source())
            .await;
    }
}
