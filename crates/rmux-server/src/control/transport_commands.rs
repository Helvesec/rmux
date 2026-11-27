#[cfg(windows)]
use super::acquire_control_eof_queue_lease;
use super::active_command::spawn_control_command_with_first_poll;
use super::transport_state::ControlTransport;
#[cfg(windows)]
use super::CONTROL_STDIN_EOF_MARKER;
use super::{
    arm_control_eof_transition, control_commands_require_drain, drain_ready_pane_events,
    format_guard_line, io, mpsc, unix_epoch_seconds, validate_control_command_arguments,
    with_control_command_response_sink, with_control_queue_eof_cancellation, ActiveControlCommand,
    ActiveControlResponseFrame, Arc, ControlCommandResponseSink, ControlGuardKind,
    ControlQueueEofCancellation, CONTROL_SERVER_EVENT_CAPACITY,
};

impl ControlTransport {
    pub(super) async fn start_queued_commands(&mut self) -> io::Result<bool> {
        let requester_pid = self.control_identity.requester_pid();
        let control_id = self.control_identity.control_id();
        while !self.shutdown_draining && self.current_command.is_none() {
            let Some(next_line) = self.queued_lines.front() else {
                break;
            };
            #[cfg(windows)]
            if next_line == CONTROL_STDIN_EOF_MARKER {
                self.queued_lines
                    .pop_front()
                    .expect("peeked control EOF marker remains queued");
                self.input_closed = true;
                acquire_control_eof_queue_lease(
                    &mut self.eof_queue_lease,
                    &self.handler,
                    self.control_identity,
                )
                .await;
                arm_control_eof_transition(&mut self.eof_transition);
                self.input_buffer.clear();
                self.queued_lines.clear();
                self.queued_input_bytes = 0;
                self.eof_completion
                    .observe_stdin_eof_marker(self.events.attachment.is_attached(), false);
                break;
            }
            if next_line.is_empty() {
                self.queued_lines
                    .pop_front()
                    .expect("peeked empty control line remains queued");
                self.events.output_queue.enqueue_exit(None);
                self.flush_output().await?;
                return Ok(true);
            }
            let parsed_commands = self
                .handler
                .parse_control_commands(next_line)
                .await
                .and_then(validate_control_command_arguments);
            let requires_drain = parsed_commands
                .as_ref()
                .is_ok_and(control_commands_require_drain);
            let Some(normal_request_guard) = self.handler.try_begin_normal_request(requires_drain)
            else {
                self.shutdown_draining = true;
                self.input_closed = true;
                self.input_buffer.clear();
                self.queued_lines.clear();
                self.queued_input_bytes = 0;
                self.eof_transition = None;
                break;
            };
            let line = self
                .queued_lines
                .pop_front()
                .expect("admitted control command remains queued");
            self.queued_input_bytes = self.queued_input_bytes.saturating_sub(line.len());

            if drain_ready_pane_events(
                &mut self.pane_event_rx,
                &self.pane_event_tx,
                &mut self.events.output_queue,
                &mut self.events.paused_panes,
                &mut self.events.pane_output_states,
                self.events.flags,
            )
            .await?
            {
                self.flush_output().await?;
                return Ok(true);
            }
            self.events.output_queue.response_open = true;
            let timestamp = unix_epoch_seconds();
            let command_frame = self.command_numbering.next_frame();
            self.events.output_queue.enqueue_line(
                format_guard_line(
                    ControlGuardKind::Begin,
                    timestamp,
                    command_frame.number,
                    command_frame.guard_flag,
                )
                .into_bytes(),
                false,
            );
            self.flush_output().await?;

            match parsed_commands {
                Ok(commands) => {
                    self.eof_completion.command_started(&commands);
                    let handler = Arc::clone(&self.handler);
                    let eof_cancellation = ControlQueueEofCancellation::new(self.control_identity);
                    if self.input_closed {
                        eof_cancellation.cancel_for_eof();
                    }
                    let task_eof_cancellation = eof_cancellation.clone();
                    let (response_tx, response_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
                    let response_sink = ControlCommandResponseSink::new(
                        self.control_identity,
                        response_tx,
                        Arc::clone(&self.lifecycle.closing),
                        command_frame.guard_flag == 1,
                        self.write_half.has_typed_status(),
                    );
                    self.current_response_notifications = Some(response_rx);
                    self.current_response_frame = Some(ActiveControlResponseFrame::owner());
                    let (task, first_poll) = spawn_control_command_with_first_poll(async move {
                        let _normal_request_guard = normal_request_guard;
                        with_control_command_response_sink(
                            response_sink,
                            with_control_queue_eof_cancellation(
                                task_eof_cancellation,
                                handler.execute_control_commands_identity(
                                    requester_pid,
                                    control_id,
                                    commands,
                                ),
                            ),
                        )
                        .await
                    });
                    self.current_command = Some(ActiveControlCommand {
                        timestamp,
                        command_number: command_frame.number,
                        guard_flag: command_frame.guard_flag,
                        origin: command_frame.origin,
                        owner_failed: None,
                        eof_cancellation,
                        task: Some(task),
                    });
                    if self.input_closed {
                        arm_control_eof_transition(&mut self.eof_transition);
                        if !self.mode.is_control_control()
                            && self.eof_completion.allows_detached_drain_transition()
                        {
                            // A deadline inherited from earlier frames must not
                            // close a new response before its task has run once.
                            // The active command owns the task before this wait,
                            // so cancellation still aborts it. A genuinely pending
                            // job remains subject to the unchanged global grace.
                            // On a multithreaded runtime this receipt can wake
                            // the transport before the JoinHandle publishes its
                            // already-ready result. Preserve that result too.
                            self.eof_command_ready = first_poll.await.unwrap_or(false);
                        }
                    }
                }
                Err(error) => {
                    drop(normal_request_guard);
                    let message = match error {
                        super::RmuxError::Server(message) => message,
                        other => other.to_string(),
                    };
                    self.events
                        .output_queue
                        .enqueue_stdout(format!("parse error: {message}").into_bytes());
                    self.events.output_queue.enqueue_line(
                        format_guard_line(
                            ControlGuardKind::Error,
                            timestamp,
                            command_frame.number,
                            command_frame.guard_flag,
                        )
                        .into_bytes(),
                        false,
                    );
                    self.events.output_queue.response_open = false;
                    self.flush_output().await?;
                    if command_frame.origin.is_initial() {
                        self.exit_status = 1;
                        self.complete_initial_frame(command_frame.origin, true);
                        break;
                    }
                }
            }
        }
        Ok(false)
    }
}

#[cfg(all(test, unix))]
mod parse_error_fifo_tests;
