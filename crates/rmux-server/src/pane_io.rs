#[cfg(any(unix, windows))]
use rmux_core::events::OutputCursorItem;
#[cfg(any(unix, windows))]
use rmux_ipc::LocalStream;
#[cfg(any(unix, windows))]
use rmux_proto::{AttachFrameDecoder, AttachMessage};
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicBool, AtomicU64};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::{collections::VecDeque, io, sync::atomic::Ordering};
#[cfg(any(unix, windows))]
use tokio::sync::mpsc;
#[cfg(any(unix, windows))]
use tokio::sync::watch;
#[cfg(any(unix, windows))]
use tracing::debug;

const READ_BUFFER_SIZE: usize = 8192;
mod control;
mod deferred_passthrough;
mod live_render;
mod passthrough;
mod persistent_overlay;
mod reader;
mod refresh_scheduler;
mod types;
mod wire;

#[cfg(any(unix, windows))]
use crate::renderer::PaneRenderDelta;
#[cfg(any(unix, windows))]
use control::{
    apply_pending_attach_controls, recv_attach_control,
    redraw_after_persistent_overlay_state_advance, should_emit_overlay, switch_attach_target,
    PendingAttachAction,
};
#[cfg(any(unix, windows))]
use deferred_passthrough::{
    clear_deferred_passthroughs_if_target_changed, defer_passthroughs, flush_deferred_passthroughs,
    take_passthrough_frame,
};
pub(crate) use live_render::LivePaneRender;
#[cfg(any(unix, windows))]
use persistent_overlay::{
    accept_persistent_overlay_state, advance_persistent_overlay_state, clear_then_base_frame,
    defer_persistent_clear, discard_stale_persistent_overlays, is_stale_persistent_switch,
    persistent_overlay_replacement_pending, prime_persistent_overlay_barriers,
    replacement_persistent_overlay_frame, switch_requires_screen_clear,
    take_pending_persistent_overlay_for_state, update_persistent_overlay_cache,
};
#[cfg(windows)]
pub(crate) use reader::spawn_pane_exit_watcher;
pub(crate) use reader::spawn_pane_output_reader;
#[cfg(any(unix, windows))]
use refresh_scheduler::{
    wait_for_refresh_deadline, AttachRefreshScheduler, AttachStatusRefreshScheduler,
};
#[cfg(test)]
pub(crate) use types::pane_output_channel_with_limits;
#[cfg(any(unix, windows))]
pub(crate) use types::LiveAttachInputContext;
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use types::{
    pane_output_channel, AttachControl, AttachTarget, HandleOutcome, OverlayFrame,
    PaneAlertCallback, PaneAlertEvent, PaneExitCallback, PaneExitEvent, PaneOutputReceiver,
    PaneOutputSender,
};
#[cfg(any(unix, windows))]
use wire::{
    emit_attach_bytes, emit_attach_frame, emit_attach_message, emit_attach_stop,
    emit_detached_message, emit_exited_message, emit_render_frame, invalid_attach_message,
    open_attach_target, read_socket_bytes, recv_pane_output_optional, try_read_socket_bytes,
    TrySocketRead,
};

#[allow(clippy::too_many_arguments)]
#[cfg(any(unix, windows))]
pub(crate) async fn forward_attach(
    stream: LocalStream,
    target: AttachTarget,
    initial_socket_bytes: Vec<u8>,
    mut shutdown: watch::Receiver<()>,
    control_rx: mpsc::UnboundedReceiver<AttachControl>,
    closing: Arc<AtomicBool>,
    persistent_overlay_epoch: Arc<AtomicU64>,
    live_input: LiveAttachInputContext,
) -> io::Result<()> {
    let stream = stream;
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut attach_controls = Some(control_rx);
    let mut deferred_controls = VecDeque::new();
    let mut socket_read_buffer = [0_u8; READ_BUFFER_SIZE];
    let mut current_target = open_attach_target(target)?;
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut pane_refresh = AttachRefreshScheduler::default();
    let mut deferred_passthroughs = Vec::new();
    let mut status_refresh = AttachStatusRefreshScheduler::new(
        live_input
            .handler
            .attached_status_interval(&current_target.session_name)
            .await,
    );
    let mut locked = false;
    decoder.push_bytes(&initial_socket_bytes);
    emit_attach_bytes(
        &stream,
        &current_target.outer_terminal.attach_start_sequence(),
    )
    .await?;
    if let Some(sequence) = current_target
        .outer_terminal
        .render_cursor_style_transition(None, current_target.cursor_style)
    {
        emit_attach_bytes(&stream, sequence.as_bytes()).await?;
    }
    emit_render_frame(
        &stream,
        &current_target.outer_terminal,
        &current_target.render_frame,
    )
    .await?;

    let result = async {
        loop {
            let overlay_barrier = persistent_overlay_epoch.load(Ordering::SeqCst);
            let previous_overlay_state_id = persistent_overlay_state_id;
            advance_persistent_overlay_state(
                &mut persistent_overlay_state_id,
                attach_controls.as_mut(),
                &mut deferred_controls,
                overlay_barrier,
            );
            redraw_after_persistent_overlay_state_advance(
                &stream,
                &current_target,
                &mut persistent_overlay,
                &mut persistent_overlay_visible,
                previous_overlay_state_id,
                persistent_overlay_state_id,
                persistent_overlay_replacement_pending(
                    &deferred_controls,
                    persistent_overlay_state_id,
                ),
            )
            .await?;
            if closing.load(Ordering::SeqCst) {
                let _ = emit_attach_stop(&stream, &current_target).await;
                return Ok(());
            }
            match apply_pending_attach_controls(
                &mut deferred_controls,
                attach_controls.as_mut(),
                &mut current_target,
                &stream,
                &mut render_generation,
                &mut overlay_generation,
                &mut persistent_overlay,
                &mut persistent_overlay_visible,
                &mut persistent_overlay_state_id,
                &mut locked,
            )
            .await?
            {
                PendingAttachAction::Exit => {
                    return Ok(());
                }
                PendingAttachAction::Continue { target_changed } => {
                    reschedule_status_refresh_if_target_changed(
                        target_changed,
                        &mut status_refresh,
                        &live_input,
                        &current_target,
                    )
                    .await;
                    clear_deferred_passthroughs_if_target_changed(
                        target_changed,
                        &mut deferred_passthroughs,
                    );
                    flush_deferred_passthroughs(
                        &stream,
                        &current_target,
                        &mut deferred_passthroughs,
                        persistent_overlay_visible,
                        persistent_overlay.is_some(),
                    )
                    .await?;
                    continue;
                }
                PendingAttachAction::Write => {}
            }
            // A pending pane refresh is an ordering barrier: emit the frame
            // for the transcript that triggered it before forwarding newer
            // keystrokes to the child process.
            if !pane_refresh.is_pending() {
                loop {
                    match try_read_socket_bytes(&stream, &mut decoder, &mut socket_read_buffer)? {
                        TrySocketRead::Read => {}
                        TrySocketRead::Closed => {
                            let _ = emit_attach_stop(&stream, &current_target).await;
                            return Ok(());
                        }
                        TrySocketRead::WouldBlock => break,
                    }
                }
                process_socket_messages(
                    &mut decoder,
                    &stream,
                    &live_input,
                    &mut pending_input,
                    &mut locked,
                )
                .await?;
            }
            prime_persistent_overlay_barriers(
                &mut persistent_overlay_state_id,
                attach_controls.as_mut(),
                &mut deferred_controls,
            );
            match apply_pending_attach_controls(
                &mut deferred_controls,
                attach_controls.as_mut(),
                &mut current_target,
                &stream,
                &mut render_generation,
                &mut overlay_generation,
                &mut persistent_overlay,
                &mut persistent_overlay_visible,
                &mut persistent_overlay_state_id,
                &mut locked,
            )
            .await?
            {
                PendingAttachAction::Exit => {
                    return Ok(());
                }
                PendingAttachAction::Continue { target_changed } => {
                    reschedule_status_refresh_if_target_changed(
                        target_changed,
                        &mut status_refresh,
                        &live_input,
                        &current_target,
                    )
                    .await;
                    clear_deferred_passthroughs_if_target_changed(
                        target_changed,
                        &mut deferred_passthroughs,
                    );
                    flush_deferred_passthroughs(
                        &stream,
                        &current_target,
                        &mut deferred_passthroughs,
                        persistent_overlay_visible,
                        persistent_overlay.is_some(),
                    )
                    .await?;
                    continue;
                }
                PendingAttachAction::Write => {}
            }
            if live_input.handler.request_shutdown_if_pending() {
                let _ = emit_attach_stop(&stream, &current_target).await;
                return Ok(());
            }

            tokio::select! {
                biased;
                result = shutdown.changed() => {
                    let _ = result;
                    let _ = emit_attach_stop(&stream, &current_target).await;
                    return Ok(());
                }
                _ = wait_for_refresh_deadline(pane_refresh.deadline()) => {
                    pane_refresh.clear();
                    if closing.load(Ordering::SeqCst) {
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    match apply_pending_attach_controls(
                        &mut deferred_controls,
                        attach_controls.as_mut(),
                        &mut current_target,
                        &stream,
                        &mut render_generation,
                        &mut overlay_generation,
                        &mut persistent_overlay,
                        &mut persistent_overlay_visible,
                        &mut persistent_overlay_state_id,
                        &mut locked,
                    )
                    .await?
                    {
                        PendingAttachAction::Exit => {
                            return Ok(());
                        }
                        PendingAttachAction::Continue { target_changed } => {
                            reschedule_status_refresh_if_target_changed(
                                target_changed,
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            flush_deferred_passthroughs(
                                &stream,
                                &current_target,
                                &mut deferred_passthroughs,
                                persistent_overlay_visible,
                                persistent_overlay.is_some(),
                            )
                            .await?;
                            continue;
                        }
                        PendingAttachAction::Write => {
                            if locked {
                                continue;
                            }
                            if closing.load(Ordering::SeqCst) {
                                let _ = emit_attach_stop(&stream, &current_target).await;
                                return Ok(());
                            }
                            let session_name = current_target.session_name.clone();
                            live_input
                                .handler
                                .refresh_attached_session(&session_name)
                                .await;
                        }
                    }
                }
                _ = wait_for_refresh_deadline(status_refresh.deadline()) => {
                    if closing.load(Ordering::SeqCst) {
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    match apply_pending_attach_controls(
                        &mut deferred_controls,
                        attach_controls.as_mut(),
                        &mut current_target,
                        &stream,
                        &mut render_generation,
                        &mut overlay_generation,
                        &mut persistent_overlay,
                        &mut persistent_overlay_visible,
                        &mut persistent_overlay_state_id,
                        &mut locked,
                    )
                    .await?
                    {
                        PendingAttachAction::Exit => {
                            return Ok(());
                        }
                        PendingAttachAction::Continue { target_changed } => {
                            reschedule_status_refresh_for_target(
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            flush_deferred_passthroughs(
                                &stream,
                                &current_target,
                                &mut deferred_passthroughs,
                                persistent_overlay_visible,
                                persistent_overlay.is_some(),
                            )
                            .await?;
                            continue;
                        }
                        PendingAttachAction::Write => {}
                    }
                    let session_name = current_target.session_name.clone();
                    if locked {
                        reschedule_status_refresh_for_session(
                            &mut status_refresh,
                            &live_input,
                            &session_name,
                        )
                        .await;
                        continue;
                    }
                    let _ = live_input
                        .handler
                        .refresh_attached_client_status(live_input.attach_pid, &session_name)
                        .await;
                    reschedule_status_refresh_for_session(
                        &mut status_refresh,
                        &live_input,
                        &session_name,
                    )
                    .await;
                }
                result = read_socket_bytes(&stream, &mut decoder, &mut socket_read_buffer) => {
                    if !result? {
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                }
                control = recv_attach_control(&mut deferred_controls, attach_controls.as_mut()) => {
                    match control {
                        Some(AttachControl::Detach) => {
                            let _ = emit_attach_stop(&stream, &current_target).await;
                            let _ = emit_detached_message(&stream, &current_target).await;
                            return Ok(());
                        }
                        Some(AttachControl::Exited) => {
                            let _ = emit_attach_stop(&stream, &current_target).await;
                            let _ = emit_exited_message(&stream).await;
                            return Ok(());
                        }
                        Some(AttachControl::DetachKill) => {
                            emit_attach_stop(&stream, &current_target).await?;
                            emit_attach_message(&stream, &AttachMessage::DetachKill).await?;
                            return Ok(());
                        }
                        Some(AttachControl::DetachExecShellCommand(command)) => {
                            emit_attach_stop(&stream, &current_target).await?;
                            emit_attach_message(
                                &stream,
                                &AttachMessage::DetachExecShellCommand(command),
                            )
                            .await?;
                            return Ok(());
                        }
                        Some(AttachControl::Switch(next_target)) => {
                            if is_stale_persistent_switch(
                                persistent_overlay_state_id,
                                next_target.as_ref(),
                            ) {
                                continue;
                            }
                            render_generation = render_generation.saturating_add(1);
                            pending_input.clear();
                            deferred_passthroughs.clear();
                            let pending_overlay = take_pending_persistent_overlay_for_state(
                                attach_controls.as_mut(),
                                &mut deferred_controls,
                                next_target.persistent_overlay_state_id,
                                render_generation,
                                overlay_generation,
                            );
                            let replacement_frame = pending_overlay
                                .as_ref()
                                .map(|overlay| overlay.frame.clone())
                                .or_else(|| {
                                    replacement_persistent_overlay_frame(
                                        &persistent_overlay,
                                        persistent_overlay_visible,
                                        next_target.as_ref(),
                                    )
                                });
                            let clear_screen = switch_requires_screen_clear(
                                persistent_overlay_visible,
                                persistent_overlay.is_some(),
                                persistent_overlay_state_id,
                                current_target.persistent_overlay_state_id,
                                next_target.persistent_overlay_state_id,
                            );
                            if replacement_frame.is_none() {
                                persistent_overlay.take();
                                persistent_overlay_visible = false;
                            }
                            if let Some(overlay) = pending_overlay.as_ref() {
                                overlay_generation = overlay.overlay_generation;
                            }
                            switch_attach_target(
                                &stream,
                                &mut current_target,
                                *next_target,
                                clear_screen,
                                replacement_frame.as_deref(),
                            )
                            .await?;
                            status_refresh.reschedule(
                                live_input
                                    .handler
                                    .attached_status_interval(&current_target.session_name)
                                    .await,
                            );
                            if let Some(overlay) = pending_overlay {
                                update_persistent_overlay_cache(
                                    &mut persistent_overlay,
                                    &mut persistent_overlay_visible,
                                    &overlay,
                                );
                            }
                            persistent_overlay_state_id = current_target.persistent_overlay_state_id;
                            if let Some(barrier_state_id) = persistent_overlay_state_id {
                                discard_stale_persistent_overlays(
                                    attach_controls.as_mut(),
                                    &mut deferred_controls,
                                    barrier_state_id,
                                );
                            }
                        }
                        Some(AttachControl::AdvancePersistentOverlayState(state_id)) => {
                            let previous_overlay_state_id = persistent_overlay_state_id;
                            advance_persistent_overlay_state(
                                &mut persistent_overlay_state_id,
                                attach_controls.as_mut(),
                                &mut deferred_controls,
                                state_id,
                            );
                            redraw_after_persistent_overlay_state_advance(
                                &stream,
                                &current_target,
                                &mut persistent_overlay,
                                &mut persistent_overlay_visible,
                                previous_overlay_state_id,
                                persistent_overlay_state_id,
                                persistent_overlay_replacement_pending(
                                    &deferred_controls,
                                    persistent_overlay_state_id,
                                ),
                            )
                            .await?;
                        }
                        Some(AttachControl::Overlay(overlay)) => {
                            if !accept_persistent_overlay_state(
                                &mut persistent_overlay_state_id,
                                &overlay,
                            ) {
                                continue;
                            }
                            let persistent_clear = overlay.persistent && overlay.frame.is_empty();
                            if persistent_clear
                                || should_emit_overlay(
                                    render_generation,
                                    &mut overlay_generation,
                                    &overlay,
                                )
                            {
                                update_persistent_overlay_cache(
                                    &mut persistent_overlay,
                                    &mut persistent_overlay_visible,
                                    &overlay,
                                );
                                if defer_persistent_clear(
                                    persistent_clear,
                                    &deferred_controls,
                                    persistent_overlay_state_id,
                                ) {
                                    continue;
                                }
                                let clear_frame =
                                    persistent_clear.then(|| clear_then_base_frame(&current_target));
                                emit_render_frame(
                                    &stream,
                                    &current_target.outer_terminal,
                                    clear_frame.as_deref().unwrap_or(&overlay.frame),
                                )
                                .await?;
                                flush_deferred_passthroughs(
                                    &stream,
                                    &current_target,
                                    &mut deferred_passthroughs,
                                    persistent_overlay_visible,
                                    persistent_overlay.is_some(),
                                )
                                .await?;
                            }
                        }
                        Some(AttachControl::Write(bytes)) => {
                            emit_attach_bytes(&stream, &bytes).await?;
                        }
                        Some(AttachControl::LockShellCommand(command)) => {
                            locked = true;
                            emit_attach_message(&stream, &AttachMessage::LockShellCommand(command))
                                .await?;
                        }
                        Some(AttachControl::Suspend) => {
                            locked = true;
                            emit_attach_message(&stream, &AttachMessage::Suspend).await?;
                        }
                        None => attach_controls = None,
                    }
                }
                result = recv_pane_output_optional(current_target.pane_output.as_mut()) => {
                    let Some(item) = result? else {
                        current_target.pane_output = None;
                        continue;
                    };
                    let (bytes, passthroughs) = match item {
                        OutputCursorItem::Event(event) => event.into_parts(),
                        OutputCursorItem::Gap(_) => {
                            pane_refresh.schedule_now();
                            continue;
                        }
                    };
                    if bytes.is_empty() {
                        current_target.pane_output = None;
                        continue;
                    }
                    if closing.load(Ordering::SeqCst) {
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    match apply_pending_attach_controls(
                        &mut deferred_controls,
                        attach_controls.as_mut(),
                        &mut current_target,
                        &stream,
                        &mut render_generation,
                        &mut overlay_generation,
                        &mut persistent_overlay,
                        &mut persistent_overlay_visible,
                        &mut persistent_overlay_state_id,
                        &mut locked,
                    )
                    .await?
                    {
                        PendingAttachAction::Exit => {
                            return Ok(());
                        }
                        PendingAttachAction::Continue { target_changed } => {
                            reschedule_status_refresh_if_target_changed(
                                target_changed,
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            continue;
                        }
                        PendingAttachAction::Write => {
                            if locked {
                                continue;
                            }
                            if closing.load(Ordering::SeqCst) {
                                let _ = emit_attach_stop(&stream, &current_target).await;
                                return Ok(());
                            }
                            if persistent_overlay_visible || persistent_overlay.is_some() {
                                defer_passthroughs(&mut deferred_passthroughs, passthroughs);
                                pane_refresh.schedule_now();
                                continue;
                            }
                            defer_passthroughs(&mut deferred_passthroughs, passthroughs);
                            let passthrough_frame = take_passthrough_frame(
                                &current_target,
                                &mut deferred_passthroughs,
                            );
                            match current_target
                                .live_pane
                                .as_mut()
                                .map(|pane| pane.render_delta_from_transcript())
                            {
                                Some(PaneRenderDelta::Incremental(delta)) => {
                                    if let Some(cursor_style) = delta.cursor_style() {
                                        if let Some(sequence) = current_target
                                            .outer_terminal
                                            .render_cursor_style_transition(
                                                Some(current_target.cursor_style),
                                                cursor_style,
                                            )
                                        {
                                            emit_attach_bytes(&stream, sequence.as_bytes())
                                                .await?;
                                        }
                                        current_target.cursor_style = cursor_style;
                                    }
                                    emit_render_frame(
                                        &stream,
                                        &current_target.outer_terminal,
                                        delta.frame(),
                                    )
                                    .await?;
                                    emit_attach_bytes(&stream, &passthrough_frame).await?;
                                }
                                Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                                    pane_refresh.schedule_now();
                                    emit_attach_bytes(&stream, &passthrough_frame).await?;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    .await;

    if result.is_err() {
        let _ = emit_attach_stop(&stream, &current_target).await;
    }

    result
}

#[cfg(any(unix, windows))]
async fn reschedule_status_refresh_if_target_changed(
    target_changed: bool,
    status_refresh: &mut AttachStatusRefreshScheduler,
    live_input: &LiveAttachInputContext,
    current_target: &types::OpenAttachTarget,
) {
    if target_changed {
        reschedule_status_refresh_for_target(status_refresh, live_input, current_target).await;
    }
}

#[cfg(any(unix, windows))]
async fn reschedule_status_refresh_for_target(
    status_refresh: &mut AttachStatusRefreshScheduler,
    live_input: &LiveAttachInputContext,
    current_target: &types::OpenAttachTarget,
) {
    reschedule_status_refresh_for_session(status_refresh, live_input, &current_target.session_name)
        .await;
}

#[cfg(any(unix, windows))]
async fn reschedule_status_refresh_for_session(
    status_refresh: &mut AttachStatusRefreshScheduler,
    live_input: &LiveAttachInputContext,
    session_name: &rmux_proto::SessionName,
) {
    status_refresh.reschedule(
        live_input
            .handler
            .attached_status_interval(session_name)
            .await,
    );
}

/// Passthrough-mode attach forwarder.
///
/// Companion to [`forward_attach`] for sessions where the
/// `passthrough` option is `on`. The host terminal is never put into
/// alt-screen, no status bar / overlay / chrome is emitted, and
/// inner-PTY bytes flow verbatim from the pane output ring to the
/// client socket. The only sequence the server prepends is the
/// pane's current rendered frame, so the client sees the inner
/// program's existing visible state right after attach without
/// waiting for the next PTY emit.
///
/// Pane operations are gated upstream (see
/// `handler_support::reject_pane_op_in_passthrough`); this loop
/// assumes the session is single-pane and does not handle pane
/// switching, overlays, copy-mode, or status redraws.
#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_attach_passthrough(
    stream: LocalStream,
    target: AttachTarget,
    initial_socket_bytes: Vec<u8>,
    mut shutdown: watch::Receiver<()>,
    mut control_rx: mpsc::UnboundedReceiver<AttachControl>,
    closing: Arc<AtomicBool>,
    _persistent_overlay_epoch: Arc<AtomicU64>,
    live_input: LiveAttachInputContext,
) -> io::Result<()> {
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut socket_read_buffer = [0_u8; READ_BUFFER_SIZE];
    let mut current_target = open_attach_target(target)?;
    let mut locked = false;
    decoder.push_bytes(&initial_socket_bytes);

    emit_initial_passthrough_state(&stream, &current_target, &live_input).await?;

    loop {
        if closing.load(Ordering::SeqCst) {
            return passthrough_exit(&stream, &current_target, "closing flag set").await;
        }
        // Drain anything already buffered in the socket without
        // blocking — keeps keystrokes from queueing behind output.
        loop {
            match try_read_socket_bytes(&stream, &mut decoder, &mut socket_read_buffer)? {
                TrySocketRead::Read => {}
                TrySocketRead::Closed => {
                    return passthrough_exit(&stream, &current_target, "client socket closed (drain)").await;
                }
                TrySocketRead::WouldBlock => break,
            }
        }
        process_socket_messages(
            &mut decoder,
            &stream,
            &live_input,
            &mut pending_input,
            &mut locked,
        )
        .await?;

        tokio::select! {
            biased;
            result = shutdown.changed() => {
                let _ = result;
                return passthrough_exit(&stream, &current_target, "server shutdown").await;
            }
            control = control_rx.recv() => {
                match control {
                    None => return passthrough_exit(&stream, &current_target, "control channel closed").await,
                    Some(AttachControl::Detach) => {
                        return passthrough_exit(&stream, &current_target, "detach").await;
                    }
                    Some(AttachControl::Exited) => {
                        return passthrough_exit(&stream, &current_target, "pane exited").await;
                    }
                    Some(AttachControl::DetachKill) => {
                        return passthrough_exit(&stream, &current_target, "detach-kill").await;
                    }
                    Some(AttachControl::Switch(next_target)) => {
                        switch_passthrough_target(
                            &stream,
                            &mut current_target,
                            *next_target,
                            &live_input,
                        )
                        .await?;
                    }
                    Some(_) => {
                        // Other control variants (Overlay, Write,
                        // LockShellCommand, Suspend, AdvancePersistentOverlayState,
                        // DetachExecShellCommand) are no-ops in passthrough.
                    }
                }
            }
            output = recv_pane_output_optional(current_target.pane_output.as_mut()) => {
                let Some(item) = output? else {
                    current_target.pane_output = None;
                    continue;
                };
                let (bytes, _passthroughs) = match item {
                    OutputCursorItem::Event(event) => event.into_parts(),
                    OutputCursorItem::Gap(_) => continue,
                };
                emit_attach_bytes(&stream, &bytes).await?;
            }
            socket = read_socket_bytes(&stream, &mut decoder, &mut socket_read_buffer) => {
                if !socket? {
                    return passthrough_exit(&stream, &current_target, "client socket closed").await;
                }
            }
        }
    }
}

/// Common shutdown path for [`forward_attach_passthrough`]. Emits the
/// outer-terminal attach-stop sequence (so the client recognises the
/// teardown instead of erroring with "stream closed before attach-stop"),
/// logs the trigger, and returns `Ok(())`.
///
/// `emit_attach_stop` failures are intentionally swallowed: by the time
/// we're tearing down the connection, the peer may have closed already
/// and the write will fail with `BrokenPipe` / `ConnectionReset`. That's
/// a benign race, not something to escalate.
#[cfg(any(unix, windows))]
async fn passthrough_exit(
    stream: &LocalStream,
    current_target: &types::OpenAttachTarget,
    reason: &'static str,
) -> io::Result<()> {
    debug!(reason, session = %current_target.session_name, "passthrough attach forwarder exiting");
    let _ = emit_attach_stop(stream, current_target).await;
    Ok(())
}

/// Emits the active pane's recent state to the client on (re)attach.
///
/// In a passthrough session this is the replay log's
/// `[snapshot] ++ [raw]` when a log exists. When no log exists — e.g.
/// the session was created in normal mode and then flipped to
/// passthrough via `set-option`, so the pane never opened a replay
/// log — we deliberately do **not** fall back to `render_frame`: that
/// frame is built by the chrome-aware renderer and contains the
/// status bar / borders. Painting it would flash chrome at the user
/// despite passthrough mode promising none. A minimal SGR-reset +
/// home + clear is enough; the inner program's next emit paints
/// real content.
///
/// Prefixed with an OSC title nudge so the host tab label carries
/// the rmux session name until the inner program sets its own.
#[cfg(any(unix, windows))]
async fn emit_initial_passthrough_state(
    stream: &LocalStream,
    current_target: &super::pane_io::types::OpenAttachTarget,
    live_input: &LiveAttachInputContext,
) -> io::Result<()> {
    emit_attach_bytes(
        stream,
        &passthrough_title_sequence(&current_target.session_name),
    )
    .await?;
    if let Some(replay) = passthrough_replay_bytes_for_target(current_target, live_input).await {
        emit_attach_bytes(stream, &replay).await
    } else {
        emit_attach_bytes(stream, PASSTHROUGH_FALLBACK_RESET).await
    }
}

/// SGR reset → cursor home → erase screen. Used when a passthrough
/// attach/switch has no replay log to paint and we explicitly do
/// **not** want the chrome-laden `render_frame` (see
/// [`emit_initial_passthrough_state`]).
#[cfg(any(unix, windows))]
const PASSTHROUGH_FALLBACK_RESET: &[u8] = b"\x1b[m\x1b[H\x1b[2J";

/// Encodes an OSC 0 (set icon name + window title) sequence with a
/// rmux-tagged session label. Emitted by the passthrough forwarder on
/// attach and on window switch so the host terminal's title bar
/// reflects the rmux context — the inner program is free to override
/// with its own title on its next emit.
#[cfg(any(unix, windows))]
fn passthrough_title_sequence(session_name: &rmux_proto::SessionName) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16 + session_name.as_str().len());
    bytes.extend_from_slice(b"\x1b]0;rmux: ");
    bytes.extend_from_slice(session_name.as_str().as_bytes());
    bytes.extend_from_slice(b"\x07");
    bytes
}

/// Resolves the bytes a passthrough client needs to see in order to
/// reproduce the pane's current state — replay log when available,
/// otherwise `None` (caller falls back to `render_frame`).
#[cfg(any(unix, windows))]
async fn passthrough_replay_bytes_for_target(
    current_target: &super::pane_io::types::OpenAttachTarget,
    live_input: &LiveAttachInputContext,
) -> Option<Vec<u8>> {
    let pane_id = current_target.active_pane_id?;
    let log = live_input
        .handler
        .passthrough_log_for_pane(&current_target.session_name, pane_id)
        .await?;
    let log = log.lock().ok()?;
    let (snapshot, raw) = log.replay_parts();
    if snapshot.is_empty() && raw.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(snapshot.len() + raw.len());
    bytes.extend_from_slice(snapshot);
    bytes.extend_from_slice(raw);
    Some(bytes)
}

/// Handles `AttachControl::Switch` inside the passthrough loop.
///
/// Swaps the attached pane output to the new window's pane, resets
/// the host terminal state, then emits the new pane's replay log so
/// the client sees the new window's recent history in its scrollback
/// + the new window's current visible state.
#[cfg(any(unix, windows))]
async fn switch_passthrough_target(
    stream: &LocalStream,
    current_target: &mut super::pane_io::types::OpenAttachTarget,
    next_target: AttachTarget,
    live_input: &LiveAttachInputContext,
) -> io::Result<()> {
    *current_target = open_attach_target(next_target)?;
    // Nudge the new window's foreground program into a repaint.
    //
    // Streaming TUIs (claude, tail -f) don't need this — they emit on
    // their own clock. But lazy-redraw TUIs (vim mid-edit, less, htop
    // between ticks) only repaint when *something* tells them to.
    // Faking a winsize flap is jarring; sending SIGWINCH directly to
    // the slave's foreground pgrp is the canonical nudge. No-op when
    // the pane has no fg pgrp (newly forked, child already exited).
    #[cfg(unix)]
    {
        let _ = rmux_os::process::unix::winch_foreground_pgrp(
            current_target.pane_master.as_fd(),
        );
    }
    // Reset the host title to a rmux-tagged label so a window-switch
    // away from a TUI that set its own title (e.g. `claude`) doesn't
    // leave that title stuck on the new window. The new window's
    // inner program will override on its next emit.
    emit_attach_bytes(
        stream,
        &passthrough_title_sequence(&current_target.session_name),
    )
    .await?;
    // The replay log's snapshot already contains a reset prefix
    // (?1049l, soft reset, SGR0, clear, home) so we don't need an
    // explicit clear here when a log is present. When no log exists,
    // emit a minimal reset before painting render_frame so the old
    // window's content doesn't bleed under.
    if let Some(replay) = passthrough_replay_bytes_for_target(current_target, live_input).await {
        emit_attach_bytes(stream, &replay).await
    } else {
        // Same constraint as on first attach: do not paint
        // `render_frame`, which would leak chrome from the
        // non-passthrough renderer.
        emit_attach_bytes(stream, PASSTHROUGH_FALLBACK_RESET).await
    }
}

#[cfg(any(unix, windows))]
async fn process_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &LocalStream,
    live_input: &LiveAttachInputContext,
    pending_input: &mut Vec<u8>,
    locked: &mut bool,
) -> io::Result<()> {
    while let Some(message) = decoder.next_message().map_err(invalid_attach_message)? {
        match message {
            AttachMessage::Data(bytes) => {
                if *locked {
                    pending_input.clear();
                    continue;
                }
                live_input
                    .handler
                    .handle_attached_live_input(live_input.attach_pid, pending_input, &bytes)
                    .await?
            }
            AttachMessage::Keystroke(keystroke) => {
                let forwarded_to_pane = if *locked {
                    pending_input.clear();
                    false
                } else {
                    live_input
                        .handler
                        .handle_attached_live_input_inner(
                            live_input.attach_pid,
                            pending_input,
                            keystroke.bytes(),
                        )
                        .await?
                };
                let response = live_input
                    .handler
                    .handle_attached_keystroke(
                        live_input.attach_pid,
                        &keystroke,
                        !forwarded_to_pane,
                    )
                    .await
                    .map_err(io::Error::other)?;
                emit_attach_frame(stream, &AttachMessage::KeyDispatched(response)).await?;
            }
            AttachMessage::Resize(size) => {
                live_input
                    .handler
                    .handle_attached_resize(live_input.attach_pid, size)
                    .await
                    .map_err(io::Error::other)?;
            }
            AttachMessage::ResizeGeometry(geometry) => {
                live_input
                    .handler
                    .handle_attached_resize_geometry(live_input.attach_pid, geometry)
                    .await
                    .map_err(io::Error::other)?;
            }
            AttachMessage::Lock(_) | AttachMessage::LockShellCommand(_) => {
                return Err(io::Error::other(
                    "received unexpected lock message from attach client",
                ));
            }
            AttachMessage::Suspend
            | AttachMessage::DetachKill
            | AttachMessage::DetachExec(_)
            | AttachMessage::DetachExecShellCommand(_) => {
                return Err(io::Error::other(
                    "received unexpected control action from attach client",
                ));
            }
            AttachMessage::Unlock => {
                *locked = false;
                live_input
                    .handler
                    .handle_attached_unlock(live_input.attach_pid)
                    .await;
                if let Ok(session_name) = live_input
                    .handler
                    .attached_session_name(live_input.attach_pid)
                    .await
                {
                    live_input
                        .handler
                        .refresh_attached_client(live_input.attach_pid, &session_name)
                        .await;
                }
            }
            AttachMessage::KeyDispatched(_) => {
                return Err(io::Error::other(
                    "received unexpected key dispatch acknowledgement from attach client",
                ));
            }
        }
    }

    Ok(())
}

#[cfg(all(test, unix))]
mod tests;
