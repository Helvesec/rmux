use super::*;
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
    let active_query = live_input
        .handler
        .attached_palette_query_is_active_for_identity(live_input.identity)
        .await;
    sync_pending_escape_flush_with_escape_time(
        pending_escape_flush,
        pending_input,
        pending_escape::keyboard_escape_time(escape_time, active_query),
    );
}

pub(super) async fn absorb_transient_terminal_prefix(
    live_input: &LiveAttachInputContext,
    pending_input: &mut Vec<u8>,
    pending_escape_flush: &mut PendingEscapeFlush,
) {
    let prefix = live_input
        .handler
        .take_transient_terminal_prefix_for_identity(live_input.identity)
        .await;
    if prefix.is_empty() {
        return;
    }
    pending_input.extend(prefix);
    sync_pending_escape_flush(pending_escape_flush, live_input, pending_input).await;
}

pub(super) async fn flush_due_pending_escape_input(
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

pub(super) fn pending_escape_deadline_due(pending_escape_flush: &PendingEscapeFlush) -> bool {
    pending_escape_flush
        .deadline()
        .is_some_and(|deadline| deadline <= Instant::now())
}

pub(super) fn sync_pending_escape_flush_with_escape_time(
    pending_escape_flush: &mut PendingEscapeFlush,
    pending_input: &[u8],
    escape_time: Duration,
) {
    // `PendingEscapeFlush` owns the retained-input grammar. Keeping the
    // classifier out of this wrapper prevents the decoder/timer split-brain
    // that previously dropped APC and numeric CSI deadlines here.
    pending_escape_flush.sync(pending_input, escape_time);
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn process_attach_socket_messages(
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

pub(super) fn mark_attach_interactive_input(
    pane_refresh: &mut AttachRefreshScheduler,
    last_client_input_at: &mut Option<Instant>,
) {
    *last_client_input_at = Some(Instant::now());
    pane_refresh.note_interactive_output();
}

pub(super) async fn process_attach_data_payload(
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
pub(super) fn is_predictable_local_echo(bytes: &[u8]) -> bool {
    predictable_local_echo_prefix_len(bytes) == bytes.len()
}

#[cfg(all(unix, test))]
pub(super) fn predictable_local_echo_prefix_len(bytes: &[u8]) -> usize {
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
pub(super) fn consume_predicted_echo(
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
pub(super) enum PredictedEcho {
    NoPrediction,
    Consumed,
    Mismatch,
}
