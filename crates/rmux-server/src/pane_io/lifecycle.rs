use super::*;
pub(super) fn attach_shutdown_observable(shutdown: &watch::Receiver<()>) -> bool {
    shutdown.has_changed().unwrap_or(true)
}

/// Linearize one attach mutation batch against shutdown.
///
/// Callers keep the returned Drain guard through the batch's last handler mutation and any
/// lifecycle publication it awaits. Transport reads may happen before this point, but decoded
/// input or attach controls must not be applied until admission succeeds.
pub(super) fn begin_attach_mutation_batch(
    live_input: &LiveAttachInputContext,
    shutdown: &watch::Receiver<()>,
) -> Option<crate::handler::NormalRequestGuard> {
    if attach_shutdown_observable(shutdown) {
        return None;
    }
    let guard = live_input.handler.try_begin_normal_request(true)?;
    if attach_shutdown_observable(shutdown) {
        // The listener closes normal admission before publishing transport shutdown. If the
        // watch raced the optimistic admission, reject the batch before its first mutation.
        return None;
    }
    Some(guard)
}

pub(super) fn take_pending_terminal_attach_control(
    deferred_controls: &mut VecDeque<AttachControl>,
    mut attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    control_backlog: &AtomicUsize,
) -> Option<AttachControl> {
    loop {
        let control = match deferred_controls.pop_front() {
            Some(control) => control,
            None => {
                let control_rx = attach_controls.as_mut()?;
                match try_recv_attach_control(control_rx, control_backlog) {
                    Ok(control) => control,
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => return None,
                }
            }
        };
        if control.is_terminal() {
            return Some(control);
        }
        // Shutdown has already been observed. Dropping a queued refresh, switch, overlay,
        // write, lock, or suspend releases its memory reservation without starting the
        // attach-control batch that could otherwise mutate the live forwarder state.
    }
}

pub(super) async fn finish_terminal_attach_control(
    control: AttachControl,
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    pending_output_batch: Option<AttachOutputBatch>,
) -> io::Result<AttachExitReason> {
    match control {
        AttachControl::Detach => {
            emit_detached_attach_stop(stream, current_target).await?;
            Ok(AttachExitReason::AttachControlDetach)
        }
        AttachControl::Exited => {
            finish_pending_attach_exit_with_batch(
                AttachExitReason::AttachControlExited,
                stream,
                current_target,
                deferred_passthroughs,
                pending_output_batch,
            )
            .await?;
            Ok(AttachExitReason::AttachControlExited)
        }
        AttachControl::DetachKill => {
            emit_attach_stop(stream, current_target).await?;
            emit_attach_message(stream, &AttachMessage::DetachKill).await?;
            Ok(AttachExitReason::AttachControlDetachKill)
        }
        AttachControl::DetachExecShellCommand(command) => {
            emit_attach_stop(stream, current_target).await?;
            emit_attach_message(stream, &AttachMessage::DetachExecShellCommand(command)).await?;
            Ok(AttachExitReason::AttachControlDetachExec)
        }
        AttachControl::InteractiveInput
        | AttachControl::Refresh
        | AttachControl::Switch(_)
        | AttachControl::AdvancePersistentOverlayState(_)
        | AttachControl::Overlay(_)
        | AttachControl::CoalescedPopupOverlay(_)
        | AttachControl::Write(_)
        | AttachControl::ClipboardWrite { .. }
        | AttachControl::LockShellCommand(_)
        | AttachControl::Suspend => {
            unreachable!("only terminal attach controls reach shutdown finalization")
        }
    }
}

pub(super) fn log_attach_exit(
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

pub(super) async fn finish_pending_attach_exit(
    reason: AttachExitReason,
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
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

pub(super) fn pending_attach_exit_output_batch(
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

pub(super) async fn finish_pending_attach_exit_with_batch(
    reason: AttachExitReason,
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    pending_batch: Option<AttachOutputBatch>,
) -> io::Result<()> {
    if reason != AttachExitReason::AttachControlExited {
        return Ok(());
    }
    let mut output_bytes = Vec::new();
    let mut passthrough_batches = Vec::new();
    let mut saw_gap = false;
    let output_closed = pending_batch.is_some_and(|batch| {
        collect_final_attach_output_batch(
            batch,
            &mut output_bytes,
            &mut passthrough_batches,
            &mut saw_gap,
        )
    });
    let mut pane_output = if output_closed {
        None
    } else {
        current_target.pane_output.take()
    };
    if let Some(pane_output) = pane_output.as_mut() {
        #[cfg(not(windows))]
        while let Some(item) = pane_output.try_recv_drained() {
            if collect_final_attach_output_item(
                item,
                pane_output,
                &mut output_bytes,
                &mut passthrough_batches,
                &mut saw_gap,
            ) {
                break;
            }
        }
        #[cfg(windows)]
        {
            let deadline = Instant::now() + ATTACH_EXIT_OUTPUT_DRAIN_TIMEOUT;
            loop {
                let item = match pane_output.try_recv_drained() {
                    Some(item) => item,
                    None => {
                        match tokio::time::timeout_at(deadline, pane_output.recv_drained()).await {
                            Ok(item) => item,
                            Err(_) => break,
                        }
                    }
                };
                if collect_final_attach_output_item(
                    item,
                    pane_output,
                    &mut output_bytes,
                    &mut passthrough_batches,
                    &mut saw_gap,
                ) {
                    break;
                }
            }
        }
    }
    // EOF releases an unclosed synchronized interval before publishing, so a
    // passthrough held through that interval is about to be emitted against
    // the stale frame the client last received. Draw the converged screen
    // first rather than replaying the ordering this queue exists to prevent.
    let positioned_passthrough_pending = current_target.presentation_frozen()
        && (deferred_passthroughs.holds_cursor_positioned()
            || passthrough_batches
                .iter()
                .any(|(_, passthroughs)| holds_cursor_positioned_passthrough(passthroughs)));
    let output_forwarded = !output_bytes.is_empty()
        && try_forward_plain_output(stream, current_target, &output_bytes).await?;
    let require_final_render = saw_gap
        || (!output_bytes.is_empty() && !output_forwarded)
        || positioned_passthrough_pending;
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
    let current_generation = pane_output.as_ref().map_or_else(
        || current_target.pane_output_generation(),
        |pane_output| PaneOutputGeneration::subscribed(pane_output.current_generation()),
    );
    let passthrough_frame = drain_passthrough_frame_at_exit(
        current_target,
        deferred_passthroughs,
        current_generation,
        passthrough_batches,
    );
    emit_attach_bytes(stream, &passthrough_frame).await?;
    emit_exited_attach_stop(stream, current_target).await
}

fn collect_final_attach_output_item(
    item: types::DrainedPaneOutput,
    pane_output: &mut types::PaneOutputReceiver,
    output_bytes: &mut Vec<u8>,
    passthrough_batches: &mut Vec<(PaneOutputGeneration, Vec<TerminalPassthrough>)>,
    saw_gap: &mut bool,
) -> bool {
    let batch = collect_attach_output_batch(item, Some(pane_output));
    collect_final_attach_output_batch(batch, output_bytes, passthrough_batches, saw_gap)
}

fn collect_final_attach_output_batch(
    batch: AttachOutputBatch,
    output_bytes: &mut Vec<u8>,
    passthrough_batches: &mut Vec<(PaneOutputGeneration, Vec<TerminalPassthrough>)>,
    saw_gap: &mut bool,
) -> bool {
    match batch {
        AttachOutputBatch::Closed => true,
        AttachOutputBatch::Gap => {
            *saw_gap = true;
            false
        }
        AttachOutputBatch::Events {
            generation,
            bytes,
            passthroughs: batch_passthroughs,
            close_after_render,
            sustained: _,
            ..
        } => {
            output_bytes.extend_from_slice(&bytes);
            if !batch_passthroughs.is_empty() {
                passthrough_batches.push((generation, batch_passthroughs));
            }
            close_after_render
        }
    }
}
