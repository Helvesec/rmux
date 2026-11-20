use rmux_core::LifecycleEvent;
use rmux_proto::{OptionName, PaneStateClosedReason, PaneTarget, RmuxError, WindowTarget};

use super::super::super::{prepare_lifecycle_event, QueuedLifecycleEvent, RequestHandler};
use super::dead_pane_footer::{append_remain_on_exit_message, hide_dead_pane_cursor};
use super::wait_for_pane_output_eof;
use crate::pane_io::PaneExitEvent;
use crate::pane_state_journal::PaneStateChange;
use crate::pane_terminals::{session_not_found, HandlerState, PaneExitMetadata};
use tracing::warn;

impl RequestHandler {
    pub(super) async fn commit_kept_dead_pane(
        &self,
        event: &PaneExitEvent,
        output_generation: u64,
        output_eof_observed: bool,
    ) -> Option<(PaneTarget, QueuedLifecycleEvent)> {
        let output_rx = if !output_eof_observed {
            let state = self.state.lock().await;
            let (runtime_session_name, _) =
                current_kept_dead_pane(&state, event, output_generation)?;
            if state.kept_dead_pane_prepared_for_generation(
                &runtime_session_name,
                event.pane_id,
                output_generation,
            ) {
                return None;
            }
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
        let prepare_dead = !state.kept_dead_pane_prepared_for_generation(
            &runtime_session_name,
            event.pane_id,
            output_generation,
        );
        if !prepare_dead {
            return None;
        }
        // The state lock spans publication, format expansion and footer drawing.
        state.publish_runtime_pane_exit(&runtime_session_name, event.pane_id);
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
        // The source expands the format first and hides the cursor after
        // it (`tmux-3.7b/server-fn.c:364` then `370`), so a template that
        // reads `#{cursor_flag}` sees the cursor the command left and the
        // pane still ends with it hidden. The hiding stays the dead pane's
        // own preparation - it runs whether the template was empty, the
        // expansion was empty, or the drawing failed - and the two are
        // reported independently of each other.
        if let Err(error) = hide_dead_pane_cursor(&mut state, &runtime_session_name, &target) {
            warn!(
                session = %runtime_session_name,
                pane_id = event.pane_id.as_u32(),
                "failed to hide the dead pane's cursor: {error}"
            );
        }

        state.mark_kept_dead_pane_prepared(&runtime_session_name, event.pane_id, output_generation);
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

pub(super) fn current_kept_dead_pane(
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
        .observed_runtime_pane_exit_metadata(&runtime_session_name, event.pane_id)
        .map(|_| (runtime_session_name, target))
}

pub(super) fn should_keep_dead_pane(
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

pub(super) fn apply_dead_pane_automatic_window_name(
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

    let _ = state.sessions.set_automatic_window_name(
        &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        rendered,
    )?;
    state.mark_auto_named_window(target.session_name(), target.window_index());
    state.synchronize_linked_window_from_slot(target.session_name(), target.window_index())?;
    Ok(state
        .synchronize_session_group_from(target.session_name())
        .unwrap_or_else(|_| vec![target.session_name().clone()]))
}
