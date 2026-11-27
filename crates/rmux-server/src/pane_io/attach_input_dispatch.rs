use std::io;

use rmux_proto::{AttachFrameDecoder, AttachMessage};

use super::{
    emit_attach_bytes, emit_attach_frame, emit_render_frame, invalid_attach_message,
    pending_escape, pending_escape_deadline_due, process_attach_data_payload, types,
    AttachTransport, LiveAttachInputContext, PendingAttachInputState, ATTACH_INPUT_STACK_PAYLOAD,
};
#[cfg(test)]
use super::{pause_after_live_attach_input_validation, pause_before_live_attach_input_validation};

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
pub(super) async fn process_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    current_target: Option<&mut types::OpenAttachTarget>,
    pending_input: PendingAttachInputState<'_>,
    active_emit_cache: &mut Option<(u64, rmux_proto::WindowTarget)>,
    locked: &mut bool,
) -> io::Result<bool> {
    process_socket_messages_until_barrier(
        decoder,
        stream,
        live_input,
        current_target,
        pending_input,
        active_emit_cache,
        locked,
    )
    .await
    .map(|(forwarded, _)| forwarded)
}

#[cfg(any(unix, windows))]
pub(super) async fn process_socket_messages_until_barrier(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    mut current_target: Option<&mut types::OpenAttachTarget>,
    mut pending_input: PendingAttachInputState<'_>,
    active_emit_cache: &mut Option<(u64, rmux_proto::WindowTarget)>,
    locked: &mut bool,
) -> io::Result<(bool, bool)> {
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
    let mut stopped_at_barrier = false;
    let mut deferred_outputs = Vec::new();
    let mut data_scratch = [0_u8; ATTACH_INPUT_STACK_PAYLOAD];
    'messages: loop {
        loop {
            if pending_escape_deadline_due(pending_escape_flush) {
                stopped_at_barrier = true;
                break 'messages;
            }
            let Some(bytes) = decoder
                .next_data_payload_into(&mut data_scratch)
                .map_err(invalid_attach_message)?
            else {
                break;
            };
            let input_dispatch =
                pending_escape::PendingInputDispatch::capture(pending_input, bytes);
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
            pending_escape_flush.observe_input_dispatch(input_dispatch, pending_input);
        }

        if pending_escape_deadline_due(pending_escape_flush) {
            stopped_at_barrier = true;
            break;
        }
        let Some(message) = decoder.next_message().map_err(invalid_attach_message)? else {
            break;
        };
        match message {
            AttachMessage::Data(bytes) => {
                let input_dispatch =
                    pending_escape::PendingInputDispatch::capture(pending_input, &bytes);
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
                pending_escape_flush.observe_input_dispatch(input_dispatch, pending_input);
            }
            AttachMessage::Keystroke(keystroke) => {
                let input_dispatch =
                    pending_escape::PendingInputDispatch::capture(pending_input, keystroke.bytes());
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
                pending_escape_flush.observe_input_dispatch(input_dispatch, pending_input);
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
            | AttachMessage::DetachExecShellCommand(_)
            | AttachMessage::ServerExited => {
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
                stopped_at_barrier = true;
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

    Ok((forwarded_to_pane, stopped_at_barrier))
}
