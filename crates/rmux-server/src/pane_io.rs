#[cfg(any(unix, windows))]
use rmux_proto::{AttachFrameDecoder, AttachMessage};
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(any(unix, windows))]
use std::{collections::VecDeque, io, sync::atomic::Ordering};
#[cfg(any(unix, windows))]
use tokio::sync::mpsc;
#[cfg(any(unix, windows))]
use tokio::sync::watch;
#[cfg(any(unix, windows))]
use tokio::time::{Duration, Instant};

const READ_BUFFER_SIZE: usize = 64 * 1024;
#[cfg(any(unix, windows))]
const ATTACH_INTERACTIVE_OUTPUT_WINDOW: Duration = Duration::from_millis(250);
#[cfg(any(unix, windows))]
const ATTACH_INPUT_STACK_PAYLOAD: usize = 1024;
#[cfg(unix)]
const MAX_PREDICTED_LOCAL_ECHO_BYTES: usize = 16;
#[cfg(unix)]
const PREDICTED_LOCAL_ECHO_TIMEOUT: Duration = Duration::from_millis(250);
mod attach_output_batch;
mod attach_transport;
mod control;
mod deferred_passthrough;
mod exit_log;
mod live_render;
mod passthrough;
mod pending_escape;
mod persistent_overlay;
mod reader;
mod refresh_scheduler;
mod types;
mod wire;

#[cfg(any(unix, windows))]
use crate::renderer::{PaneRenderDelta, PaneRenderDeltaFrame};
#[cfg(any(unix, windows))]
use attach_output_batch::{collect_attach_output_batch, AttachOutputBatch};
#[cfg(all(any(unix, windows), feature = "web"))]
pub(crate) use attach_transport::in_process_attach_pair;
use attach_transport::{AttachTransport, TryAttachRead};
#[cfg(any(unix, windows))]
use control::{
    apply_pending_attach_controls, coalesce_render_switches, recv_attach_control,
    redraw_after_persistent_overlay_state_advance, should_emit_overlay, switch_attach_target,
    take_pending_live_passthroughs, PendingAttachAction,
};
#[cfg(any(unix, windows))]
use deferred_passthrough::{
    clear_deferred_passthroughs_if_target_changed, defer_passthroughs, flush_deferred_passthroughs,
    take_passthrough_frame_with_live_passthroughs,
};
#[cfg(any(unix, windows))]
use exit_log::{record_attach_error, record_attach_exit, AttachExitReason};
pub(crate) use live_render::LivePaneRender;
#[cfg(any(unix, windows))]
use pending_escape::{is_pending_escape, PendingEscapeFlush};
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
#[cfg(unix)]
pub(crate) use reader::PaneOutputReaderTask;
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
    emit_coalescible_render_frame, emit_detached_attach_stop, emit_exited_attach_stop,
    emit_render_frame, invalid_attach_message, open_attach_target, read_socket_bytes,
    recv_pane_output_optional, try_read_socket_bytes,
};

#[allow(clippy::too_many_arguments)]
#[cfg(any(unix, windows))]
pub(crate) async fn forward_attach(
    stream: impl Into<AttachTransport>,
    target: AttachTarget,
    initial_socket_bytes: Vec<u8>,
    mut shutdown: watch::Receiver<()>,
    control_rx: mpsc::UnboundedReceiver<AttachControl>,
    control_backlog: Arc<AtomicUsize>,
    closing: Arc<AtomicBool>,
    persistent_overlay_epoch: Arc<AtomicU64>,
    live_input: LiveAttachInputContext,
    render_stream: bool,
) -> io::Result<()> {
    let stream = stream.into();
    let mut decoder = AttachFrameDecoder::new();
    let mut pending_input = Vec::new();
    let mut attach_controls = Some(control_rx);
    let mut deferred_controls = VecDeque::new();
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let mut current_target = open_attach_target(target, render_stream)?;
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut pane_refresh = AttachRefreshScheduler::default();
    let mut pane_refresh_requires_full = false;
    let mut close_pane_output_after_refresh = false;
    let mut deferred_passthroughs = Vec::new();
    let mut last_client_input_at = None::<Instant>;
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
    emit_coalescible_render_frame(
        &stream,
        &current_target.outer_terminal,
        &current_target.render_frame,
        current_target.render_stream,
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
                &control_backlog,
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
            match apply_pending_attach_controls(
                &mut deferred_controls,
                attach_controls.as_mut(),
                &control_backlog,
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
                PendingAttachAction::Exit(reason) => {
                    log_attach_exit(&live_input, &current_target, reason);
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
                    clear_close_pane_output_after_refresh_if_target_changed(
                        target_changed,
                        &mut close_pane_output_after_refresh,
                    );
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
                PendingAttachAction::InteractiveInput => {
                    mark_attach_interactive_input(&mut pane_refresh, &mut last_client_input_at);
                    pane_refresh.schedule_now();
                    continue;
                }
                PendingAttachAction::Refresh { target_changed } => {
                    reschedule_status_refresh_if_target_changed(
                        target_changed,
                        &mut status_refresh,
                        &live_input,
                        &current_target,
                    )
                    .await;
                    clear_close_pane_output_after_refresh_if_target_changed(
                        target_changed,
                        &mut close_pane_output_after_refresh,
                    );
                    clear_deferred_passthroughs_if_target_changed(
                        target_changed,
                        &mut deferred_passthroughs,
                    );
                    schedule_attach_render_refresh(
                        &mut pane_refresh,
                        &mut pane_refresh_requires_full,
                        &live_input,
                    )
                    .await;
                    continue;
                }
                PendingAttachAction::Write => {}
            }
            // A pending repaint must not stop input from reaching the pane.
            // The repaint is rendered from the current transcript when its
            // deadline fires, so fresh input can safely pull the deadline in.
            loop {
                match try_read_socket_bytes(&stream, &mut decoder)? {
                    TryAttachRead::Read => {}
                    TryAttachRead::Closed => {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachStreamClosed,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    TryAttachRead::WouldBlock => break,
                }
            }
            process_attach_socket_messages(
                &mut decoder,
                &stream,
                &live_input,
                &mut current_target,
                &mut pending_input,
                &mut locked,
                &mut pane_refresh,
                &mut pending_escape_flush,
                &mut last_client_input_at,
            )
            .await?;
            prime_persistent_overlay_barriers(
                &mut persistent_overlay_state_id,
                attach_controls.as_mut(),
                &mut deferred_controls,
                &control_backlog,
            );
            match apply_pending_attach_controls(
                &mut deferred_controls,
                attach_controls.as_mut(),
                &control_backlog,
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
                PendingAttachAction::Exit(reason) => {
                    log_attach_exit(&live_input, &current_target, reason);
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
                    clear_close_pane_output_after_refresh_if_target_changed(
                        target_changed,
                        &mut close_pane_output_after_refresh,
                    );
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
                PendingAttachAction::InteractiveInput => {
                    mark_attach_interactive_input(&mut pane_refresh, &mut last_client_input_at);
                    pane_refresh.schedule_now();
                    continue;
                }
                PendingAttachAction::Refresh { target_changed } => {
                    reschedule_status_refresh_if_target_changed(
                        target_changed,
                        &mut status_refresh,
                        &live_input,
                        &current_target,
                    )
                    .await;
                    clear_close_pane_output_after_refresh_if_target_changed(
                        target_changed,
                        &mut close_pane_output_after_refresh,
                    );
                    clear_deferred_passthroughs_if_target_changed(
                        target_changed,
                        &mut deferred_passthroughs,
                    );
                    schedule_attach_render_refresh(
                        &mut pane_refresh,
                        &mut pane_refresh_requires_full,
                        &live_input,
                    )
                    .await;
                    continue;
                }
                PendingAttachAction::Write => {}
            }
            if live_input.handler.request_shutdown_if_pending() {
                log_attach_exit(
                    &live_input,
                    &current_target,
                    AttachExitReason::PendingServerShutdown,
                );
                let _ = emit_attach_stop(&stream, &current_target).await;
                return Ok(());
            }

            tokio::select! {
                biased;
                result = shutdown.changed() => {
                    let _ = result;
                    log_attach_exit(
                        &live_input,
                        &current_target,
                        AttachExitReason::ServerShutdown,
                    );
                    let _ = emit_attach_stop(&stream, &current_target).await;
                    return Ok(());
                }
                result = read_socket_bytes(&stream, &mut decoder) => {
                    if !result? {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachStreamClosed,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    process_attach_socket_messages(
                        &mut decoder,
                        &stream,
                        &live_input,
                        &mut current_target,
                        &mut pending_input,
                        &mut locked,
                        &mut pane_refresh,
                        &mut pending_escape_flush,
                        &mut last_client_input_at,
                    )
                    .await?;
                }
                _ = wait_for_refresh_deadline(pane_refresh.deadline()) => {
                    pane_refresh.clear();
                    if closing.load(Ordering::SeqCst) {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachClosingFlag,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    match apply_pending_attach_controls(
                        &mut deferred_controls,
                        attach_controls.as_mut(),
                &control_backlog,
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
                        PendingAttachAction::Exit(reason) => {
                            log_attach_exit(&live_input, &current_target, reason);
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
                            clear_close_pane_output_after_refresh_if_target_changed(
                                target_changed,
                                &mut close_pane_output_after_refresh,
                            );
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
                        PendingAttachAction::InteractiveInput => {
                            mark_attach_interactive_input(
                                &mut pane_refresh,
                                &mut last_client_input_at,
                            );
                            pane_refresh.schedule_now();
                            continue;
                        }
                        PendingAttachAction::Refresh { target_changed } => {
                            reschedule_status_refresh_if_target_changed(
                                target_changed,
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_close_pane_output_after_refresh_if_target_changed(
                                target_changed,
                                &mut close_pane_output_after_refresh,
                            );
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            schedule_attach_render_refresh(
                                &mut pane_refresh,
                                &mut pane_refresh_requires_full,
                                &live_input,
                            )
                            .await;
                            continue;
                        }
                        PendingAttachAction::Write => {
                            if locked {
                                continue;
                            }
                            if closing.load(Ordering::SeqCst) {
                                log_attach_exit(
                                    &live_input,
                                    &current_target,
                                    AttachExitReason::AttachClosingFlag,
                                );
                                let _ = emit_attach_stop(&stream, &current_target).await;
                                return Ok(());
                            }
                            let force_full_refresh = pane_refresh_requires_full
                                || persistent_overlay_visible
                                || persistent_overlay.is_some()
                                || current_target.live_pane.is_none();
                            pane_refresh_requires_full = false;
                            if force_full_refresh {
                                refresh_current_attach_client(&live_input).await;
                            } else {
                                let pending_output =
                                    collect_pending_attach_output_batch(&mut current_target);
                                let mut drained_sustained_output = false;
                                let mut live_passthroughs = Vec::new();
                                if let Some(batch) = pending_output {
                                    match batch {
                                        AttachOutputBatch::Closed => {
                                            current_target.pane_output = None;
                                        }
                                        AttachOutputBatch::Gap => {
                                            pane_refresh_requires_full = true;
                                            pane_refresh.schedule_now();
                                            continue;
                                        }
                                        AttachOutputBatch::Events {
                                            bytes: _,
                                            passthroughs,
                                            close_after_render,
                                            sustained,
                                        } => {
                                            drained_sustained_output = sustained;
                                            live_passthroughs = passthroughs;
                                            if close_after_render {
                                                close_pane_output_after_refresh = true;
                                            }
                                        }
                                    }
                                }
                                let passthrough_frame = take_passthrough_frame_with_live_passthroughs(
                                    &current_target,
                                    &mut deferred_passthroughs,
                                    live_passthroughs,
                                );
                                let replaceable_render = current_target.render_stream
                                    && !drained_sustained_output
                                    && !pane_refresh.is_sustained();
                                match current_target
                                    .live_pane
                                    .as_mut()
                                    .map(|pane| {
                                        pane.render_frame_from_transcript(replaceable_render)
                                    }) {
                                    Some(PaneRenderDelta::Incremental(delta)) => {
                                        emit_live_render_frame(
                                            &stream,
                                            &mut current_target,
                                            &delta,
                                            replaceable_render,
                                        )
                                        .await?;
                                        emit_attach_bytes(&stream, &passthrough_frame).await?;
                                    }
                                    Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                                        refresh_current_attach_client(&live_input).await;
                                        emit_attach_bytes(&stream, &passthrough_frame).await?;
                                    }
                                }
                                let _ = pane_refresh.note_output_batch(drained_sustained_output);
                            }
                            if close_pane_output_after_refresh {
                                current_target.pane_output = None;
                                close_pane_output_after_refresh = false;
                            }
                        }
                    }
                }
                _ = wait_for_refresh_deadline(status_refresh.deadline()) => {
                    if closing.load(Ordering::SeqCst) {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachClosingFlag,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    match apply_pending_attach_controls(
                        &mut deferred_controls,
                        attach_controls.as_mut(),
                &control_backlog,
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
                        PendingAttachAction::Exit(reason) => {
                            log_attach_exit(&live_input, &current_target, reason);
                            return Ok(());
                        }
                        PendingAttachAction::Continue { target_changed } => {
                            reschedule_status_refresh_for_target(
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_close_pane_output_after_refresh_if_target_changed(
                                target_changed,
                                &mut close_pane_output_after_refresh,
                            );
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
                        PendingAttachAction::InteractiveInput => {
                            mark_attach_interactive_input(
                                &mut pane_refresh,
                                &mut last_client_input_at,
                            );
                            pane_refresh.schedule_now();
                            continue;
                        }
                        PendingAttachAction::Refresh { target_changed } => {
                            reschedule_status_refresh_if_target_changed(
                                target_changed,
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_close_pane_output_after_refresh_if_target_changed(
                                target_changed,
                                &mut close_pane_output_after_refresh,
                            );
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            schedule_attach_render_refresh(
                                &mut pane_refresh,
                                &mut pane_refresh_requires_full,
                                &live_input,
                            )
                            .await;
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
                _ = wait_for_refresh_deadline(pending_escape_flush.deadline()) => {
                    pending_escape_flush.clear();
                    if locked {
                        pending_input.clear();
                        continue;
                    }
                    live_input
                        .handler
                        .flush_attached_pending_escape_input(
                            live_input.attach_pid,
                            &mut pending_input,
                        )
                        .await?;
                }
                control = recv_attach_control(&mut deferred_controls, attach_controls.as_mut(), &control_backlog) => {
                    match control {
                        Some(AttachControl::Detach) => {
                            log_attach_exit(
                                &live_input,
                                &current_target,
                                AttachExitReason::AttachControlDetach,
                            );
                            let _ = emit_detached_attach_stop(&stream, &current_target).await;
                            return Ok(());
                        }
                        Some(AttachControl::Exited) => {
                            log_attach_exit(
                                &live_input,
                                &current_target,
                                AttachExitReason::AttachControlExited,
                            );
                            let _ = emit_exited_attach_stop(&stream, &current_target).await;
                            return Ok(());
                        }
                        Some(AttachControl::DetachKill) => {
                            log_attach_exit(
                                &live_input,
                                &current_target,
                                AttachExitReason::AttachControlDetachKill,
                            );
                            emit_attach_stop(&stream, &current_target).await?;
                            emit_attach_message(&stream, &AttachMessage::DetachKill).await?;
                            return Ok(());
                        }
                        Some(AttachControl::DetachExecShellCommand(command)) => {
                            log_attach_exit(
                                &live_input,
                                &current_target,
                                AttachExitReason::AttachControlDetachExec,
                            );
                            emit_attach_stop(&stream, &current_target).await?;
                            emit_attach_message(
                                &stream,
                                &AttachMessage::DetachExecShellCommand(command),
                            )
                            .await?;
                            return Ok(());
                        }
                        Some(AttachControl::Refresh) => {
                            schedule_attach_render_refresh(
                                &mut pane_refresh,
                                &mut pane_refresh_requires_full,
                                &live_input,
                            )
                            .await;
                        }
                        Some(AttachControl::InteractiveInput) => {
                            mark_attach_interactive_input(
                                &mut pane_refresh,
                                &mut last_client_input_at,
                            );
                            pane_refresh.schedule_now();
                        }
                        Some(AttachControl::Switch(next_target)) => {
                            let next_target = coalesce_render_switches(
                                next_target,
                                &mut deferred_controls,
                                attach_controls.as_mut(),
                                &control_backlog,
                            );
                            let drop_live_output = !next_target.is_coalescible_render_refresh();
                            let pending_passthroughs = if drop_live_output {
                                Vec::new()
                            } else {
                                take_pending_live_passthroughs(
                                    &mut current_target,
                                    next_target.pane_output_start_sequence,
                                )
                            };
                            if is_stale_persistent_switch(
                                persistent_overlay_state_id,
                                next_target.as_ref(),
                            ) {
                                continue;
                            }
                            close_pane_output_after_refresh = false;
                            render_generation = render_generation.saturating_add(1);
                            pending_input.clear();
                            pending_escape_flush.clear();
                            clear_deferred_passthroughs_if_target_changed(
                                drop_live_output,
                                &mut deferred_passthroughs,
                            );
                            let pending_overlay = take_pending_persistent_overlay_for_state(
                                attach_controls.as_mut(),
                                &mut deferred_controls,
                                next_target.persistent_overlay_state_id,
                                render_generation,
                                overlay_generation,
                                &control_backlog,
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
                            if !pending_passthroughs.is_empty() {
                                let passthrough_frame = take_passthrough_frame_with_live_passthroughs(
                                    &current_target,
                                    &mut deferred_passthroughs,
                                    pending_passthroughs,
                                );
                                emit_attach_bytes(&stream, &passthrough_frame).await?;
                            }
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
                                    &control_backlog,
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
                                &control_backlog,
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
                result = recv_pane_output_optional(current_target.pane_output.as_mut()), if !pane_refresh.is_pending() => {
                    let Some(item) = result? else {
                        current_target.pane_output = None;
                        continue;
                    };
                    #[cfg(unix)]
                    if let rmux_core::events::OutputCursorItem::Event(event) = &item {
                        if event.passthroughs().is_empty() {
                            match consume_predicted_echo(&mut current_target, event.bytes()) {
                                PredictedEcho::Consumed => continue,
                                PredictedEcho::Mismatch => {
                                    pane_refresh_requires_full = true;
                                    pane_refresh.schedule_immediate();
                                }
                                PredictedEcho::NoPrediction => {}
                            }
                        }
                    }
                    let item = if deferred_controls.is_empty()
                        && control_backlog.load(Ordering::Acquire) == 0
                        && !locked
                        && !persistent_overlay_visible
                        && persistent_overlay.is_none()
                        && should_treat_attach_output_as_interactive(last_client_input_at)
                    {
                        match item {
                            rmux_core::events::OutputCursorItem::Event(event)
                                if !event.is_empty()
                                    && event.byte_len() <= 512
                                    && event.passthroughs().is_empty()
                                    && pane_refresh.can_bypass_small_plain_output()
                                    && current_target.live_pane.as_ref().is_some_and(|pane| {
                                        pane.can_forward_plain_bytes(event.bytes())
                                    }) =>
                            {
                                if closing.load(Ordering::SeqCst) {
                                    log_attach_exit(
                                        &live_input,
                                        &current_target,
                                        AttachExitReason::AttachClosingFlag,
                                    );
                                    let _ = emit_attach_stop(&stream, &current_target).await;
                                    return Ok(());
                                }
                                let snapshot_synced = current_target
                                    .live_pane
                                    .as_mut()
                                    .is_some_and(|pane| pane.apply_forwarded_plain_bytes(event.bytes()));
                                if snapshot_synced {
                                    emit_attach_bytes(&stream, event.bytes()).await?;
                                    let _ = pane_refresh.note_output_batch(false);
                                    continue;
                                }
                                rmux_core::events::OutputCursorItem::Event(event)
                            }
                            other => other,
                        }
                    } else {
                        item
                    };
                    let (raw_output_bytes, passthroughs, close_after_render, sustained_output) =
                        match collect_attach_output_batch(item, current_target.pane_output.as_mut())
                        {
                        AttachOutputBatch::Closed => {
                            current_target.pane_output = None;
                            continue;
                        }
                        AttachOutputBatch::Gap => {
                            if current_target.live_pane.is_none()
                                || persistent_overlay_visible
                                || persistent_overlay.is_some()
                            {
                                pane_refresh_requires_full = true;
                            }
                            pane_refresh.schedule_sustained();
                            continue;
                        }
                        AttachOutputBatch::Events {
                            bytes,
                            passthroughs,
                            close_after_render,
                            sustained,
                        } => (bytes, passthroughs, close_after_render, sustained),
                    };
                    if closing.load(Ordering::SeqCst) {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachClosingFlag,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    match apply_pending_attach_controls(
                        &mut deferred_controls,
                        attach_controls.as_mut(),
                &control_backlog,
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
                        PendingAttachAction::Exit(reason) => {
                            log_attach_exit(&live_input, &current_target, reason);
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
                            clear_close_pane_output_after_refresh_if_target_changed(
                                target_changed,
                                &mut close_pane_output_after_refresh,
                            );
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            continue;
                        }
                        PendingAttachAction::InteractiveInput => {
                            mark_attach_interactive_input(
                                &mut pane_refresh,
                                &mut last_client_input_at,
                            );
                            pane_refresh.schedule_now();
                            continue;
                        }
                        PendingAttachAction::Refresh { target_changed } => {
                            reschedule_status_refresh_if_target_changed(
                                target_changed,
                                &mut status_refresh,
                                &live_input,
                                &current_target,
                            )
                            .await;
                            clear_close_pane_output_after_refresh_if_target_changed(
                                target_changed,
                                &mut close_pane_output_after_refresh,
                            );
                            clear_deferred_passthroughs_if_target_changed(
                                target_changed,
                                &mut deferred_passthroughs,
                            );
                            defer_passthroughs(&mut deferred_passthroughs, passthroughs);
                            schedule_attach_render_refresh(
                                &mut pane_refresh,
                                &mut pane_refresh_requires_full,
                                &live_input,
                            )
                            .await;
                            if close_after_render {
                                current_target.pane_output = None;
                            }
                            continue;
                        }
                        PendingAttachAction::Write => {
                            if locked {
                                if close_after_render {
                                    current_target.pane_output = None;
                                }
                                continue;
                            }
                            if closing.load(Ordering::SeqCst) {
                                log_attach_exit(
                                    &live_input,
                                    &current_target,
                                    AttachExitReason::AttachClosingFlag,
                                );
                                let _ = emit_attach_stop(&stream, &current_target).await;
                                return Ok(());
                            }
                            if persistent_overlay_visible || persistent_overlay.is_some() {
                                defer_passthroughs(&mut deferred_passthroughs, passthroughs);
                                pane_refresh_requires_full = true;
                                pane_refresh.schedule_now();
                                if close_after_render {
                                    current_target.pane_output = None;
                                }
                                continue;
                            }
                            if passthroughs.is_empty() && current_target.live_pane.is_some() {
                                let interactive_output =
                                    should_treat_attach_output_as_interactive(last_client_input_at);
                                let small_plain_output = raw_output_bytes.len() <= 512;
                                let output_can_bypass_render = small_plain_output
                                    && pane_refresh.can_bypass_small_plain_output();
                                if !sustained_output
                                    && output_can_bypass_render
                                    && try_forward_plain_output(
                                        &stream,
                                        &mut current_target,
                                        &raw_output_bytes,
                                    )
                                    .await?
                                {
                                    let _ = pane_refresh.note_output_batch(sustained_output);
                                    if close_after_render {
                                        current_target.pane_output = None;
                                    }
                                    continue;
                                }
                                if interactive_output {
                                    pane_refresh.note_interactive_output();
                                    let replaceable_render = current_target.render_stream;
                                    match current_target
                                        .live_pane
                                        .as_mut()
                                        .map(|pane| {
                                            pane.render_frame_from_transcript(replaceable_render)
                                        })
                                    {
                                        Some(PaneRenderDelta::Incremental(delta)) => {
                                            emit_live_render_frame(
                                                &stream,
                                                &mut current_target,
                                                &delta,
                                                replaceable_render,
                                            )
                                            .await?;
                                            if close_after_render {
                                                current_target.pane_output = None;
                                            }
                                            continue;
                                        }
                                        Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                                            pane_refresh_requires_full = true;
                                            pane_refresh.schedule_immediate();
                                        }
                                    }
                                } else if pane_refresh.note_output_batch(sustained_output) {
                                    pane_refresh.schedule_sustained();
                                } else {
                                    pane_refresh.schedule_now();
                                }
                                if close_after_render {
                                    close_pane_output_after_refresh = true;
                                }
                                continue;
                            }
                            let passthrough_frame = take_passthrough_frame_with_live_passthroughs(
                                &current_target,
                                &mut deferred_passthroughs,
                                passthroughs,
                            );
                            let replaceable_render = current_target.render_stream;
                            match current_target
                                .live_pane
                                .as_mut()
                                .map(|pane| pane.render_frame_from_transcript(replaceable_render))
                            {
                                Some(PaneRenderDelta::Incremental(delta)) => {
                                    emit_live_render_frame(
                                        &stream,
                                        &mut current_target,
                                        &delta,
                                        replaceable_render,
                                    )
                                    .await?;
                                    emit_attach_bytes(&stream, &passthrough_frame).await?;
                                }
                                Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                                    pane_refresh_requires_full = true;
                                    pane_refresh.schedule_now();
                                    emit_attach_bytes(&stream, &passthrough_frame).await?;
                                }
                            }
                            if close_after_render {
                                current_target.pane_output = None;
                            }
                        }
                    }
                }
            }
        }
    }
    .await;

    if let Err(error) = &result {
        record_attach_error(live_input.attach_pid, &current_target.session_name, error);
        let _ = emit_attach_stop(&stream, &current_target).await;
    }

    result
}

#[cfg(any(unix, windows))]
fn log_attach_exit(
    live_input: &LiveAttachInputContext,
    current_target: &types::OpenAttachTarget,
    reason: AttachExitReason,
) {
    record_attach_exit(live_input.attach_pid, &current_target.session_name, reason);
}

#[cfg(any(unix, windows))]
fn clear_close_pane_output_after_refresh_if_target_changed(
    target_changed: bool,
    close_pane_output_after_refresh: &mut bool,
) {
    if target_changed {
        *close_pane_output_after_refresh = false;
    }
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
async fn emit_live_render_frame(
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    frame: &PaneRenderDeltaFrame,
    replaceable: bool,
) -> io::Result<()> {
    if let Some(cursor_style) = frame.cursor_style() {
        if let Some(sequence) = current_target
            .outer_terminal
            .render_cursor_style_transition(Some(current_target.cursor_style), cursor_style)
        {
            emit_attach_bytes(stream, sequence.as_bytes()).await?;
        }
        current_target.cursor_style = cursor_style;
    }
    if replaceable {
        emit_coalescible_render_frame(
            stream,
            &current_target.outer_terminal,
            frame.frame(),
            current_target.render_stream,
        )
        .await
    } else {
        emit_render_frame(stream, &current_target.outer_terminal, frame.frame()).await
    }
}

#[cfg(any(unix, windows))]
async fn try_forward_plain_output(
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    bytes: &[u8],
) -> io::Result<bool> {
    if bytes.is_empty() {
        return Ok(false);
    }

    if current_target
        .live_pane
        .as_ref()
        .is_some_and(|pane| pane.can_forward_plain_bytes(bytes))
    {
        let snapshot_synced = current_target
            .live_pane
            .as_mut()
            .is_some_and(|pane| pane.apply_forwarded_plain_bytes(bytes));
        if !snapshot_synced {
            return Ok(false);
        }

        emit_attach_bytes(stream, bytes).await?;
        return Ok(true);
    }

    if let Some(frame) = current_target
        .live_pane
        .as_mut()
        .and_then(|pane| pane.positioned_plain_output_frame(bytes))
    {
        emit_attach_bytes(stream, &frame).await?;
        return Ok(true);
    }

    Ok(false)
}

#[cfg(any(unix, windows))]
async fn schedule_attach_render_refresh(
    pane_refresh: &mut AttachRefreshScheduler,
    pane_refresh_requires_full: &mut bool,
    live_input: &LiveAttachInputContext,
) {
    live_input
        .handler
        .clear_attached_render_refresh_pending(live_input.attach_pid)
        .await;
    *pane_refresh_requires_full = true;
    pane_refresh.note_interactive_output();
    pane_refresh.schedule_now();
}

#[cfg(any(unix, windows))]
fn collect_pending_attach_output_batch(
    current_target: &mut types::OpenAttachTarget,
) -> Option<AttachOutputBatch> {
    let pane_output = current_target.pane_output.as_mut()?;
    let first = pane_output.try_recv()?;
    Some(collect_attach_output_batch(first, Some(pane_output)))
}

#[cfg(any(unix, windows))]
async fn refresh_current_attach_client(live_input: &LiveAttachInputContext) {
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

#[cfg(any(unix, windows))]
async fn sync_pending_escape_flush(
    pending_escape_flush: &mut PendingEscapeFlush,
    live_input: &LiveAttachInputContext,
    pending_input: &[u8],
) {
    if !is_pending_escape(pending_input) {
        pending_escape_flush.clear();
        return;
    }
    let escape_time = live_input.handler.attached_escape_time().await;
    pending_escape_flush.sync(pending_input, escape_time);
}

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
async fn process_attach_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    current_target: &mut types::OpenAttachTarget,
    pending_input: &mut Vec<u8>,
    locked: &mut bool,
    pane_refresh: &mut AttachRefreshScheduler,
    pending_escape_flush: &mut PendingEscapeFlush,
    last_client_input_at: &mut Option<Instant>,
) -> io::Result<()> {
    if process_socket_messages(
        decoder,
        stream,
        live_input,
        Some(current_target),
        pending_input,
        locked,
    )
    .await?
    {
        mark_attach_interactive_input(pane_refresh, last_client_input_at);
        if pane_refresh.is_pending() {
            pane_refresh.schedule_immediate();
        }
    }
    sync_pending_escape_flush(pending_escape_flush, live_input, pending_input).await;
    Ok(())
}

#[cfg(any(unix, windows))]
fn mark_attach_interactive_input(
    pane_refresh: &mut AttachRefreshScheduler,
    last_client_input_at: &mut Option<Instant>,
) {
    *last_client_input_at = Some(Instant::now());
    pane_refresh.note_interactive_output();
}

#[cfg(any(unix, windows))]
async fn process_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    mut current_target: Option<&mut types::OpenAttachTarget>,
    pending_input: &mut Vec<u8>,
    locked: &mut bool,
) -> io::Result<bool> {
    let mut saw_client_activity = false;
    let mut data_scratch = [0_u8; ATTACH_INPUT_STACK_PAYLOAD];
    loop {
        while let Some(bytes) = decoder
            .next_data_payload_into(&mut data_scratch)
            .map_err(invalid_attach_message)?
        {
            saw_client_activity = true;
            process_attach_data_payload(
                live_input,
                stream,
                current_target.as_deref_mut(),
                pending_input,
                locked,
                bytes,
            )
            .await?;
        }

        let Some(message) = decoder.next_message().map_err(invalid_attach_message)? else {
            break;
        };
        match message {
            AttachMessage::Data(bytes) => {
                saw_client_activity = true;
                process_attach_data_payload(
                    live_input,
                    stream,
                    current_target.as_deref_mut(),
                    pending_input,
                    locked,
                    &bytes,
                )
                .await?;
            }
            AttachMessage::Keystroke(keystroke) => {
                saw_client_activity = true;
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
            AttachMessage::Render(_)
            | AttachMessage::Lock(_)
            | AttachMessage::LockShellCommand(_) => {
                return Err(io::Error::other(
                    "received unexpected server-to-client message from attach client",
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

    Ok(saw_client_activity)
}

#[cfg(any(unix, windows))]
async fn process_attach_data_payload(
    live_input: &LiveAttachInputContext,
    stream: &AttachTransport,
    current_target: Option<&mut types::OpenAttachTarget>,
    pending_input: &mut Vec<u8>,
    locked: &mut bool,
    bytes: &[u8],
) -> io::Result<()> {
    if *locked {
        pending_input.clear();
        return Ok(());
    }
    #[cfg(windows)]
    {
        let _ = stream;
        let _ = current_target;
    }
    #[cfg(unix)]
    if let Some(current_target) = current_target {
        if let Some(master) = current_target.pane_master.as_ref() {
            if let Some(forwarded) = live_input
                .handler
                .try_forward_plain_attached_bytes_to_current_pane_fast(
                    live_input.attach_pid,
                    pending_input,
                    bytes,
                    &current_target.input_target,
                    master,
                )
                .await?
            {
                if forwarded {
                    let echo_enabled = master.local_echo_enabled().unwrap_or(false);
                    maybe_emit_predicted_local_echo(stream, current_target, echo_enabled, bytes)
                        .await?;
                    return Ok(());
                }
            }
        }
    }
    live_input
        .handler
        .handle_attached_live_input(live_input.attach_pid, pending_input, bytes)
        .await
}

#[cfg(unix)]
async fn maybe_emit_predicted_local_echo(
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    echo_enabled: bool,
    bytes: &[u8],
) -> io::Result<()> {
    let prefix_len = predictable_local_echo_prefix_len(bytes);
    expire_stale_predicted_echo(current_target);
    if prefix_len == 0 || !current_target.predicted_echo.is_empty() || !echo_enabled {
        return Ok(());
    }
    let bytes = &bytes[..prefix_len];

    let Some(frame) = current_target.live_pane.as_ref().and_then(|pane| {
        if pane.can_forward_plain_bytes(bytes) {
            Some(bytes.to_vec())
        } else {
            pane.positioned_plain_echo_frame(bytes)
        }
    }) else {
        return Ok(());
    };

    emit_attach_bytes(stream, &frame).await?;
    current_target.predicted_echo.extend(bytes.iter().copied());
    current_target.predicted_echo_started_at = Some(Instant::now());
    Ok(())
}

#[cfg(all(test, unix))]
fn is_predictable_local_echo(bytes: &[u8]) -> bool {
    predictable_local_echo_prefix_len(bytes) == bytes.len()
}

#[cfg(unix)]
fn predictable_local_echo_prefix_len(bytes: &[u8]) -> usize {
    let printable_prefix = bytes
        .iter()
        .take(MAX_PREDICTED_LOCAL_ECHO_BYTES)
        .take_while(|byte| matches!(**byte, b' '..=b'~'))
        .count();
    if printable_prefix == 0 {
        return 0;
    }
    if printable_prefix == bytes.len() {
        return printable_prefix;
    }
    if matches!(bytes.get(printable_prefix), Some(b'\r' | b'\n')) {
        return printable_prefix;
    }
    0
}

#[cfg(unix)]
fn consume_predicted_echo(
    current_target: &mut types::OpenAttachTarget,
    bytes: &[u8],
) -> PredictedEcho {
    expire_stale_predicted_echo(current_target);
    if current_target.predicted_echo.is_empty() || bytes.is_empty() {
        return PredictedEcho::NoPrediction;
    }
    if current_target.predicted_echo.len() < bytes.len() {
        clear_predicted_echo(current_target);
        return PredictedEcho::Mismatch;
    }
    if !current_target
        .predicted_echo
        .iter()
        .take(bytes.len())
        .copied()
        .eq(bytes.iter().copied())
    {
        clear_predicted_echo(current_target);
        return PredictedEcho::Mismatch;
    }

    current_target.predicted_echo.drain(..bytes.len());
    if current_target.predicted_echo.is_empty() {
        current_target.predicted_echo_started_at = None;
    }
    if let Some(pane) = current_target.live_pane.as_mut() {
        let _ = pane.apply_forwarded_plain_bytes(bytes);
    }
    PredictedEcho::Consumed
}

#[cfg(unix)]
fn expire_stale_predicted_echo(current_target: &mut types::OpenAttachTarget) {
    if current_target
        .predicted_echo_started_at
        .is_some_and(|started_at| {
            Instant::now().saturating_duration_since(started_at) >= PREDICTED_LOCAL_ECHO_TIMEOUT
        })
    {
        clear_predicted_echo(current_target);
    }
}

#[cfg(unix)]
fn clear_predicted_echo(current_target: &mut types::OpenAttachTarget) {
    current_target.predicted_echo.clear();
    current_target.predicted_echo_started_at = None;
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PredictedEcho {
    NoPrediction,
    Consumed,
    Mismatch,
}

#[cfg(any(unix, windows))]
fn should_treat_attach_output_as_interactive(last_client_input_at: Option<Instant>) -> bool {
    last_client_input_at.is_some_and(|input_at| {
        Instant::now().saturating_duration_since(input_at) <= ATTACH_INTERACTIVE_OUTPUT_WINDOW
    })
}

#[cfg(all(test, unix))]
mod tests;
