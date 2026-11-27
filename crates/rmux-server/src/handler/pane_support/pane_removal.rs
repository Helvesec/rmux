use super::super::{
    attach_support::SessionDetachOnDestroy, focus_support::prepare_server_focus_effects,
    subscription_support::capture_pane_stream_sources, RequestHandler, SelectionTransitionSnapshot,
};
use super::pane_key_retirement::KeyDismissal;
use super::pane_kill_effects::{after_kill_pane_target, KillPaneLifecycleBatch};
use crate::hook_runtime::PendingInlineHookFormat;
use rmux_proto::{ErrorResponse, HookName, PaneTarget, Response, SessionId, WindowTarget};

pub(super) enum PaneRemovalReason {
    Command,
    Key(KeyDismissal),
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_kill_pane(
        &self,
        request: rmux_proto::KillPaneRequest,
    ) -> Response {
        self.remove_pane(
            request.target,
            request.kill_all_except,
            PaneRemovalReason::Command,
        )
        .await
        .expect("explicit removal is never an abandoned key admission")
    }

    pub(super) async fn remove_pane(
        &self,
        target: PaneTarget,
        kill_all_except: bool,
        reason: PaneRemovalReason,
    ) -> Option<Response> {
        let session_name = target.session_name().clone();
        let (
            response,
            lifecycle_events,
            affected_sessions,
            destroyed_sessions,
            destroyed_attached_sessions,
            removed_subscription_keys,
            removed_pane_ids,
            after_hook_target,
            focus_effects,
            post_lifecycle_events,
        ) = {
            let mut state = self.state.lock().await;
            if let Err(error) = super::super::require_expected_pane_identity(&state, &target) {
                return Some(Response::Error(ErrorResponse { error }));
            }
            let mut active_attach = self.active_attach.lock().await;
            if let PaneRemovalReason::Key(intent) = &reason {
                if !intent.is_current(&state, &active_attach, &target) {
                    return None;
                }
            }
            let detach_on_destroy = SessionDetachOnDestroy::capture_all(&state);
            let hook_batch = KillPaneLifecycleBatch::capture(&state, &target, kill_all_except);
            let removed_subscription_keys = state
                .pane_output_subscription_keys_for_kill(&target, kill_all_except)
                .unwrap_or_default();
            let removed_stream_sources =
                capture_pane_stream_sources(&state, &removed_subscription_keys);
            let timer_mutation = self.plan_all_window_mutation_silence_timers_locked(&state);
            let selection_before = SelectionTransitionSnapshot::capture(&state);
            match state.kill_pane_with_options(target.clone(), kill_all_except) {
                Ok(result) => {
                    self.stage_removed_pane_stream_sources(
                        &removed_subscription_keys,
                        removed_stream_sources,
                    );
                    state.retire_removed_lifecycle_targets();
                    self.apply_window_mutation_silence_timers_and_arm_all_locked(
                        &state,
                        timer_mutation,
                        Vec::new(),
                        &result.reindexed_windows,
                    );
                    let mut affected_sessions = result.affected_sessions.clone();
                    state.expand_with_active_window_linked_session_families(&mut affected_sessions);
                    let destroyed_sessions = result.destroyed_sessions.clone();
                    let destroyed_attached_sessions = destroyed_sessions
                        .iter()
                        .filter_map(|(session_name, session_id)| {
                            let session_id = SessionId::new(*session_id);
                            detach_on_destroy
                                .get(&session_id)
                                .copied()
                                .map(|policy| (session_name.clone(), session_id, policy))
                        })
                        .collect::<Vec<_>>();
                    let after_hook_target =
                        after_kill_pane_target(&state, &result.hook_context, &affected_sessions);
                    let layout_target =
                        WindowTarget::with_window(session_name.clone(), target.window_index());
                    let focus_effects =
                        prepare_server_focus_effects(&mut state, &mut active_attach);
                    let (lifecycle_events, post_lifecycle_events) = hook_batch
                        .prepare_explicit_committed(
                            &mut state,
                            &destroyed_sessions,
                            &selection_before,
                            layout_target,
                            result.response.window_destroyed,
                        );
                    if !result.session_destroyed && !result.response.window_destroyed {
                        let _ = state.hooks.remove_pane(&target);
                    }
                    self.record_panes_closed_as_killed(&result.removed_pane_ids);
                    (
                        Response::KillPane(result.response),
                        lifecycle_events,
                        affected_sessions,
                        destroyed_sessions,
                        destroyed_attached_sessions,
                        removed_subscription_keys,
                        result.removed_pane_ids,
                        after_hook_target,
                        Some(focus_effects),
                        post_lifecycle_events,
                    )
                }
                Err(error) => (
                    Response::Error(ErrorResponse { error }),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    None,
                    None,
                    Vec::new(),
                ),
            }
        };

        if matches!(reason, PaneRemovalReason::Command) && matches!(response, Response::KillPane(_))
        {
            match after_hook_target {
                Some(target) => self.queue_exact_pane_inline_hook(
                    HookName::AfterKillPane,
                    target.target,
                    target.identity,
                    PendingInlineHookFormat::AfterCommand,
                ),
                None => self.queue_missing_target_inline_hook(
                    HookName::AfterKillPane,
                    PendingInlineHookFormat::AfterCommand,
                ),
            }
        }

        for (destroyed_session, session_id) in &destroyed_sessions {
            self.prune_web_session(Some((
                destroyed_session.clone(),
                SessionId::new(*session_id),
            )));
        }

        if !removed_pane_ids.is_empty() {
            self.forget_pane_snapshot_coalescers(&removed_pane_ids);
        }
        let mut prepared_attached_switches = std::collections::HashMap::new();
        if matches!(response, Response::KillPane(_)) {
            for (session_name, session_id, detach_on_destroy) in &destroyed_attached_sessions {
                let prepared = self
                    .prepare_destroy_session_rehome(session_name, *session_id, *detach_on_destroy)
                    .await;
                prepared_attached_switches.insert(*session_id, prepared);
            }
        }
        if let Some(focus_effects) = focus_effects {
            let _ = focus_effects.execute(self).await;
        }
        for event in lifecycle_events {
            self.emit_prepared(event).await;
        }
        for (_, session_id, _) in &destroyed_attached_sessions {
            if let Some(prepared) = prepared_attached_switches.get_mut(session_id) {
                for event in prepared.control_lifecycle_events.drain(..) {
                    self.emit_prepared(event).await;
                }
            }
        }
        for event in post_lifecycle_events {
            self.emit_prepared(event).await;
        }
        if matches!(response, Response::KillPane(_)) {
            self.drain_removed_pane_output_subscriptions(&removed_subscription_keys)
                .await;
            let destroyed_names = destroyed_sessions
                .iter()
                .map(|(destroyed_session, _)| destroyed_session.clone())
                .collect::<Vec<_>>();
            let destroyed_identities = destroyed_sessions
                .iter()
                .map(|(session_name, session_id)| {
                    (session_name.clone(), SessionId::new(*session_id))
                })
                .collect::<Vec<_>>();
            self.remove_session_leases(&destroyed_identities);
            for (session_name, session_id, _detach_on_destroy) in destroyed_attached_sessions {
                let prepared = prepared_attached_switches
                    .remove(&session_id)
                    .expect("kill-pane destroy rehome must be prepared before publication");
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
                self.dismiss_mode_tree_for_session(&affected_session).await;
                self.refresh_attached_session(&affected_session).await;
            }
            if !destroyed_names.is_empty() {
                let _ = self.queue_shutdown_if_server_empty().await;
            }
        }

        Some(response)
    }
}
