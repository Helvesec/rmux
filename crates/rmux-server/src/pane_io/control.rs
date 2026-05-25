use std::collections::VecDeque;
use std::future::pending;
use std::io;

use rmux_proto::AttachMessage;
use tokio::sync::mpsc;

use super::attach_transport::AttachTransport;
use super::exit_log::AttachExitReason;
use super::persistent_overlay::{
    accept_persistent_overlay_state, advance_persistent_overlay_state, clear_then_base_frame,
    defer_persistent_clear, discard_stale_persistent_overlays, is_stale_persistent_switch,
    persistent_overlay_replacement_pending, replacement_persistent_overlay_frame,
    switch_requires_screen_clear, take_pending_persistent_overlay_for_state,
    update_persistent_overlay_cache,
};
use super::types::{AttachControl, AttachTarget, OpenAttachTarget, OverlayFrame};
use super::wire::{
    emit_attach_bytes, emit_attach_message, emit_attach_stop, emit_detached_message,
    emit_exited_message, emit_render_frame, open_attach_target,
};

pub(super) fn should_emit_overlay(
    render_generation: u64,
    current_overlay_generation: &mut u64,
    overlay: &OverlayFrame,
) -> bool {
    if overlay.render_generation != render_generation {
        return false;
    }
    if overlay.overlay_generation < *current_overlay_generation {
        return false;
    }

    *current_overlay_generation = overlay.overlay_generation;
    true
}

pub(super) async fn recv_attach_control(
    deferred_controls: &mut VecDeque<AttachControl>,
    control_rx: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
) -> Option<AttachControl> {
    if let Some(control) = deferred_controls.pop_front() {
        return Some(control);
    }
    match control_rx {
        Some(control_rx) => control_rx.recv().await,
        None => pending().await,
    }
}

pub(super) async fn switch_attach_target(
    stream: &AttachTransport,
    current_target: &mut OpenAttachTarget,
    next_target: AttachTarget,
    clear_from_persistent_overlay: bool,
    replacement_frame: Option<&[u8]>,
) -> io::Result<()> {
    let previous_terminal = current_target.outer_terminal.clone();
    let previous_cursor_style = current_target.cursor_style;
    *current_target = open_attach_target(next_target)?;
    emit_attach_bytes(
        stream,
        &current_target
            .outer_terminal
            .transition_sequence_from(&previous_terminal),
    )
    .await?;
    if let Some(sequence) = current_target
        .outer_terminal
        .render_cursor_style_transition(Some(previous_cursor_style), current_target.cursor_style)
    {
        emit_attach_bytes(stream, sequence.as_bytes()).await?;
    }
    if let Some(overlay_frame) = replacement_frame {
        let mut frame = Vec::with_capacity(current_target.render_frame.len() + overlay_frame.len());
        frame.extend_from_slice(&current_target.render_frame);
        frame.extend_from_slice(overlay_frame);
        emit_render_frame(stream, &current_target.outer_terminal, &frame).await
    } else if clear_from_persistent_overlay {
        let frame = clear_then_base_frame(current_target);
        emit_render_frame(stream, &current_target.outer_terminal, &frame).await
    } else {
        emit_render_frame(
            stream,
            &current_target.outer_terminal,
            &current_target.render_frame,
        )
        .await
    }
}

pub(super) enum PendingAttachAction {
    Exit(AttachExitReason),
    Continue { target_changed: bool },
    Write,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_pending_attach_controls(
    deferred_controls: &mut VecDeque<AttachControl>,
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    current_target: &mut OpenAttachTarget,
    stream: &AttachTransport,
    render_generation: &mut u64,
    overlay_generation: &mut u64,
    persistent_overlay: &mut Option<Vec<u8>>,
    persistent_overlay_visible: &mut bool,
    persistent_overlay_state_id: &mut Option<u64>,
    locked: &mut bool,
) -> io::Result<PendingAttachAction> {
    let Some(control_rx) = attach_controls else {
        return Ok(PendingAttachAction::Write);
    };

    let mut should_drop_output = false;
    let mut target_changed = false;
    loop {
        let control = deferred_controls
            .pop_front()
            .map(Ok)
            .unwrap_or_else(|| control_rx.try_recv());
        match control {
            Ok(AttachControl::Detach) => {
                emit_attach_stop(stream, current_target).await?;
                emit_detached_message(stream, current_target).await?;
                return Ok(PendingAttachAction::Exit(
                    AttachExitReason::AttachControlDetach,
                ));
            }
            Ok(AttachControl::Exited) => {
                emit_attach_stop(stream, current_target).await?;
                emit_exited_message(stream).await?;
                return Ok(PendingAttachAction::Exit(
                    AttachExitReason::AttachControlExited,
                ));
            }
            Ok(AttachControl::DetachKill) => {
                emit_attach_stop(stream, current_target).await?;
                emit_attach_message(stream, &AttachMessage::DetachKill).await?;
                return Ok(PendingAttachAction::Exit(
                    AttachExitReason::AttachControlDetachKill,
                ));
            }
            Ok(AttachControl::DetachExecShellCommand(command)) => {
                emit_attach_stop(stream, current_target).await?;
                emit_attach_message(stream, &AttachMessage::DetachExecShellCommand(command))
                    .await?;
                return Ok(PendingAttachAction::Exit(
                    AttachExitReason::AttachControlDetachExec,
                ));
            }
            Ok(AttachControl::Switch(next_target)) => {
                if is_stale_persistent_switch(*persistent_overlay_state_id, next_target.as_ref()) {
                    continue;
                }
                *render_generation = render_generation.saturating_add(1);
                let pending_overlay = take_pending_persistent_overlay_for_state(
                    Some(control_rx),
                    deferred_controls,
                    next_target.persistent_overlay_state_id,
                    *render_generation,
                    *overlay_generation,
                );
                let replacement_frame = pending_overlay
                    .as_ref()
                    .map(|overlay| overlay.frame.clone())
                    .or_else(|| {
                        replacement_persistent_overlay_frame(
                            persistent_overlay,
                            *persistent_overlay_visible,
                            next_target.as_ref(),
                        )
                    });
                let clear_screen = switch_requires_screen_clear(
                    *persistent_overlay_visible,
                    persistent_overlay.is_some(),
                    *persistent_overlay_state_id,
                    current_target.persistent_overlay_state_id,
                    next_target.persistent_overlay_state_id,
                );
                if replacement_frame.is_none() {
                    persistent_overlay.take();
                    *persistent_overlay_visible = false;
                }
                if let Some(overlay) = pending_overlay.as_ref() {
                    *overlay_generation = overlay.overlay_generation;
                }
                switch_attach_target(
                    stream,
                    current_target,
                    *next_target,
                    clear_screen,
                    replacement_frame.as_deref(),
                )
                .await?;
                target_changed = true;
                if let Some(overlay) = pending_overlay {
                    update_persistent_overlay_cache(
                        persistent_overlay,
                        persistent_overlay_visible,
                        &overlay,
                    );
                }
                *persistent_overlay_state_id = current_target.persistent_overlay_state_id;
                if let Some(barrier_state_id) = *persistent_overlay_state_id {
                    discard_stale_persistent_overlays(
                        Some(control_rx),
                        deferred_controls,
                        barrier_state_id,
                    );
                }
                should_drop_output = true;
            }
            Ok(AttachControl::AdvancePersistentOverlayState(state_id)) => {
                let previous_overlay_state_id = *persistent_overlay_state_id;
                advance_persistent_overlay_state(
                    persistent_overlay_state_id,
                    Some(control_rx),
                    deferred_controls,
                    state_id,
                );
                redraw_after_persistent_overlay_state_advance(
                    stream,
                    current_target,
                    persistent_overlay,
                    persistent_overlay_visible,
                    previous_overlay_state_id,
                    *persistent_overlay_state_id,
                    persistent_overlay_replacement_pending(
                        deferred_controls,
                        *persistent_overlay_state_id,
                    ),
                )
                .await?;
            }
            Ok(AttachControl::Overlay(overlay)) => {
                if !accept_persistent_overlay_state(persistent_overlay_state_id, &overlay) {
                    continue;
                }
                let persistent_clear = overlay.persistent && overlay.frame.is_empty();
                if persistent_clear
                    || should_emit_overlay(*render_generation, overlay_generation, &overlay)
                {
                    update_persistent_overlay_cache(
                        persistent_overlay,
                        persistent_overlay_visible,
                        &overlay,
                    );
                    if defer_persistent_clear(
                        persistent_clear,
                        deferred_controls,
                        *persistent_overlay_state_id,
                    ) {
                        continue;
                    }
                    let clear_frame =
                        persistent_clear.then(|| clear_then_base_frame(current_target));
                    emit_render_frame(
                        stream,
                        &current_target.outer_terminal,
                        clear_frame.as_deref().unwrap_or(&overlay.frame),
                    )
                    .await?;
                }
            }
            Ok(AttachControl::Write(bytes)) => {
                emit_attach_bytes(stream, &bytes).await?;
            }
            Ok(AttachControl::LockShellCommand(command)) => {
                *locked = true;
                emit_attach_message(stream, &AttachMessage::LockShellCommand(command)).await?;
                should_drop_output = true;
            }
            Ok(AttachControl::Suspend) => {
                *locked = true;
                emit_attach_message(stream, &AttachMessage::Suspend).await?;
                should_drop_output = true;
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }

    if should_drop_output {
        Ok(PendingAttachAction::Continue { target_changed })
    } else {
        Ok(PendingAttachAction::Write)
    }
}

pub(super) async fn redraw_after_persistent_overlay_state_advance(
    _stream: &AttachTransport,
    _current_target: &OpenAttachTarget,
    _persistent_overlay: &mut Option<Vec<u8>>,
    persistent_overlay_visible: &mut bool,
    previous_state_id: Option<u64>,
    current_state_id: Option<u64>,
    replacement_pending: bool,
) -> io::Result<()> {
    if !*persistent_overlay_visible || previous_state_id == current_state_id {
        return Ok(());
    }

    if replacement_pending {
        // State advance is only an ordering barrier when a replacement repaint
        // is queued. Keep the current overlay on screen to avoid flashing a
        // stale base pane between choose-tree frames.
        return Ok(());
    }

    // A state advance is a barrier, not a fresh base snapshot. Dismiss paths
    // queue a switch repaint after the mode tree state is removed; clearing here
    // can repaint an older attach target while that fresh switch is still being
    // produced.
    Ok(())
}
