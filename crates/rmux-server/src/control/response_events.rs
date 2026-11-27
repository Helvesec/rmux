use super::{
    apply_control_pane_output_actions, drain_ready_pane_events, format_guard_line,
    handle_server_event, io, refresh_control_read_context, unix_epoch_seconds,
    write_control_notification, ActiveControlCommand, ActiveControlResponseFrame,
    ControlCommandNumbering, ControlCommandResponseEvent, ControlGuardKind, ControlOutputQueue,
    ControlServerEvent, DeferredSessionChange, RmuxError, ServerEventContext,
};
use tokio::sync::mpsc;

#[cfg(any(unix, windows))]
pub(super) async fn handle_control_command_response_event(
    event: ControlCommandResponseEvent,
    numbering: &mut ControlCommandNumbering,
    command: &mut ActiveControlCommand,
    response_frame: &mut ActiveControlResponseFrame,
    context: &mut ServerEventContext<'_>,
) -> io::Result<bool> {
    match event {
        ControlCommandResponseEvent::Notification(line) => {
            if !response_frame.open {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "control response notification has no open owner guard",
                ));
            }
            write_control_notification(line, context).await
        }
        ControlCommandResponseEvent::PaneOutputActions(actions) => {
            if !response_frame.open {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "control pane action has no open owner guard",
                ));
            }
            refresh_control_read_context(context).await;
            apply_control_pane_output_actions(actions, context).await;
            context.flush_output().await?;
            Ok(false)
        }
        ControlCommandResponseEvent::FrameStarted { origin } => {
            if response_frame.open {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "synchronous {origin:?} child started before its owner guard completed"
                    ),
                ));
            }
            command.timestamp = unix_epoch_seconds();
            command.command_number = numbering.next_synchronous_child_number();
            response_frame.open = true;
            context.output_queue.response_open = true;
            response_frame.origin = Some(origin);
            context.output_queue.enqueue_line(
                format_guard_line(
                    ControlGuardKind::Begin,
                    command.timestamp,
                    command.command_number,
                    command.guard_flag,
                )
                .into_bytes(),
                false,
            );
            context.flush_output().await?;
            Ok(false)
        }
        ControlCommandResponseEvent::FrameCompleted {
            stdout,
            error,
            trailing_stdout,
        } => {
            if !response_frame.open {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "control response completed without an open owner guard",
                ));
            }
            refresh_control_read_context(context).await;
            if !stdout.is_empty() {
                context.output_queue.enqueue_stdout(stdout);
            }
            enqueue_control_frame_terminal(context.output_queue, command, error);
            response_frame.open = false;
            if !trailing_stdout.is_empty() {
                context.output_queue.enqueue_stdout(trailing_stdout);
            }
            if drain_ready_pane_events(
                context.pane_event_rx,
                &context.pane_event_tx,
                context.output_queue,
                context.paused_panes,
                context.pane_output_states,
                *context.flags,
            )
            .await?
            {
                context.flush_output().await?;
                return Ok(true);
            }
            let _completed_origin = response_frame.origin.take();
            context.flush_output().await?;
            Ok(false)
        }
    }
}

#[cfg(any(unix, windows))]
pub(super) fn enqueue_control_frame_terminal(
    output_queue: &mut ControlOutputQueue,
    command: &mut ActiveControlCommand,
    error: Option<RmuxError>,
) {
    let terminal = match error {
        Some(error) => {
            output_queue.enqueue_stdout(error.tmux_message().into_bytes());
            ControlGuardKind::Error
        }
        None => ControlGuardKind::End,
    };
    output_queue.response_open = false;
    command
        .owner_failed
        .get_or_insert(terminal == ControlGuardKind::Error);
    output_queue.enqueue_line(
        format_guard_line(
            terminal,
            command.timestamp,
            command.command_number,
            command.guard_flag,
        )
        .into_bytes(),
        false,
    );
}

#[cfg(any(unix, windows))]
pub(super) async fn flush_deferred_server_events(
    context: &mut ServerEventContext<'_>,
) -> io::Result<bool> {
    while let Some(line) = context.deferred.pop_notification() {
        // These notifications were accepted before the deferred Exit. Re-entering
        // the admission path would discard them because that Exit is now pending.
        if write_control_notification(line, context).await? {
            return Ok(true);
        }
    }

    if let Some(DeferredSessionChange {
        session_name,
        pane_sequences,
    }) = context.deferred.session_change.take()
    {
        let event = match (session_name, pane_sequences) {
            (Some(session_name), Some(pane_sequences)) => ControlServerEvent::SessionChangedAt {
                session_name,
                pane_sequences,
            },
            (next_session, None) => ControlServerEvent::SessionChanged(next_session),
            (None, Some(_)) => unreachable!("pane cursors require a control session"),
        };
        if handle_server_event(event, context, false).await? {
            return Ok(true);
        }
    }

    if let Some(reason) = context.deferred.exit_reason.take() {
        return handle_server_event(ControlServerEvent::Exit(reason), context, false).await;
    }

    Ok(false)
}

/// Reconciles the bounded snapshot of server events already queued for this
/// transport, for example after a completed command whose events lost the
/// race against its completion.
///
/// Those events belong to the command that published them, so they keep the
/// order its frame gives them when they arrive while it runs: notifications
/// first, then the session change that starts the attached panes' output.
/// tmux 3.7b drains its notification queue right after the command that
/// attached the client, before it dispatches any pane read, so
/// `%session-changed` always precedes the new session's `%output`.
/// New arrivals are left to the caller's select, so a continuous producer
/// cannot starve it.
#[cfg(any(unix, windows))]
pub(super) async fn reconcile_ready_server_events(
    server_events: &mut mpsc::Receiver<ControlServerEvent>,
    context: &mut ServerEventContext<'_>,
) -> io::Result<bool> {
    for _ in 0..server_events.len() {
        let Ok(event) = server_events.try_recv() else {
            break;
        };
        if handle_server_event(event, context, true).await? {
            return Ok(true);
        }
    }
    flush_deferred_server_events(context).await
}
