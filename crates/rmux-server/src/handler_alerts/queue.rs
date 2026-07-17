use rmux_core::{AlertFlags, WINDOW_ACTIVITY, WINDOW_BELL, WINDOW_SILENCE, WINLINK_SILENCE};
use rmux_proto::WindowTarget;

use super::super::prepare_lifecycle_event;
use super::{
    alert_action, alert_flags_enabled, alert_kind_enabled, monitor_silence_seconds, visual_mode,
    AlertKind, AlertPlan, RequestHandler, VisualMode,
};
use crate::hook_runtime::hooks_disabled;
use crate::pane_terminals::HandlerState;

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) async fn alerts_queue_window(
        &self,
        target: WindowTarget,
        flags: AlertFlags,
    ) {
        let attached_count = self.attached_count(target.session_name()).await;
        let plans = {
            let mut state = self.state.lock().await;
            self.alerts_queue_window_locked(&mut state, target, flags, attached_count)
        };
        self.execute_alert_plans(plans).await;
    }

    pub(in crate::handler) fn alerts_queue_window_locked(
        &self,
        state: &mut HandlerState,
        target: WindowTarget,
        flags: AlertFlags,
        attached_count: usize,
    ) -> Vec<AlertPlan> {
        self.alerts_queue_window_locked_inner(state, target, flags, attached_count, true)
    }

    pub(in crate::handler) fn alerts_queue_window_locked_without_silence_reset(
        &self,
        state: &mut HandlerState,
        target: WindowTarget,
        flags: AlertFlags,
        attached_count: usize,
    ) -> Vec<AlertPlan> {
        self.alerts_queue_window_locked_inner(state, target, flags, attached_count, false)
    }

    /// Clears the family's silence state and rearms each represented winlink.
    ///
    /// Callers that serialize pane-output observation may run this before a
    /// lifecycle hook wait, then use `alerts_queue_window_locked_without_silence_reset`
    /// for the final alert application. This keeps timer ordering tied to the
    /// observed activity without holding a global dispatch lock across hooks.
    pub(in crate::handler) fn reset_window_family_silence_locked(
        &self,
        state: &mut HandlerState,
        target: &WindowTarget,
    ) -> bool {
        let target_exists = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some();
        if !target_exists {
            return false;
        }

        let family_targets =
            state.window_linked_window_targets(target.session_name(), target.window_index());
        let timer_resets = family_targets
            .iter()
            .map(|family_target| {
                (
                    family_target.clone(),
                    monitor_silence_seconds(
                        &state.options,
                        family_target.session_name(),
                        family_target.window_index(),
                    ),
                )
            })
            .collect::<Vec<_>>();
        for family_target in family_targets {
            let Some(session) = state.sessions.session_mut(family_target.session_name()) else {
                continue;
            };
            if let Some(window) = session.window_at_mut(family_target.window_index()) {
                window.clear_alert_flags(WINDOW_SILENCE);
            }
            let _ =
                session.clear_winlink_alert_flags(family_target.window_index(), WINLINK_SILENCE);
        }
        for (family_target, seconds) in timer_resets {
            self.configure_silence_timer_locked(state, family_target, seconds);
        }
        true
    }

    fn alerts_queue_window_locked_inner(
        &self,
        state: &mut HandlerState,
        target: WindowTarget,
        flags: AlertFlags,
        attached_count: usize,
        reset_family_silence: bool,
    ) -> Vec<AlertPlan> {
        let target_exists = state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some();
        if !target_exists {
            return Vec::new();
        }

        // Only reset the silence timer on activity/bell, not when silence itself fires.
        if reset_family_silence && flags.intersects(WINDOW_ACTIVITY.union(WINDOW_BELL)) {
            let _ = self.reset_window_family_silence_locked(state, &target);
        }

        let alerts_enabled = alert_flags_enabled(
            &state.options,
            target.session_name(),
            target.window_index(),
            flags,
        );
        if !alerts_enabled {
            return Vec::new();
        }

        let queued = {
            let session = state
                .sessions
                .session_mut(target.session_name())
                .expect("alert target existence was checked");
            let window = session
                .window_at_mut(target.window_index())
                .expect("alert window existence was checked");
            window.queue_alerts(flags);
            window.set_alerts_queued(true);
            let queued = window.take_alert_flags();
            window.set_alerts_queued(false);
            queued
        };

        let mut plans = Vec::new();
        for kind in [AlertKind::Bell, AlertKind::Activity, AlertKind::Silence] {
            if !queued.contains(kind.window_flag()) {
                continue;
            }
            if let Some(plan) = build_alert_plan_locked(state, &target, kind, attached_count) {
                plans.push(plan);
            }
        }
        plans
    }

    pub(in crate::handler) async fn execute_alert_plans(&self, plans: Vec<AlertPlan>) {
        for plan in plans {
            if let Some(event) = &plan.lifecycle_event {
                self.emit_prepared(event.clone()).await;
            }
            self.pause_after_alert_plan_hook_enqueue().await;
            if plan.send_bell {
                self.send_attached_bell(plan.session_id).await;
            }
            if plan.show_message {
                self.show_alert_message(&plan).await;
            }
            if plan.refresh_session {
                self.refresh_alert_plan_session(plan.session_id).await;
            }
        }
    }
}

fn build_alert_plan_locked(
    state: &mut HandlerState,
    target: &WindowTarget,
    kind: AlertKind,
    attached_count: usize,
) -> Option<AlertPlan> {
    let session_name = target.session_name();
    if !alert_kind_enabled(&state.options, session_name, target.window_index(), kind) {
        return None;
    }
    let action = alert_action(&state.options, session_name, kind.action_option());
    let visual = visual_mode(&state.options, session_name, kind.visual_option());

    let session = state.sessions.session_mut(session_name)?;
    let session_id = session.id();
    let is_current = session.active_window_index() == target.window_index();
    let winlink_flag = kind.winlink_flag();
    let existing_flags = session.winlink_alert_flags(target.window_index());
    if matches!(kind, AlertKind::Activity | AlertKind::Silence)
        && existing_flags.contains(winlink_flag)
    {
        return None;
    }

    let refresh_session = if !is_current || attached_count == 0 {
        session.add_winlink_alert_flags(target.window_index(), winlink_flag)
    } else {
        false
    };
    let action_applies = action.applies(is_current);
    let message_text = if is_current {
        format!("{} in current window", kind.label())
    } else {
        format!("{} in window {}", kind.label(), target.window_index())
    };

    let lifecycle_event = (action_applies && !hooks_disabled()).then(|| {
        let event = kind.lifecycle_event(target.clone());
        prepare_lifecycle_event(state, &event)
    });

    Some(AlertPlan {
        session_id,
        refresh_session,
        send_bell: action_applies && matches!(visual, VisualMode::Off | VisualMode::Both),
        show_message: action_applies && !matches!(visual, VisualMode::Off),
        message_text,
        lifecycle_event,
    })
}
