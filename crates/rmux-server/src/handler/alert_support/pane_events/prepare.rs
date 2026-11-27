//! Preparation of one pane alert under the state lock: stable identity, titles, clipboard and flags.

use rmux_core::{LifecycleEvent, WINDOW_ACTIVITY, WINDOW_BELL};
use rmux_proto::OptionName;

use super::super::super::{prepare_unsequenced_lifecycle_event, RequestHandler};
use super::{PreparedPaneAlertEvent, StablePaneAlertIdentity};
use crate::pane_io::PaneAlertEvent;

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) async fn prepare_pane_alert_event(
        &self,
        event: PaneAlertEvent,
    ) -> Option<PreparedPaneAlertEvent> {
        let mut state = self.state.lock().await;
        self.prepare_pane_alert_event_locked(&mut state, event)
    }

    pub(super) fn prepare_pane_alert_event_locked(
        &self,
        state: &mut crate::pane_terminals::HandlerState,
        event: PaneAlertEvent,
    ) -> Option<PreparedPaneAlertEvent> {
        let runtime_session_name = state.resolve_pane_event_runtime_session(
            &event.session_name,
            event.pane_id,
            event.generation,
        )?;
        let pane_target =
            state.pane_target_for_runtime_pane(&runtime_session_name, event.pane_id)?;
        let identity = StablePaneAlertIdentity::capture(
            state,
            runtime_session_name,
            &pane_target,
            event.pane_id,
            event.generation,
        )?;
        let window_index = pane_target.window_index();
        if event.alternate_mode_changed {
            if let Err(error) = state.resize_terminals(pane_target.session_name()) {
                tracing::warn!(
                    session = %pane_target.session_name(),
                    pane_id = event.pane_id.as_u32(),
                    "failed to reconcile pane geometry after alternate-screen transition: {error}"
                );
            }
        }
        // Alert bookkeeping keeps its own guarantees: a momentarily
        // inconsistent linked family contributes no activity, alert, clipboard
        // or refresh. Renames no longer ride on this event, so nothing of
        // theirs is lost when it is dropped.
        state
            .touch_linked_window_activity_for_pane(&pane_target, event.pane_id)
            .then_some(())?;
        let refresh_for_inactive_pane_output = state
            .sessions
            .session(pane_target.session_name())
            .is_some_and(|session| {
                session.active_window_index() != window_index
                    || session.active_pane_id() != Some(event.pane_id)
            });
        // The active pane's title and OSC 7 path reach the outer terminal only
        // through a re-render: the attach prelude carries the expanded
        // `set-titles-string` and the pane's path, and no other path re-emits
        // them, so a program that retitles itself or changes directory never
        // updated its host (issue #182).
        let outer_identity_changed = event.title_changed || event.path_changed;
        // A pane toggling its mouse-tracking mode must refresh attached
        // clients even when it is the active pane: the refresh rebuilds the
        // outer terminal, whose transition diff emits the outer mouse
        // enable/disable for pane-driven tracking (issue #93).
        let redraws_every_linked_session = refresh_for_inactive_pane_output
            || event.mouse_mode_changed
            || event.alternate_mode_changed;
        let linked_sessions = if redraws_every_linked_session || outer_identity_changed {
            state.window_linked_session_family_list(pane_target.session_name(), window_index)
        } else {
            Vec::new()
        };
        let inactive_refresh_sessions = linked_sessions
            .into_iter()
            // A session with `set-titles off` writes neither OSC 0 nor OSC 7,
            // so an outer-identity change alone must cost it no redraw — not
            // even when it shares the window with a session that has it on.
            .filter(|session_name| {
                redraws_every_linked_session
                    || state
                        .options
                        .resolve(Some(session_name), OptionName::SetTitles)
                        == Some("on")
            })
            .filter_map(|session_name| {
                state
                    .sessions
                    .session(&session_name)
                    .map(|session| session.id())
            })
            .collect();
        let set_clipboard_on = matches!(
            state.options.resolve(None, OptionName::SetClipboard),
            Some("on")
        );
        let mut lifecycle_events = Vec::new();
        if event.title_changed {
            lifecycle_events.push(prepare_unsequenced_lifecycle_event(
                state,
                &LifecycleEvent::PaneTitleChanged {
                    target: pane_target.clone(),
                },
            ));
        }
        if event.clipboard_set && set_clipboard_on {
            lifecycle_events.push(prepare_unsequenced_lifecycle_event(
                state,
                &LifecycleEvent::PaneSetClipboard {
                    target: pane_target,
                },
            ));
        }
        let clipboard_writes = if set_clipboard_on {
            event.clipboard_writes
        } else {
            Vec::new()
        };
        let alert_flags = (event.queue_activity_alert || event.bell_count > 0).then_some(
            if event.bell_count > 0 {
                WINDOW_ACTIVITY.union(WINDOW_BELL)
            } else {
                WINDOW_ACTIVITY
            },
        );

        Some(PreparedPaneAlertEvent {
            identity,
            inactive_refresh_sessions,
            clipboard_writes,
            lifecycle_events,
            alert_flags,
        })
    }
}
