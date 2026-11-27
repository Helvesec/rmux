use super::*;

/// Keep the read-time foreground observation with a batch through worker and
/// admission delays. Unattributed bytes retain the conservative drain policy.
pub(super) struct PaneReadBatch {
    pub(super) bytes: Vec<u8>,
    #[cfg(unix)]
    pub(super) foreground_group: Option<u32>,
}

impl From<Vec<u8>> for PaneReadBatch {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            #[cfg(unix)]
            foreground_group: None,
        }
    }
}

#[cfg(test)]
pub(super) fn publish_pane_bytes(context: PanePublishContext<'_>, bytes: Vec<u8>) -> Vec<u8> {
    try_publish_pane_bytes(context, bytes, true)
        .unwrap_or_else(|_| unreachable!("test publication supplies admission"))
}

pub(super) fn try_publish_pane_bytes(
    context: PanePublishContext<'_>,
    batch: impl Into<PaneReadBatch>,
    rename_admitted: bool,
) -> Result<Vec<u8>, Vec<u8>> {
    let PaneReadBatch {
        bytes,
        #[cfg(unix)]
        foreground_group,
    } = batch.into();
    let PanePublishContext {
        session_name,
        pane_id,
        transcript,
        pane_output,
        generation,
        pane_alert_callback,
        status_terminal,
        emit_no_bell_alert,
    } = context;
    #[cfg(not(unix))]
    let _ = status_terminal;
    if !pane_output.accepts_generation(generation) {
        return Ok(Vec::new());
    }
    let Some((_sequence, append_result)) = pane_output
        .try_publish_for_generation_with_invalidation(generation, bytes, |bytes| {
            // This guard is taken under PaneOutputState, like every output
            // mutation: a strict lock would let a poison left by another
            // operation kill this reader and poison the output state too.
            let mut transcript = transcript.lock_for_output_mutation();
            // A policy read outside this guard could go off -> on before the
            // parser accepts ESC k. Refuse without effects, then reserve outside
            // both locks and retry. An off feed can publish through a full FIFO.
            if pane_alert_callback.is_some()
                && !rename_admitted
                && transcript.screen().allow_rename()
            {
                return None;
            }
            // Sample within the generation-checked transcript publication, not
            // before a potentially delayed blocking parse job.
            #[cfg(unix)]
            if let Some(terminal) = status_terminal {
                let current_group = rmux_os::process::unix::foreground_pid(terminal.as_fd());
                transcript.observe_foreground_group(current_group);
                // A clean parser and the same group at read and publication
                // establish a new batch boundary before its first report.
                // A delayed old-group batch or an unfinished old sequence
                // must still drain under the existing suppression policy.
                if foreground_group.is_some() && foreground_group == current_group {
                    transcript.finish_program_status_drain();
                }
            }
            let mouse_mode_before = transcript.mode() & rmux_core::input::mode::ALL_MOUSE_MODES;
            // These bytes came from this pane's own terminal, which is what
            // makes them unseen while a mode is up; everything this server
            // writes into a transcript itself keeps the common append.
            let mut append_result = transcript.append_pty_bytes_with_effects(bytes);
            let mouse_mode_changed =
                transcript.mode() & rmux_core::input::mode::ALL_MOUSE_MODES != mouse_mode_before;
            // The entire reader-owned batch must be parsed before an empty
            // kernel queue can establish the end of the old foreground drain.
            #[cfg(unix)]
            if transcript.program_status_transition_pending()
                && status_terminal.is_some_and(|terminal| {
                    rustix::io::ioctl_fionread(terminal).is_ok_and(|count| count == 0)
                })
            {
                transcript.finish_program_status_drain();
            }
            let passthroughs = std::mem::take(&mut append_result.passthroughs);
            let clipboard_set = passthroughs.iter().any(passthrough_is_clipboard_set);
            let clipboard_writes = passthroughs
                .iter()
                .filter_map(osc52_clipboard_write_payload)
                .collect::<Vec<_>>();
            let clipboard_queries = passthroughs
                .iter()
                .filter_map(TerminalPassthrough::clipboard_query_metadata)
                .collect::<Vec<_>>();
            if let Some(effects) = pane_alert_callback {
                // The requests enter the shared sequence at the point the
                // pipeline already serialises publications, so the order two
                // panes of one window were observed in is the order they are
                // applied in. The push never waits: this runs under the
                // output-sender and transcript locks.
                effects.renames().push(
                    session_name,
                    pane_id,
                    generation,
                    std::mem::take(&mut append_result.window_rename_requests),
                );
                effects.alert(PaneAlertEvent {
                    session_name: session_name.clone(),
                    pane_id,
                    bell_count: append_result.bell_count,
                    title_changed: append_result.title_changed,
                    title_change: append_result.title_change.clone(),
                    path_changed: append_result.path_changed,
                    clipboard_set,
                    clipboard_writes,
                    clipboard_queries,
                    mouse_mode_changed,
                    alternate_mode_changed: append_result.alternate_mode_changed,
                    queue_activity_alert: emit_no_bell_alert || append_result.bell_count > 0,
                    generation,
                });
            }
            let invalidation = append_result
                .recovery_rebase_required
                .then_some(crate::pane_io::PaneInvalidationReason::TranscriptMutation);
            Some((append_result, passthroughs, invalidation))
        })?
    else {
        return Ok(Vec::new());
    };
    if let Some(timer) = append_result.ground_timer {
        schedule_pane_transcript_timer(
            session_name,
            pane_id,
            transcript.clone(),
            pane_output.clone(),
            PaneTranscriptTimer::Ground(timer),
        );
    }
    if let Some(timer) = append_result.synchronized_output_timer {
        schedule_pane_transcript_timer(
            session_name,
            pane_id,
            transcript.clone(),
            pane_output.clone(),
            PaneTranscriptTimer::SynchronizedOutput(timer),
        );
    }
    let replies = append_result.replies;
    let dropped_passthrough_count = append_result.dropped_passthrough_count;
    if dropped_passthrough_count > 0 {
        warn!(
            session = %session_name,
            pane_id = pane_id.as_u32(),
            dropped = dropped_passthrough_count,
            "dropped terminal passthrough events due to parser safety limits"
        );
    }
    Ok(replies)
}

/// Publishes bytes through the production pane-output path for one real pane
/// and returns the alert events production built from them.
///
/// Tests that need the handler side of an alert drive it with these events
/// rather than a hand-written one, so what the parser reported and what the
/// handler acts on cannot drift apart.
#[cfg(test)]
pub(crate) fn publish_pane_bytes_capturing_alerts(
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: &SharedPaneTranscript,
    pane_output: &PaneOutputSender,
    bytes: Vec<u8>,
) -> Vec<super::PaneAlertEvent> {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    {
        let callback: super::PaneAlertCallback = Arc::new(crate::pane_io::PaneEffectSink::new(
            move |event| {
                sink.lock()
                    .expect("pane alert capture mutex must not be poisoned")
                    .push(event);
            },
            Arc::new(crate::pane_rename_queue::PaneRenameQueue::new()),
        ));
        let _ = publish_pane_bytes(
            PanePublishContext {
                session_name,
                pane_id,
                transcript,
                pane_output,
                generation: None,
                pane_alert_callback: Some(&callback),
                status_terminal: None,
                emit_no_bell_alert: false,
            },
            bytes,
        );
    }
    let captured = std::mem::take(
        &mut *events
            .lock()
            .expect("pane alert capture mutex must not be poisoned"),
    );
    captured
}

/// Publishes bytes through the production pane-output path using the server's
/// own effect sink, including its rename admission.
///
/// A witness that needs the handler side of a rename drives it with this,
/// because everything that decides the outcome — the parser, the owner stamp,
/// the admission, the ordered sequence and the alert callback — is then the
/// production one, in the production order.
#[cfg(test)]
pub(crate) async fn publish_pane_bytes_through_effects(
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: &SharedPaneTranscript,
    pane_output: &PaneOutputSender,
    generation: Option<u64>,
    effects: &PaneAlertCallback,
    bytes: Vec<u8>,
) {
    let _ = publish_pane_bytes_admitted(
        PanePublishContext {
            session_name,
            pane_id,
            transcript,
            pane_output,
            generation,
            pane_alert_callback: Some(effects),
            status_terminal: None,
            emit_no_bell_alert: true,
        },
        bytes,
        false,
    )
    .await
    .expect("inline publication cannot fail a worker join");
}

#[cfg(test)]
pub(crate) fn publish_pane_bytes_for_test(
    transcript: &SharedPaneTranscript,
    pane_output: &PaneOutputSender,
    bytes: Vec<u8>,
) {
    let session_name =
        rmux_proto::SessionName::new("pane-output-test").expect("test session name must be valid");
    let _ = publish_pane_bytes(
        PanePublishContext {
            session_name: &session_name,
            pane_id: PaneId::new(1),
            transcript,
            pane_output,
            generation: None,
            pane_alert_callback: None,
            status_terminal: None,
            emit_no_bell_alert: false,
        },
        bytes,
    );
}

/// Closes a pane through the production EOF path, including its release of an
/// unclosed synchronized interval, so an attach test observes the same
/// ordering a real child exit produces.
#[cfg(all(test, unix))]
pub(crate) fn publish_pane_eof_for_test(
    transcript: &SharedPaneTranscript,
    pane_output: &PaneOutputSender,
) {
    publish_pane_eof(transcript, pane_output, None);
}

pub(super) fn passthrough_is_clipboard_set(passthrough: &TerminalPassthrough) -> bool {
    passthrough.kind() == TerminalPassthroughKind::Clipboard
        && osc52_payload_is_clipboard_set(passthrough.payload())
}

pub(super) fn osc52_payload_is_clipboard_set(sequence: &[u8]) -> bool {
    let Some(body) = sequence.strip_prefix(b"\x1b]52;") else {
        return false;
    };
    let Some(body) = body
        .strip_suffix(b"\x07")
        .or_else(|| body.strip_suffix(b"\x1b\\"))
    else {
        return false;
    };
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return false;
    };
    let payload = &body[separator + 1..];
    if payload.is_empty() || payload == b"?" {
        return false;
    }
    // Use the same strict decoder as `osc52_clipboard_write_payload` so the
    // clipboard_set flag (which drives the PaneSetClipboard hook emission)
    // stays in sync with buffer storage. Base64 payloads that decode to zero
    // bytes (e.g. a lone `==` sequence) match the syntax-only classifier but
    // are dropped by paste_add on the frozen tmux 3.7b oracle — mirror that
    // so the hook does not fire without a stored buffer.
    osc52_payload_decodes(payload)
}

/// Decodes an inbound OSC 52 clipboard-write passthrough to its raw bytes, or
/// returns None for a query (`?`), an empty/malformed payload, or a
/// non-clipboard passthrough. Matches the frozen tmux 3.7b oracle: empty and
/// invalid-base64 writes create no paste buffer (input_osc_52 returns early
/// without paste_add).
pub(super) fn osc52_clipboard_write_payload(passthrough: &TerminalPassthrough) -> Option<Vec<u8>> {
    if passthrough.kind() != TerminalPassthroughKind::Clipboard {
        return None;
    }
    let body = passthrough.payload().strip_prefix(b"\x1b]52;")?;
    let body = body
        .strip_suffix(b"\x07")
        .or_else(|| body.strip_suffix(b"\x1b\\"))?;
    let separator = body.iter().position(|byte| *byte == b';')?;
    let payload = &body[separator + 1..];
    if payload.is_empty() || payload == b"?" {
        return None;
    }
    let decoded = decode_pane_clipboard_write_payload(payload)?;
    // A decoded length of 0 is possible when the base64 symbols round to zero
    // output bytes (e.g. a lone `=` sequence). tmux drops these too.
    if decoded.is_empty() {
        return None;
    }
    Some(decoded)
}

/// Same validation as `osc52_clipboard_write_payload` reduced to a boolean,
/// exposed to the outer-forward gate in `passthrough.rs`. Kept in this module
/// so both paths share the exact same decoder.
pub(in crate::pane_io) fn osc52_payload_decodes(payload: &[u8]) -> bool {
    decode_pane_clipboard_write_payload(payload)
        .map(|decoded| !decoded.is_empty())
        .unwrap_or(false)
}

#[cfg(any(unix, test))]
pub(super) async fn publish_pane_bytes_without_blocking_runtime(
    context: OwnedPanePublishContext,
    bytes: impl Into<PaneReadBatch>,
    rename_admitted: bool,
) -> io::Result<Result<Vec<u8>, Vec<u8>>> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    let bytes = bytes.into();
    let publish = move || {
        let context = PanePublishContext {
            session_name: &context.session_name,
            pane_id: context.pane_id,
            transcript: &context.transcript,
            pane_output: &context.pane_output,
            generation: context.generation,
            pane_alert_callback: context.pane_alert_callback.as_ref(),
            status_terminal: context.status_terminal.as_ref(),
            emit_no_bell_alert: context.emit_no_bell_alert,
        };
        try_publish_pane_bytes(context, bytes, rename_admitted)
    };
    if Handle::current().runtime_flavor() == RuntimeFlavor::MultiThread {
        return Ok(tokio::task::block_in_place(publish));
    }
    tokio::task::spawn_blocking(publish)
        .await
        .map_err(|error| io::Error::other(format!("pane parser task failed: {error}")))
}

#[cfg(unix)]
pub(super) async fn write_parser_replies_to_pane(
    pane_writer: &tokio::io::unix::AsyncFd<PtyIo>,
    replies: Vec<u8>,
) -> io::Result<()> {
    if replies.is_empty() {
        return Ok(());
    }

    let mut remaining = replies.as_slice();
    while !remaining.is_empty() {
        let mut ready = pane_writer.writable().await?;
        match ready.try_io(|inner| {
            rustix::io::write(inner.get_ref().as_fd(), remaining).map_err(io::Error::from)
        }) {
            Ok(Ok(0)) => {
                return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
            }
            Ok(Ok(bytes_written)) => remaining = &remaining[bytes_written..],
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn write_parser_replies_to_pane_blocking(
    pane_writer: &PtyIo,
    replies: Vec<u8>,
) -> io::Result<()> {
    if replies.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    super::windows_reply_probe::before_write();
    pane_writer.write_all(&replies)
}

// The caller owns this permit until the worker has completed, and explicitly
// drops it before any PTY/control wait. Pending names are charged separately.
type AdmittedPublication<'a> = (
    Vec<u8>,
    Option<crate::pane_rename_queue::PaneRenameAdmission<'a>>,
);

#[cfg(any(unix, test))]
pub(super) async fn publish_pane_bytes_admitted<'a>(
    context: PanePublishContext<'a>,
    bytes: impl Into<PaneReadBatch>,
    worker: bool,
) -> io::Result<AdmittedPublication<'a>> {
    let mut bytes = bytes.into();
    #[cfg(unix)]
    let foreground_group = bytes.foreground_group;
    let mut admission = None;
    loop {
        let result = if worker {
            publish_pane_bytes_without_blocking_runtime(
                OwnedPanePublishContext {
                    session_name: context.session_name.clone(),
                    pane_id: context.pane_id,
                    transcript: context.transcript.clone(),
                    pane_output: context.pane_output.clone(),
                    generation: context.generation,
                    pane_alert_callback: context.pane_alert_callback.cloned(),
                    status_terminal: context
                        .status_terminal
                        .map(|terminal| terminal.try_clone())
                        .transpose()
                        .map_err(io::Error::other)?,
                    emit_no_bell_alert: context.emit_no_bell_alert,
                },
                bytes,
                admission.is_some(),
            )
            .await?
        } else {
            try_publish_pane_bytes(context, bytes, admission.is_some())
        };
        match result {
            Ok(replies) => return Ok((replies, admission)),
            Err(unpublished) => {
                bytes = PaneReadBatch {
                    bytes: unpublished,
                    #[cfg(unix)]
                    foreground_group,
                };
                admission = Some(
                    context
                        .pane_alert_callback
                        .expect("rename sink refused")
                        .renames()
                        .admit()
                        .await,
                );
            }
        }
    }
}

#[cfg(windows)]
pub(super) fn publish_pane_bytes_admitted_blocking<'a>(
    context: PanePublishContext<'a>,
    mut bytes: Vec<u8>,
) -> AdmittedPublication<'a> {
    let mut admission = None;
    loop {
        match try_publish_pane_bytes(context, bytes, admission.is_some()) {
            Ok(replies) => return (replies, admission),
            Err(unpublished) => {
                bytes = unpublished;
                admission = Some(
                    context
                        .pane_alert_callback
                        .expect("rename sink refused")
                        .renames()
                        .admit_blocking(),
                );
            }
        }
    }
}
