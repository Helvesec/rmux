#[cfg(unix)]
use rmux_ipc::LocalStream;
#[cfg(unix)]
use rmux_proto::{AttachFrameDecoder, AttachMessage};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU64};
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use std::{collections::VecDeque, io, sync::atomic::Ordering};
#[cfg(unix)]
use tokio::sync::mpsc;
#[cfg(unix)]
use tokio::sync::watch;

const READ_BUFFER_SIZE: usize = 8192;
mod control;
mod persistent_overlay;
#[cfg(unix)]
mod reader;
mod types;
mod wire;

#[cfg(unix)]
use control::{
    apply_pending_attach_controls, recv_attach_control,
    redraw_after_persistent_overlay_state_advance, should_emit_overlay, switch_attach_target,
    PendingAttachAction,
};
#[cfg(unix)]
use persistent_overlay::{
    accept_persistent_overlay_state, advance_persistent_overlay_state, clear_then_base_frame,
    discard_stale_persistent_overlays, is_stale_persistent_switch,
    persistent_overlay_replacement_pending, prime_persistent_overlay_barriers,
    replacement_persistent_overlay_frame, update_persistent_overlay_cache,
};
#[cfg(unix)]
pub(crate) use reader::spawn_pane_output_reader;
#[cfg(unix)]
pub(crate) use types::{
    pane_output_channel, AttachControl, AttachTarget, HandleOutcome, LiveAttachInputContext,
    OverlayFrame, PaneAlertCallback, PaneAlertEvent, PaneExitCallback, PaneExitEvent,
    PaneOutputSender,
};
#[cfg(unix)]
use wire::{
    emit_attach_bytes, emit_attach_frame, emit_attach_message, emit_attach_stop,
    emit_detached_message, emit_exited_message, emit_render_frame, invalid_attach_message,
    open_attach_target, read_socket_bytes, recv_pane_output_optional, try_read_socket_bytes,
    TrySocketRead,
};

#[allow(clippy::too_many_arguments)]
#[cfg(unix)]
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
            loop {
                match try_read_socket_bytes(&stream, &mut decoder, &mut socket_read_buffer)? {
                    TrySocketRead::Read => {}
                    TrySocketRead::Closed => return Ok(()),
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
                PendingAttachAction::Continue => continue,
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
                result = read_socket_bytes(&stream, &mut decoder, &mut socket_read_buffer) => {
                    if !result? {
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
                        Some(AttachControl::DetachExec(command)) => {
                            emit_attach_stop(&stream, &current_target).await?;
                            emit_attach_message(&stream, &AttachMessage::DetachExec(command)).await?;
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
                            let replacement_frame = replacement_persistent_overlay_frame(
                                &persistent_overlay,
                                persistent_overlay_visible,
                                next_target.as_ref(),
                            );
                            let had_persistent_overlay =
                                persistent_overlay_visible || persistent_overlay.is_some();
                            let stale_persistent_overlay_on_screen =
                                persistent_overlay_state_id
                                    != current_target.persistent_overlay_state_id;
                            let next_target_has_no_persistent_overlay =
                                next_target.persistent_overlay_state_id.is_none();
                            let leaving_persistent_overlay =
                                current_target.persistent_overlay_state_id.is_some()
                                    && next_target_has_no_persistent_overlay;
                            if replacement_frame.is_none() {
                                persistent_overlay.take();
                                persistent_overlay_visible = false;
                            }
                            switch_attach_target(
                                &stream,
                                &mut current_target,
                                *next_target,
                                had_persistent_overlay
                                    || stale_persistent_overlay_on_screen
                                    || leaving_persistent_overlay
                                    || next_target_has_no_persistent_overlay,
                                replacement_frame.as_deref(),
                            )
                            .await?;
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
                                let clear_frame =
                                    persistent_clear.then(|| clear_then_base_frame(&current_target));
                                update_persistent_overlay_cache(
                                    &mut persistent_overlay,
                                    &mut persistent_overlay_visible,
                                    &overlay,
                                );
                                emit_render_frame(
                                    &stream,
                                    &current_target.outer_terminal,
                                    clear_frame.as_deref().unwrap_or(&overlay.frame),
                                )
                                .await?;
                            }
                        }
                        Some(AttachControl::Write(bytes)) => {
                            emit_attach_bytes(&stream, &bytes).await?;
                        }
                        Some(AttachControl::Lock(command)) => {
                            locked = true;
                            emit_attach_message(&stream, &AttachMessage::Lock(command)).await?;
                        }
                        Some(AttachControl::Suspend) => {
                            locked = true;
                            emit_attach_message(&stream, &AttachMessage::Suspend).await?;
                        }
                        None => attach_controls = None,
                    }
                }
                result = recv_pane_output_optional(current_target.pane_output.as_mut()) => {
                    let Some(bytes) = result? else {
                        current_target.pane_output = None;
                        continue;
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
                        PendingAttachAction::Continue => continue,
                        PendingAttachAction::Write => {
                            if locked {
                                continue;
                            }
                            if closing.load(Ordering::SeqCst) {
                                let _ = emit_attach_stop(&stream, &current_target).await;
                                return Ok(());
                            }
                            loop {
                                match try_read_socket_bytes(
                                    &stream,
                                    &mut decoder,
                                    &mut socket_read_buffer,
                                )? {
                                    TrySocketRead::Read => {}
                                    TrySocketRead::Closed => return Ok(()),
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
                            let session_name = current_target.session_name.clone();
                            live_input
                                .handler
                                .refresh_attached_session(&session_name)
                                .await;
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

#[cfg(unix)]
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
            AttachMessage::Lock(_) => {
                return Err(io::Error::other(
                    "received unexpected lock message from attach client",
                ));
            }
            AttachMessage::Suspend | AttachMessage::DetachKill | AttachMessage::DetachExec(_) => {
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
