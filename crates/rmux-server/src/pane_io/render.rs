use super::*;
#[allow(clippy::too_many_arguments)]
pub(super) async fn synchronize_persistent_overlay_epoch(
    persistent_overlay_epoch: &AtomicU64,
    stream: &AttachTransport,
    current_target: &types::OpenAttachTarget,
    attach_controls: Option<&mut mpsc::UnboundedReceiver<AttachControl>>,
    deferred_controls: &mut VecDeque<AttachControl>,
    control_backlog: &AtomicUsize,
    persistent_overlay: &mut Option<CachedPersistentOverlay>,
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

pub(super) fn clear_close_pane_output_after_refresh_if_target_changed(
    target_changed: bool,
    close_pane_output_after_refresh: &mut bool,
) {
    if target_changed {
        *close_pane_output_after_refresh = false;
    }
}

pub(super) async fn reschedule_status_refresh_if_target_changed(
    target_changed: bool,
    status_refresh: &mut AttachStatusRefreshScheduler,
    live_input: &LiveAttachInputContext,
    current_target: &types::OpenAttachTarget,
) {
    if target_changed {
        reschedule_status_refresh_for_target(status_refresh, live_input, current_target).await;
    }
}

pub(super) async fn emit_live_render_frame(
    stream: &AttachTransport,
    current_target: &mut types::OpenAttachTarget,
    frame: &PaneRenderDeltaFrame,
    replaceable: bool,
) -> io::Result<()> {
    if let Some(cursor_style) = frame.cursor_style_state() {
        if let Some(sequence) = current_target
            .outer_terminal
            .render_cursor_state_transition(Some(current_target.cursor_style), cursor_style)
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

pub(super) async fn try_forward_plain_output(
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

pub(super) async fn schedule_attach_render_refresh(
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

pub(super) fn collect_pending_attach_output_batch_metadata(
    current_target: &mut types::OpenAttachTarget,
) -> Option<AttachOutputBatch> {
    let pane_output = current_target.pane_output.as_mut()?;
    let first = pane_output.try_recv_drained()?;
    Some(collect_attach_output_batch_metadata(
        first,
        Some(pane_output),
    ))
}

pub(super) async fn refresh_current_attach_client(live_input: &LiveAttachInputContext) {
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

pub(super) async fn reschedule_status_refresh_for_target(
    status_refresh: &mut AttachStatusRefreshScheduler,
    live_input: &LiveAttachInputContext,
    current_target: &types::OpenAttachTarget,
) {
    reschedule_status_refresh_for_session(status_refresh, live_input, &current_target.session_name)
        .await;
}

pub(super) async fn reschedule_status_refresh_for_session(
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

pub(super) fn should_treat_attach_output_as_interactive(
    last_client_input_at: Option<Instant>,
) -> bool {
    last_client_input_at.is_some_and(|input_at| {
        Instant::now().saturating_duration_since(input_at) <= ATTACH_INTERACTIVE_OUTPUT_WINDOW
    })
}
