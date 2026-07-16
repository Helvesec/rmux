//! Explicit destination-group fanout for structural timer mutations.

use rmux_proto::{WindowId, WindowTarget};

use super::super::{desired_silence_timer, monitor_silence_seconds};
use super::{target_has_window_id, SilenceTimerWindowMutation};
use crate::pane_terminals::HandlerState;

pub(super) struct ResolvedSilenceTimerTarget {
    pub(super) target: WindowTarget,
    pub(super) session_id: rmux_proto::SessionId,
}

impl SilenceTimerWindowMutation {
    pub(in crate::handler) fn fanout_target_to_destination_group_locked(
        &mut self,
        state: &HandlerState,
        source: WindowTarget,
        destination: &WindowTarget,
    ) {
        let Some(window_id) = state
            .sessions
            .session(destination.session_name())
            .and_then(|session| session.window_at(destination.window_index()))
            .map(rmux_core::Window::id)
        else {
            return;
        };
        let targets = self.fanout_targets.entry(source).or_default();
        for session_name in state
            .sessions
            .session_group_members(destination.session_name())
        {
            let target = WindowTarget::with_window(session_name, destination.window_index());
            if target_has_window_id(state, &target, window_id) && !targets.contains(&target) {
                targets.push(target);
            }
        }
    }
}

pub(super) fn resolve_silence_timer_target(
    state: &HandlerState,
    target: WindowTarget,
    window_id: WindowId,
) -> Option<ResolvedSilenceTimerTarget> {
    desired_silence_timer(
        state,
        target.clone(),
        monitor_silence_seconds(&state.options, target.session_name(), target.window_index()),
    )
    .filter(|desired| desired.window_id == window_id && desired.seconds > 0)
    .map(|desired| ResolvedSilenceTimerTarget {
        target,
        session_id: desired.session_id,
    })
}
