#[cfg(any(unix, windows))]
use rmux_core::TerminalPassthrough;
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
// Bound each opportunistic socket drain so sustained attach input cannot keep
// this task away from render, control, shutdown, or escape-flush futures.
const MAX_IMMEDIATE_ATTACH_READS: usize = 8;
#[cfg(windows)]
const ATTACH_EXIT_OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
#[cfg(any(unix, windows))]
const ATTACH_INPUT_STACK_PAYLOAD: usize = 1024;
#[cfg(all(unix, test))]
const MAX_PREDICTED_LOCAL_ECHO_BYTES: usize = 16;
#[cfg(unix)]
const PREDICTED_LOCAL_ECHO_TIMEOUT: Duration = Duration::from_millis(250);

#[cfg(test)]
#[derive(Debug, Default)]
struct LiveAttachInputApplyPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static LIVE_ATTACH_INPUT_APPLY_PAUSE: std::sync::Mutex<
    Option<(
        crate::handler::attach_support::ActiveAttachIdentity,
        Arc<LiveAttachInputApplyPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
static LIVE_ATTACH_INPUT_VALIDATION_PAUSE: std::sync::Mutex<
    Option<(
        crate::handler::attach_support::ActiveAttachIdentity,
        Arc<LiveAttachInputApplyPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
fn install_live_attach_input_apply_pause(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) -> Arc<LiveAttachInputApplyPause> {
    let pause = Arc::new(LiveAttachInputApplyPause::default());
    *LIVE_ATTACH_INPUT_APPLY_PAUSE
        .lock()
        .expect("live attach input pause lock") = Some((identity, Arc::clone(&pause)));
    pause
}

#[cfg(test)]
fn install_live_attach_input_validation_pause(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) -> Arc<LiveAttachInputApplyPause> {
    let pause = Arc::new(LiveAttachInputApplyPause::default());
    *LIVE_ATTACH_INPUT_VALIDATION_PAUSE
        .lock()
        .expect("live attach input validation pause lock") = Some((identity, Arc::clone(&pause)));
    pause
}

#[cfg(test)]
async fn pause_before_live_attach_input_validation(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) {
    let pause = {
        let mut installed = LIVE_ATTACH_INPUT_VALIDATION_PAUSE
            .lock()
            .expect("live attach input validation pause lock");
        installed
            .as_ref()
            .is_some_and(|(expected, _)| *expected == identity)
            .then(|| {
                installed
                    .take()
                    .expect("matching validation pause remains installed")
                    .1
            })
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(test)]
async fn pause_after_live_attach_input_validation(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) {
    let pause = {
        let mut installed = LIVE_ATTACH_INPUT_APPLY_PAUSE
            .lock()
            .expect("live attach input pause lock");
        installed
            .as_ref()
            .is_some_and(|(expected, _)| *expected == identity)
            .then(|| {
                installed
                    .take()
                    .expect("matching pause remains installed")
                    .1
            })
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}
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
use attach_output_batch::{
    collect_attach_output_batch, collect_attach_output_batch_metadata, AttachOutputBatch,
};
#[cfg(all(any(unix, windows), feature = "web"))]
pub(crate) use attach_transport::in_process_attach_pair;
use attach_transport::{AttachTransport, TryAttachRead};
#[cfg(any(unix, windows))]
use control::{
    apply_pending_attach_controls, coalesce_render_switches, preserves_live_output,
    recv_attach_control, redraw_after_persistent_overlay_state_advance, should_emit_overlay,
    switch_attach_target, take_pending_live_passthroughs, PendingAttachAction, PendingAttachExit,
    PendingAttachInputState,
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
use pending_escape::PendingEscapeFlush;
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
#[cfg(windows)]
pub(crate) use reader::PaneOutputEofState;
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
    pane_output_channel, AttachControl, AttachSessionUpgrade, AttachTarget, HandleOutcome,
    OverlayFrame, PaneAlertCallback, PaneAlertEvent, PaneExitCallback, PaneExitEvent,
    PaneOutputReceiver, PaneOutputSender,
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
    let mut active_emit_cache = None;
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
            // Socket reads and attach-control refreshes can both remain
            // continuously ready. Service an expired input ambiguity before
            // either queue so its deadline is a real upper bound rather than
            // merely another selectable wakeup.
            flush_due_pending_escape_input(
                &mut pending_escape_flush,
                &live_input,
                &mut pending_input,
                locked,
            )
            .await?;
            synchronize_persistent_overlay_epoch(
                &persistent_overlay_epoch,
                &stream,
                &current_target,
                attach_controls.as_mut(),
                &mut deferred_controls,
                &control_backlog,
                &mut persistent_overlay,
                &mut persistent_overlay_visible,
                &mut persistent_overlay_state_id,
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
                Some(PendingAttachInputState::new(
                    &mut pending_input,
                    &mut pending_escape_flush,
                )),
            )
            .await?
            {
                PendingAttachAction::Exit(PendingAttachExit { reason, .. }) => {
                    finish_pending_attach_exit(
                        reason,
                        &stream,
                        &mut current_target,
                        &mut deferred_passthroughs,
                    )
                    .await?;
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
            for _ in 0..MAX_IMMEDIATE_ATTACH_READS {
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
                &closing,
                &mut current_target,
                &mut pending_input,
                &mut active_emit_cache,
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
                Some(PendingAttachInputState::new(
                    &mut pending_input,
                    &mut pending_escape_flush,
                )),
            )
            .await?
            {
                PendingAttachAction::Exit(PendingAttachExit { reason, .. }) => {
                    finish_pending_attach_exit(
                        reason,
                        &stream,
                        &mut current_target,
                        &mut deferred_passthroughs,
                    )
                    .await?;
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
            let pending_shutdown_requested = live_input.handler.request_shutdown_if_pending();

            tokio::select! {
                result = shutdown.changed() => {
                    let _ = result;
                    if closing.load(Ordering::SeqCst) {
                        loop {
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
                                Some(PendingAttachInputState::new(
                                    &mut pending_input,
                                    &mut pending_escape_flush,
                                )),
                            )
                            .await?
                            {
                                PendingAttachAction::Exit(PendingAttachExit { reason, .. }) => {
                                    finish_pending_attach_exit(
                                        reason,
                                        &stream,
                                        &mut current_target,
                                        &mut deferred_passthroughs,
                                    )
                                    .await?;
                                    log_attach_exit(&live_input, &current_target, reason);
                                    return Ok(());
                                }
                                PendingAttachAction::Continue { .. }
                                | PendingAttachAction::InteractiveInput
                                | PendingAttachAction::Refresh { .. } => continue,
                                PendingAttachAction::Write => break,
                            }
                        }
                    }
                    let reason = if pending_shutdown_requested {
                        AttachExitReason::PendingServerShutdown
                    } else {
                        AttachExitReason::ServerShutdown
                    };
                    log_attach_exit(
                        &live_input,
                        &current_target,
                        reason,
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
                        &closing,
                        &mut current_target,
                        &mut pending_input,
                        &mut active_emit_cache,
                        &mut locked,
                        &mut pane_refresh,
                        &mut pending_escape_flush,
                        &mut last_client_input_at,
                    )
                    .await?;
                }
                _ = wait_for_refresh_deadline(pane_refresh.deadline()) => {
                    pane_refresh.clear();
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
                        Some(PendingAttachInputState::new(
                            &mut pending_input,
                            &mut pending_escape_flush,
                        )),
                    )
                    .await?
                    {
                        PendingAttachAction::Exit(PendingAttachExit { reason, .. }) => {
                            finish_pending_attach_exit(
                                reason,
                                &stream,
                                &mut current_target,
                                &mut deferred_passthroughs,
                            )
                            .await?;
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
                                    collect_pending_attach_output_batch_metadata(&mut current_target);
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
                                            ..
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
                        Some(PendingAttachInputState::new(
                            &mut pending_input,
                            &mut pending_escape_flush,
                        )),
                    )
                    .await?
                    {
                        PendingAttachAction::Exit(PendingAttachExit { reason, .. }) => {
                            finish_pending_attach_exit(
                                reason,
                                &stream,
                                &mut current_target,
                                &mut deferred_passthroughs,
                            )
                            .await?;
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
                    if closing.load(Ordering::SeqCst) {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachClosingFlag,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
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
                        .refresh_attached_client_status(live_input.attach_pid(), &session_name)
                        .await;
                    reschedule_status_refresh_for_session(
                        &mut status_refresh,
                        &live_input,
                        &session_name,
                    )
                        .await;
                }
                _ = wait_for_refresh_deadline(pending_escape_flush.deadline()) => {
                    flush_due_pending_escape_input(
                        &mut pending_escape_flush,
                        &live_input,
                        &mut pending_input,
                        locked,
                    )
                    .await?;
                }
                control = recv_attach_control(&mut deferred_controls, attach_controls.as_mut(), &control_backlog) => {
                    // Dismissal publishes its persistent-overlay epoch before
                    // the fresh base frame is ready. This select arm may have
                    // already received a stale tree control while that epoch
                    // advanced, so fence it again immediately before dispatch.
                    synchronize_persistent_overlay_epoch(
                        &persistent_overlay_epoch,
                        &stream,
                        &current_target,
                        attach_controls.as_mut(),
                        &mut deferred_controls,
                        &control_backlog,
                        &mut persistent_overlay,
                        &mut persistent_overlay_visible,
                        &mut persistent_overlay_state_id,
                    )
                    .await?;
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
                            finish_pending_attach_exit(
                                AttachExitReason::AttachControlExited,
                                &stream,
                                &mut current_target,
                                &mut deferred_passthroughs,
                            )
                            .await?;
                            log_attach_exit(
                                &live_input,
                                &current_target,
                                AttachExitReason::AttachControlExited,
                            );
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
                            let (next_target, switch_count) = coalesce_render_switches(
                                next_target,
                                &mut deferred_controls,
                                attach_controls.as_mut(),
                                &control_backlog,
                            );
                            let drop_live_output =
                                !preserves_live_output(&current_target, &next_target);
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
                                render_generation = render_generation.saturating_add(switch_count);
                                continue;
                            }
                            close_pane_output_after_refresh = false;
                            render_generation = render_generation.saturating_add(switch_count);
                            PendingAttachInputState::new(
                                &mut pending_input,
                                &mut pending_escape_flush,
                            )
                            .clear_if_pane_source_changed(
                                &current_target,
                                next_target.as_ref(),
                            );
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
                        Some(AttachControl::ClipboardWrite { bytes, reservation }) => {
                            emit_attach_bytes(&stream, &bytes).await?;
                            drop(reservation);
                        }
                        Some(AttachControl::LockShellCommand(command)) => {
                            PendingAttachInputState::new(
                                &mut pending_input,
                                &mut pending_escape_flush,
                            )
                            .clear();
                            locked = true;
                            emit_attach_stop(&stream, &current_target).await?;
                            emit_attach_message(&stream, &AttachMessage::LockShellCommand(command))
                                .await?;
                        }
                        Some(AttachControl::Suspend) => {
                            PendingAttachInputState::new(
                                &mut pending_input,
                                &mut pending_escape_flush,
                            )
                            .clear();
                            locked = true;
                            emit_attach_stop(&stream, &current_target).await?;
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
                        && !closing.load(Ordering::SeqCst)
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
                    let pending_output_batch =
                        match collect_attach_output_batch(item, current_target.pane_output.as_mut()) {
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
                        batch @ AttachOutputBatch::Events { .. } => batch,
                        };
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
                        Some(PendingAttachInputState::new(
                            &mut pending_input,
                            &mut pending_escape_flush,
                        )),
                    )
                    .await?
                    {
                        PendingAttachAction::Exit(PendingAttachExit {
                            reason,
                            drop_pending_output,
                            snapshot_covered_output_before_sequence,
                        }) => {
                            // The receiver cursor already owns this batch. Hand it to the
                            // terminal exit drain unless an earlier control deliberately
                            // invalidated output from the old attach target.
                            finish_pending_attach_exit_with_batch(
                                reason,
                                &stream,
                                &mut current_target,
                                &mut deferred_passthroughs,
                                pending_attach_exit_output_batch(
                                    drop_pending_output,
                                    snapshot_covered_output_before_sequence,
                                    pending_output_batch,
                                ),
                            )
                            .await?;
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
                            let AttachOutputBatch::Events {
                                passthroughs,
                                close_after_render,
                                ..
                            } = pending_output_batch
                            else {
                                unreachable!("closed and gap batches return before control dispatch")
                            };
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
                            let AttachOutputBatch::Events {
                                bytes: raw_output_bytes,
                                passthroughs,
                                close_after_render,
                                sustained: sustained_output,
                                ..
                            } = pending_output_batch
                            else {
                                unreachable!("closed and gap batches return before control dispatch")
                            };
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
                                    match current_target
                                        .live_pane
                                        .as_mut()
                                        .map(|pane| pane.render_interactive_frame_from_transcript())
                                    {
                                        Some(PaneRenderDelta::Incremental(delta)) => {
                                            emit_live_render_frame(
                                                &stream,
                                                &mut current_target,
                                                &delta,
                                                false,
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
        record_attach_error(live_input.attach_pid(), &current_target.session_name, error);
        let _ = emit_attach_stop(&stream, &current_target).await;
    }

    result
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(unix, windows))]
async fn synchronize_persistent_overlay_epoch(
    persistent_overlay_epoch: &AtomicU64,
    stream: &AttachTransport,
    current_target: &types::OpenAttachTarget,
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    deferred_controls: &mut VecDeque<AttachControl>,
    control_backlog: &AtomicUsize,
    persistent_overlay: &mut Option<Vec<u8>>,
    persistent_overlay_visible: &mut bool,
    persistent_overlay_state_id: &mut Option<u64>,
) -> io::Result<()> {
    let overlay_barrier = persistent_overlay_epoch.load(Ordering::SeqCst);
    let previous_overlay_state_id = *persistent_overlay_state_id;
    advance_persistent_overlay_state(
        persistent_overlay_state_id,
        attach_controls,
        deferred_controls,
        overlay_barrier,
        control_backlog,
    );
    redraw_after_persistent_overlay_state_advance(
        stream,
        current_target,
        persistent_overlay,
        persistent_overlay_visible,
        previous_overlay_state_id,
        *persistent_overlay_state_id,
        persistent_overlay_replacement_pending(deferred_controls, *persistent_overlay_state_id),
    )
    .await
}

#[cfg(any(unix, windows))]
fn log_attach_exit(
    live_input: &LiveAttachInputContext,
    current_target: &types::OpenAttachTarget,
    reason: AttachExitReason,
) {
    record_attach_exit(
        live_input.attach_pid(),
        &current_target.session_name,
        reason,
    );
}

#[cfg(any(unix, windows))]
async fn finish_pending_attach_exit(
    reason: AttachExitReason,
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    deferred_passthroughs: &mut Vec<TerminalPassthrough>,
) -> io::Result<()> {
    finish_pending_attach_exit_with_batch(
        reason,
        stream,
        current_target,
        deferred_passthroughs,
        None,
    )
    .await
}

#[cfg(any(unix, windows))]
fn pending_attach_exit_output_batch(
    drop_pending_output: bool,
    snapshot_covered_output_before_sequence: Option<u64>,
    batch: AttachOutputBatch,
) -> Option<AttachOutputBatch> {
    if drop_pending_output {
        return None;
    }
    Some(match snapshot_covered_output_before_sequence {
        Some(before_sequence) => batch.covered_by_render_snapshot(before_sequence),
        None => batch,
    })
}

#[cfg(any(unix, windows))]
async fn finish_pending_attach_exit_with_batch(
    reason: AttachExitReason,
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    deferred_passthroughs: &mut Vec<TerminalPassthrough>,
    pending_batch: Option<AttachOutputBatch>,
) -> io::Result<()> {
    if reason != AttachExitReason::AttachControlExited {
        return Ok(());
    }

    let mut output_bytes = Vec::new();
    let mut passthroughs = Vec::new();
    let mut saw_gap = false;
    let output_closed = pending_batch.is_some_and(|batch| {
        collect_final_attach_output_batch(batch, &mut output_bytes, &mut passthroughs, &mut saw_gap)
    });
    let pane_output = if output_closed {
        None
    } else {
        current_target.pane_output.take()
    };
    if let Some(mut pane_output) = pane_output {
        #[cfg(not(windows))]
        while let Some(item) = pane_output.try_recv() {
            if collect_final_attach_output_item(
                item,
                &mut pane_output,
                &mut output_bytes,
                &mut passthroughs,
                &mut saw_gap,
            ) {
                break;
            }
        }
        #[cfg(windows)]
        {
            let deadline = Instant::now() + ATTACH_EXIT_OUTPUT_DRAIN_TIMEOUT;
            loop {
                let item = match pane_output.try_recv() {
                    Some(item) => item,
                    None => match tokio::time::timeout_at(deadline, pane_output.recv()).await {
                        Ok(item) => item,
                        Err(_) => break,
                    },
                };
                if collect_final_attach_output_item(
                    item,
                    &mut pane_output,
                    &mut output_bytes,
                    &mut passthroughs,
                    &mut saw_gap,
                ) {
                    break;
                }
            }
        }
    }

    let output_forwarded = !output_bytes.is_empty()
        && try_forward_plain_output(stream, current_target, &output_bytes).await?;
    let require_final_render = saw_gap || (!output_bytes.is_empty() && !output_forwarded);
    if require_final_render {
        match current_target
            .live_pane
            .as_mut()
            .map(|pane| pane.render_frame_from_transcript(true))
        {
            Some(PaneRenderDelta::Incremental(frame)) => {
                emit_live_render_frame(stream, current_target, &frame, false).await?;
            }
            Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                emit_attach_bytes(stream, &output_bytes).await?;
            }
        }
    }

    let passthrough_frame = take_passthrough_frame_with_live_passthroughs(
        current_target,
        deferred_passthroughs,
        passthroughs,
    );
    emit_attach_bytes(stream, &passthrough_frame).await?;
    emit_exited_attach_stop(stream, current_target).await
}

#[cfg(any(unix, windows))]
fn collect_final_attach_output_item(
    item: rmux_core::events::OutputCursorItem,
    pane_output: &mut types::PaneOutputReceiver,
    output_bytes: &mut Vec<u8>,
    passthroughs: &mut Vec<TerminalPassthrough>,
    saw_gap: &mut bool,
) -> bool {
    let batch = collect_attach_output_batch(item, Some(pane_output));
    collect_final_attach_output_batch(batch, output_bytes, passthroughs, saw_gap)
}

#[cfg(any(unix, windows))]
fn collect_final_attach_output_batch(
    batch: AttachOutputBatch,
    output_bytes: &mut Vec<u8>,
    passthroughs: &mut Vec<TerminalPassthrough>,
    saw_gap: &mut bool,
) -> bool {
    match batch {
        AttachOutputBatch::Closed => true,
        AttachOutputBatch::Gap => {
            *saw_gap = true;
            false
        }
        AttachOutputBatch::Events {
            bytes,
            passthroughs: batch_passthroughs,
            close_after_render,
            sustained: _,
            ..
        } => {
            output_bytes.extend_from_slice(&bytes);
            passthroughs.extend(batch_passthroughs);
            close_after_render
        }
    }
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
        .clear_attached_render_refresh_pending(live_input.attach_pid())
        .await;
    *pane_refresh_requires_full = true;
    pane_refresh.note_interactive_output();
    pane_refresh.schedule_now();
}

#[cfg(any(unix, windows))]
fn collect_pending_attach_output_batch_metadata(
    current_target: &mut types::OpenAttachTarget,
) -> Option<AttachOutputBatch> {
    let pane_output = current_target.pane_output.as_mut()?;
    let first = pane_output.try_recv()?;
    Some(collect_attach_output_batch_metadata(
        first,
        Some(pane_output),
    ))
}

#[cfg(any(unix, windows))]
async fn refresh_current_attach_client(live_input: &LiveAttachInputContext) {
    if let Ok(session_name) = live_input
        .handler
        .attached_session_name(live_input.attach_pid())
        .await
    {
        live_input
            .handler
            .refresh_attached_client(live_input.attach_pid(), &session_name)
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
    if pending_input.is_empty() {
        pending_escape_flush.clear();
        return;
    }
    let escape_time = live_input.handler.attached_escape_time().await;
    sync_pending_escape_flush_with_escape_time(pending_escape_flush, pending_input, escape_time);
}

#[cfg(any(unix, windows))]
async fn flush_due_pending_escape_input(
    pending_escape_flush: &mut PendingEscapeFlush,
    live_input: &LiveAttachInputContext,
    pending_input: &mut Vec<u8>,
    locked: bool,
) -> io::Result<()> {
    if !pending_escape_deadline_due(pending_escape_flush) {
        return Ok(());
    }

    #[cfg(test)]
    let skip_identity_validation = !live_input.validate_identity;
    #[cfg(not(test))]
    let skip_identity_validation = false;
    if !skip_identity_validation
        && !live_input
            .handler
            .current_live_attach_input(live_input.identity)
            .await
    {
        pending_escape_flush.clear();
        pending_input.clear();
        return Err(io::Error::other(
            "stale attach forwarder retained pending input",
        ));
    }

    pending_escape_flush.clear();
    if locked {
        pending_input.clear();
        return Ok(());
    }

    live_input
        .handler
        .flush_attached_pending_escape_input_for_identity(live_input.identity, pending_input)
        .await?;
    // Rerouting flushed remainder bytes can retain a fresh ambiguous prefix
    // (for example, a second ESC ] inside the body). Re-arm it immediately
    // rather than waiting for another client read.
    sync_pending_escape_flush(pending_escape_flush, live_input, pending_input).await;
    Ok(())
}

#[cfg(any(unix, windows))]
fn pending_escape_deadline_due(pending_escape_flush: &PendingEscapeFlush) -> bool {
    pending_escape_flush
        .deadline()
        .is_some_and(|deadline| deadline <= Instant::now())
}

#[cfg(any(unix, windows))]
fn sync_pending_escape_flush_with_escape_time(
    pending_escape_flush: &mut PendingEscapeFlush,
    pending_input: &[u8],
    escape_time: Duration,
) {
    // `PendingEscapeFlush` owns the retained-input grammar. Keeping the
    // classifier out of this wrapper prevents the decoder/timer split-brain
    // that previously dropped APC and numeric CSI deadlines here.
    pending_escape_flush.sync(pending_input, escape_time);
}

#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
async fn process_attach_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    closing: &AtomicBool,
    current_target: &mut types::OpenAttachTarget,
    pending_input: &mut Vec<u8>,
    active_emit_cache: &mut Option<(u64, rmux_proto::WindowTarget)>,
    locked: &mut bool,
    pane_refresh: &mut AttachRefreshScheduler,
    pending_escape_flush: &mut PendingEscapeFlush,
    last_client_input_at: &mut Option<Instant>,
) -> io::Result<()> {
    let forwarded_to_pane = match process_socket_messages(
        decoder,
        stream,
        live_input,
        Some(current_target),
        PendingAttachInputState::new(pending_input, pending_escape_flush),
        active_emit_cache,
        locked,
    )
    .await
    {
        Ok(forwarded_to_pane) => forwarded_to_pane,
        Err(_) if closing.load(Ordering::SeqCst) => {
            // A terminal attach control is queued before `closing` is
            // published. Input may already be between the queue poll and its
            // identity check when close removes the registration. Discard that
            // now-stale input and let the next loop iteration consume the
            // terminal control, which owns the finite output drain.
            PendingAttachInputState::new(pending_input, pending_escape_flush).clear();
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if forwarded_to_pane {
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
enum DeferredAttachInputOutput {
    Frame(AttachMessage),
    Unlock {
        start_sequence: Vec<u8>,
        outer_terminal: Box<crate::outer_terminal::OuterTerminal>,
        render_frame: Vec<u8>,
    },
}

#[cfg(any(unix, windows))]
async fn process_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    mut current_target: Option<&mut types::OpenAttachTarget>,
    mut pending_input: PendingAttachInputState<'_>,
    active_emit_cache: &mut Option<(u64, rmux_proto::WindowTarget)>,
    locked: &mut bool,
) -> io::Result<bool> {
    // The transport read and frame accumulation have already completed. Each
    // mutation below carries the immutable registration identity and validates
    // it at its own atomic snapshot point; no guard spans PTY or command awaits.
    #[cfg(test)]
    pause_before_live_attach_input_validation(live_input.identity).await;
    #[cfg(test)]
    let skip_identity_validation = !live_input.validate_identity;
    #[cfg(not(test))]
    let skip_identity_validation = false;
    if !skip_identity_validation
        && !live_input
            .handler
            .current_live_attach_input(live_input.identity)
            .await
    {
        let (pending_input, pending_escape_flush) = pending_input.parts_mut();
        pending_input.clear();
        pending_escape_flush.clear();
        return Err(io::Error::other("stale attach forwarder input"));
    }
    #[cfg(test)]
    pause_after_live_attach_input_validation(live_input.identity).await;
    let (pending_input, pending_escape_flush) = pending_input.parts_mut();
    let mut forwarded_to_pane = false;
    let mut deferred_outputs = Vec::new();
    let mut data_scratch = [0_u8; ATTACH_INPUT_STACK_PAYLOAD];
    'messages: loop {
        loop {
            if pending_escape_deadline_due(pending_escape_flush) {
                break 'messages;
            }
            let Some(bytes) = decoder
                .next_data_payload_into(&mut data_scratch)
                .map_err(invalid_attach_message)?
            else {
                break;
            };
            let retained_before = pending_input.len();
            forwarded_to_pane |= process_attach_data_payload(
                live_input,
                stream,
                current_target.as_deref_mut(),
                pending_input,
                active_emit_cache,
                locked,
                bytes,
            )
            .await?;
            pending_escape_flush.observe_input_dispatch(
                retained_before,
                bytes.len(),
                pending_input,
            );
        }

        if pending_escape_deadline_due(pending_escape_flush) {
            break;
        }
        let Some(message) = decoder.next_message().map_err(invalid_attach_message)? else {
            break;
        };
        match message {
            AttachMessage::Data(bytes) => {
                let retained_before = pending_input.len();
                forwarded_to_pane |= process_attach_data_payload(
                    live_input,
                    stream,
                    current_target.as_deref_mut(),
                    pending_input,
                    active_emit_cache,
                    locked,
                    &bytes,
                )
                .await?;
                pending_escape_flush.observe_input_dispatch(
                    retained_before,
                    bytes.len(),
                    pending_input,
                );
            }
            AttachMessage::Keystroke(keystroke) => {
                let retained_before = pending_input.len();
                let appended = keystroke.bytes().len();
                let keystroke_forwarded_to_pane = if *locked {
                    pending_input.clear();
                    false
                } else {
                    live_input
                        .handler
                        .handle_attached_keystroke_input_with_active_cache_for_identity(
                            live_input.identity,
                            pending_input,
                            &keystroke,
                            active_emit_cache,
                        )
                        .await?
                };
                pending_escape_flush.observe_input_dispatch(
                    retained_before,
                    appended,
                    pending_input,
                );
                forwarded_to_pane |= keystroke_forwarded_to_pane;
                let response = live_input
                    .handler
                    .handle_attached_keystroke_for_identity(
                        live_input.identity,
                        &keystroke,
                        !keystroke_forwarded_to_pane,
                    )
                    .await
                    .map_err(io::Error::other)?;
                deferred_outputs.push(DeferredAttachInputOutput::Frame(
                    AttachMessage::KeyDispatched(response),
                ));
            }
            AttachMessage::Resize(size) => {
                live_input
                    .handler
                    .handle_attached_resize_for_identity(live_input.identity, size)
                    .await
                    .map_err(io::Error::other)?;
            }
            AttachMessage::ResizeGeometry(geometry) => {
                live_input
                    .handler
                    .handle_attached_resize_geometry_for_identity(live_input.identity, geometry)
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
                if !live_input
                    .handler
                    .handle_attached_unlock_for_identity(live_input.identity)
                    .await
                {
                    pending_input.clear();
                    pending_escape_flush.clear();
                    return Err(io::Error::other("stale attach forwarder unlock"));
                }
                *locked = false;
                if let Some(current_target) = current_target.as_deref() {
                    deferred_outputs.push(DeferredAttachInputOutput::Unlock {
                        start_sequence: current_target.outer_terminal.attach_start_sequence(),
                        outer_terminal: Box::new(current_target.outer_terminal.clone()),
                        render_frame: current_target.render_frame.clone(),
                    });
                }
                let session_name = live_input
                    .handler
                    .attached_session_name_for_identity(live_input.identity)
                    .await
                    .map_err(io::Error::other)?;
                live_input
                    .handler
                    .refresh_attached_client_for_identity(
                        live_input.attach_pid(),
                        live_input.identity.attach_id(),
                        &session_name,
                        "attach unlock",
                    )
                    .await
                    .map_err(io::Error::other)?;
                // Resuming terminal ownership is an inter-frame barrier. A
                // following binding may block indefinitely, so flush the
                // start sequence and render before decoding another frame.
                break 'messages;
            }
            AttachMessage::KeyDispatched(_) => {
                return Err(io::Error::other(
                    "received unexpected key dispatch acknowledgement from attach client",
                ));
            }
        }
    }

    // Client writes can block behind transport backpressure. They are emitted
    // only after all identity-checked state mutations are complete.
    for output in deferred_outputs {
        match output {
            DeferredAttachInputOutput::Frame(message) => {
                emit_attach_frame(stream, &message).await?;
            }
            DeferredAttachInputOutput::Unlock {
                start_sequence,
                outer_terminal,
                render_frame,
            } => {
                emit_attach_bytes(stream, &start_sequence).await?;
                emit_render_frame(stream, &outer_terminal, &render_frame).await?;
            }
        }
    }

    Ok(forwarded_to_pane)
}

#[cfg(any(unix, windows))]
async fn process_attach_data_payload(
    live_input: &LiveAttachInputContext,
    stream: &AttachTransport,
    current_target: Option<&mut types::OpenAttachTarget>,
    pending_input: &mut Vec<u8>,
    active_emit_cache: &mut Option<(u64, rmux_proto::WindowTarget)>,
    locked: &mut bool,
    bytes: &[u8],
) -> io::Result<bool> {
    if *locked {
        pending_input.clear();
        return Ok(false);
    }
    let _ = (stream, current_target);
    live_input
        .handler
        .handle_attached_live_input_with_active_cache_for_identity(
            live_input.identity,
            pending_input,
            bytes,
            active_emit_cache,
        )
        .await
}

#[cfg(all(test, unix))]
fn is_predictable_local_echo(bytes: &[u8]) -> bool {
    predictable_local_echo_prefix_len(bytes) == bytes.len()
}

#[cfg(all(unix, test))]
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
