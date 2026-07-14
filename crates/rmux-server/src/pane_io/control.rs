use std::collections::VecDeque;
use std::future::pending;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};

use rmux_core::{events::OutputCursorItem, TerminalPassthrough};
use rmux_proto::AttachMessage;
use tokio::sync::mpsc;

use super::attach_transport::AttachTransport;
use super::exit_log::AttachExitReason;
use super::passthrough::render_passthroughs;
use super::pending_escape::PendingEscapeFlush;
use super::persistent_overlay::{
    accept_persistent_overlay_state, advance_persistent_overlay_state, clear_then_base_frame,
    defer_persistent_clear, discard_stale_persistent_overlays, is_stale_persistent_switch,
    persistent_overlay_replacement_pending, replacement_persistent_overlay_frame,
    switch_requires_screen_clear, take_pending_persistent_overlay_for_state,
    update_persistent_overlay_cache,
};
use super::types::{AttachControl, AttachTarget, OpenAttachTarget, OverlayFrame};
use super::wire::{
    emit_attach_bytes, emit_attach_message, emit_attach_stop, emit_detached_attach_stop,
    emit_render_frame, open_attach_target,
};

pub(super) struct PendingAttachInputState<'a> {
    bytes: &'a mut Vec<u8>,
    escape_flush: &'a mut PendingEscapeFlush,
}

impl<'a> PendingAttachInputState<'a> {
    pub(super) fn new(bytes: &'a mut Vec<u8>, escape_flush: &'a mut PendingEscapeFlush) -> Self {
        Self {
            bytes,
            escape_flush,
        }
    }

    pub(super) fn clear_if_pane_source_changed(
        &mut self,
        current_target: &OpenAttachTarget,
        next_target: &AttachTarget,
    ) {
        let same_pane_source = current_target
            .pane_output
            .as_ref()
            .is_some_and(|current_output| {
                current_output.shares_pane_source_with(&next_target.pane_output)
            });
        if same_pane_source {
            return;
        }

        self.clear();
    }

    pub(super) fn parts_mut(&mut self) -> (&mut Vec<u8>, &mut PendingEscapeFlush) {
        (self.bytes, self.escape_flush)
    }

    pub(super) fn clear(&mut self) {
        self.bytes.clear();
        self.escape_flush.clear();
    }
}

pub(super) fn should_emit_overlay(
    render_generation: u64,
    current_overlay_generation: &mut u64,
    overlay: &OverlayFrame,
) -> bool {
    if overlay.render_generation < render_generation {
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
    control_backlog: &AtomicUsize,
) -> Option<AttachControl> {
    if let Some(control) = deferred_controls.pop_front() {
        return Some(control);
    }
    match control_rx {
        Some(control_rx) => {
            let control = control_rx.recv().await;
            if control.is_some() {
                decrement_control_backlog(control_backlog);
            }
            control
        }
        None => pending().await,
    }
}

pub(super) fn decrement_control_backlog(control_backlog: &AtomicUsize) {
    let _ = control_backlog.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
        value.checked_sub(1)
    });
}

pub(super) fn try_recv_attach_control(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    control_backlog: &AtomicUsize,
) -> Result<AttachControl, mpsc::error::TryRecvError> {
    let control = control_rx.try_recv()?;
    decrement_control_backlog(control_backlog);
    Ok(control)
}

pub(super) fn coalesce_render_switches(
    mut target: Box<AttachTarget>,
    deferred_controls: &mut VecDeque<AttachControl>,
    mut control_rx: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    control_backlog: &AtomicUsize,
) -> (Box<AttachTarget>, u64) {
    let mut switch_count = 1_u64;
    if !target.is_coalescible_render_refresh() {
        return (target, switch_count);
    }

    while deferred_controls
        .front()
        .is_some_and(AttachControl::is_coalescible_render_switch)
    {
        let Some(AttachControl::Switch(next_target)) = deferred_controls.pop_front() else {
            unreachable!("front was checked as a coalescible switch");
        };
        target = next_target;
        switch_count = switch_count.saturating_add(1);
    }

    let Some(control_rx) = control_rx.as_mut() else {
        return (target, switch_count);
    };
    loop {
        match try_recv_attach_control(control_rx, control_backlog) {
            Ok(AttachControl::Switch(next_target))
                if next_target.is_coalescible_render_refresh() =>
            {
                target = next_target;
                switch_count = switch_count.saturating_add(1);
            }
            Ok(control) => {
                deferred_controls.push_back(control);
                break;
            }
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                break;
            }
        }
    }

    (target, switch_count)
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
    let render_stream = current_target.render_stream;
    *current_target = open_attach_target(next_target, render_stream)?;
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
    Exit(PendingAttachExit),
    Continue { target_changed: bool },
    InteractiveInput,
    Refresh { target_changed: bool },
    Write,
}

pub(super) struct PendingAttachExit {
    pub(super) reason: AttachExitReason,
    pub(super) drop_pending_output: bool,
    pub(super) snapshot_covered_output_before_sequence: Option<u64>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn apply_pending_attach_controls(
    deferred_controls: &mut VecDeque<AttachControl>,
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    control_backlog: &AtomicUsize,
    current_target: &mut OpenAttachTarget,
    stream: &AttachTransport,
    render_generation: &mut u64,
    overlay_generation: &mut u64,
    persistent_overlay: &mut Option<Vec<u8>>,
    persistent_overlay_visible: &mut bool,
    persistent_overlay_state_id: &mut Option<u64>,
    locked: &mut bool,
    mut pending_input: Option<PendingAttachInputState<'_>>,
) -> io::Result<PendingAttachAction> {
    let Some(control_rx) = attach_controls else {
        return Ok(PendingAttachAction::Write);
    };

    let mut should_drop_output = false;
    let mut snapshot_covered_output_before_sequence = None;
    let mut target_changed = false;
    loop {
        let control = deferred_controls
            .pop_front()
            .map(Ok)
            .unwrap_or_else(|| try_recv_attach_control(control_rx, control_backlog));
        match control {
            Ok(AttachControl::Detach) => {
                emit_detached_attach_stop(stream, current_target).await?;
                return Ok(PendingAttachAction::Exit(PendingAttachExit {
                    reason: AttachExitReason::AttachControlDetach,
                    drop_pending_output: should_drop_output,
                    snapshot_covered_output_before_sequence,
                }));
            }
            Ok(AttachControl::Exited) => {
                return Ok(PendingAttachAction::Exit(PendingAttachExit {
                    reason: AttachExitReason::AttachControlExited,
                    drop_pending_output: should_drop_output,
                    snapshot_covered_output_before_sequence,
                }));
            }
            Ok(AttachControl::DetachKill) => {
                emit_attach_stop(stream, current_target).await?;
                emit_attach_message(stream, &AttachMessage::DetachKill).await?;
                return Ok(PendingAttachAction::Exit(PendingAttachExit {
                    reason: AttachExitReason::AttachControlDetachKill,
                    drop_pending_output: should_drop_output,
                    snapshot_covered_output_before_sequence,
                }));
            }
            Ok(AttachControl::DetachExecShellCommand(command)) => {
                emit_attach_stop(stream, current_target).await?;
                emit_attach_message(stream, &AttachMessage::DetachExecShellCommand(command))
                    .await?;
                return Ok(PendingAttachAction::Exit(PendingAttachExit {
                    reason: AttachExitReason::AttachControlDetachExec,
                    drop_pending_output: should_drop_output,
                    snapshot_covered_output_before_sequence,
                }));
            }
            Ok(AttachControl::InteractiveInput) => {
                return Ok(PendingAttachAction::InteractiveInput);
            }
            Ok(AttachControl::Refresh) => {
                return Ok(PendingAttachAction::Refresh { target_changed });
            }
            Ok(AttachControl::Switch(next_target)) => {
                let (next_target, switch_count) = coalesce_render_switches(
                    next_target,
                    deferred_controls,
                    Some(control_rx),
                    control_backlog,
                );
                let same_pane_source =
                    current_target
                        .pane_output
                        .as_ref()
                        .is_some_and(|current_output| {
                            current_output.shares_pane_source_with(&next_target.pane_output)
                        });
                let preserve_live_output =
                    next_target.is_coalescible_render_refresh() && same_pane_source;
                let drop_live_output = !preserve_live_output;
                if is_stale_persistent_switch(*persistent_overlay_state_id, next_target.as_ref()) {
                    *render_generation = (*render_generation).saturating_add(switch_count);
                    continue;
                }
                if drop_live_output {
                    snapshot_covered_output_before_sequence = None;
                } else if !should_drop_output {
                    // Keep the earliest snapshot boundary for the batch that was
                    // dequeued before control processing. A later same-source
                    // receiver may already forward the interval between two
                    // refreshes, so advancing this boundary would replay it on
                    // exit.
                    let next_boundary = next_target.pane_output_start_sequence;
                    snapshot_covered_output_before_sequence = Some(
                        snapshot_covered_output_before_sequence
                            .map_or(next_boundary, |boundary| boundary.min(next_boundary)),
                    );
                }
                let pending_passthroughs = if drop_live_output {
                    Vec::new()
                } else {
                    take_pending_live_passthroughs(
                        current_target,
                        next_target.pane_output_start_sequence,
                    )
                };
                if let Some(pending_input) = pending_input.as_mut() {
                    pending_input
                        .clear_if_pane_source_changed(current_target, next_target.as_ref());
                }
                *render_generation = (*render_generation).saturating_add(switch_count);
                let pending_overlay = take_pending_persistent_overlay_for_state(
                    Some(control_rx),
                    deferred_controls,
                    next_target.persistent_overlay_state_id,
                    *render_generation,
                    *overlay_generation,
                    control_backlog,
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
                if !pending_passthroughs.is_empty() {
                    let passthrough_frame =
                        render_passthroughs(current_target, &pending_passthroughs);
                    emit_attach_bytes(stream, &passthrough_frame).await?;
                }
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
                        control_backlog,
                    );
                }
                should_drop_output |= drop_live_output;
            }
            Ok(AttachControl::AdvancePersistentOverlayState(state_id)) => {
                let previous_overlay_state_id = *persistent_overlay_state_id;
                advance_persistent_overlay_state(
                    persistent_overlay_state_id,
                    Some(control_rx),
                    deferred_controls,
                    state_id,
                    control_backlog,
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
                if let Some(pending_input) = pending_input.as_mut() {
                    pending_input.clear();
                }
                *locked = true;
                emit_attach_stop(stream, current_target).await?;
                emit_attach_message(stream, &AttachMessage::LockShellCommand(command)).await?;
                should_drop_output = true;
                snapshot_covered_output_before_sequence = None;
            }
            Ok(AttachControl::Suspend) => {
                if let Some(pending_input) = pending_input.as_mut() {
                    pending_input.clear();
                }
                *locked = true;
                emit_attach_stop(stream, current_target).await?;
                emit_attach_message(stream, &AttachMessage::Suspend).await?;
                should_drop_output = true;
                snapshot_covered_output_before_sequence = None;
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

pub(super) fn take_pending_live_passthroughs(
    current_target: &mut OpenAttachTarget,
    before_sequence: u64,
) -> Vec<TerminalPassthrough> {
    let Some(pane_output) = current_target.pane_output.as_mut() else {
        return Vec::new();
    };
    let mut passthroughs = Vec::new();
    while let Some(item) = pane_output.try_recv() {
        let OutputCursorItem::Event(event) = item else {
            break;
        };
        if event.sequence() >= before_sequence {
            break;
        }
        passthroughs.extend(event.into_passthroughs());
    }
    passthroughs
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
