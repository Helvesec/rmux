//! Deadline and expired-state inheritance for newly inserted aliases.

use rmux_core::WINLINK_SILENCE;
use rmux_proto::{WindowId, WindowTarget};

use super::{desired_silence_timer, monitor_silence_seconds};
use crate::handler::RequestHandler;
use crate::pane_terminals::HandlerState;

#[derive(Clone, Copy)]
pub(in crate::handler) struct SilenceTimerDeadlineFanout {
    pub(super) window_id: WindowId,
    pub(super) deadline: Option<tokio::time::Instant>,
    source_had_silence_flag: bool,
}

impl SilenceTimerDeadlineFanout {
    pub(in crate::handler) fn apply_expired_state_locked(
        self,
        state: &mut HandlerState,
        destinations: &[WindowTarget],
    ) {
        if self.deadline.is_some() {
            return;
        }
        for destination in destinations {
            let Some(session) = state.sessions.session_mut(destination.session_name()) else {
                continue;
            };
            if session
                .window_at(destination.window_index())
                .is_none_or(|window| window.id() != self.window_id)
            {
                continue;
            }
            if self.source_had_silence_flag {
                let _ =
                    session.add_winlink_alert_flags(destination.window_index(), WINLINK_SILENCE);
            } else {
                let _ =
                    session.clear_winlink_alert_flags(destination.window_index(), WINLINK_SILENCE);
            }
        }
    }
}

impl RequestHandler {
    pub(in crate::handler) fn plan_silence_timer_deadline_fanout_locked(
        &self,
        state: &HandlerState,
        source: &WindowTarget,
    ) -> Option<SilenceTimerDeadlineFanout> {
        let desired = desired_silence_timer(
            state,
            source.clone(),
            monitor_silence_seconds(&state.options, source.session_name(), source.window_index()),
        )?;
        if desired.seconds == 0 {
            return None;
        }
        let deadline = self
            .silence_timers
            .lock()
            .expect("silence timer mutex must not be poisoned")
            .get(source)
            .filter(|timer| {
                timer.session_id == desired.session_id && timer.window_id == desired.window_id
            })
            .map(|timer| timer.deadline);
        let source_had_silence_flag =
            state
                .sessions
                .session(source.session_name())
                .is_some_and(|session| {
                    session
                        .winlink_alert_flags(source.window_index())
                        .contains(WINLINK_SILENCE)
                });
        Some(SilenceTimerDeadlineFanout {
            window_id: desired.window_id,
            deadline,
            source_had_silence_flag,
        })
    }
}
