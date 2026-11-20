use super::transport_state::ControlTransport;
use super::{
    arm_control_eof_transition, drain_ready_pane_events, enqueue_control_frame_terminal,
    handle_control_command_response_event, io, ControlCommandOrigin, ControlCommandResult,
};

impl ControlTransport {
    /// Closes the initial batch at its last command, or at the first command
    /// whose own guard failed.
    ///
    /// tmux 3.7b runs the command-line commands as one command list, which an
    /// error stops: `has-session -t x \; kill-session -t x` must not kill
    /// anything when `x` is missing. Each command keeps its own control line
    /// and flags-0 guard; the unstarted initial lines are dropped, never run.
    /// An error inside a child inserted by `if-shell` or `source-file` does
    /// not stop the list, as in tmux.
    pub(super) fn complete_initial_frame(
        &mut self,
        origin: ControlCommandOrigin,
        owner_failed: bool,
    ) {
        let ControlCommandOrigin::Initial { completes_batch } = origin else {
            return;
        };
        if !completes_batch && !owner_failed {
            return;
        }
        for _ in 0..self.command_numbering.cancel_remaining_initial_commands() {
            let Some(line) = self.queued_lines.pop_front() else {
                break;
            };
            self.queued_input_bytes = self.queued_input_bytes.saturating_sub(line.len());
        }
        self.initial_command_completion_pending = true;
    }

    pub(super) async fn finish_command(
        &mut self,
        task_result: Result<ControlCommandResult, tokio::task::JoinError>,
    ) -> io::Result<bool> {
        let result = task_result
            .map_err(|error| io::Error::other(format!("control command task failed: {error}")))?;
        let mut response_notifications = self
            .current_response_notifications
            .take()
            .expect("active control command owns a response channel");
        // The typed response sender is dropped before the task joins.
        // Drain its finite FIFO without waiting so the last child
        // boundary is applied before command completion.
        while let Ok(event) = response_notifications.try_recv() {
            let mut event_context = self.events.context(
                &self.handler,
                self.control_identity,
                &mut self.write_half,
                (&self.pane_event_tx, &mut self.pane_event_rx),
            );
            let command = self
                .current_command
                .as_mut()
                .expect("drained responses belong to an active command");
            let response_frame = self
                .current_response_frame
                .as_mut()
                .expect("active commands own response frame state");
            if handle_control_command_response_event(
                event,
                &mut self.command_numbering,
                command,
                response_frame,
                &mut event_context,
            )
            .await?
            {
                return Ok(true);
            }
        }
        let Some(mut command) = self.current_command.take() else {
            return Ok(false);
        };
        self.eof_completion.command_finished();
        self.eof_command_ready = false;
        let response_frame = self
            .current_response_frame
            .take()
            .expect("completed commands own response frame state");
        let server_shutdown_started = result.server_shutdown_started;
        if response_frame.open {
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
            if !result.stdout.is_empty() {
                self.events.output_queue.enqueue_stdout(result.stdout);
            }
            enqueue_control_frame_terminal(
                &mut self.events.output_queue,
                &mut command,
                result.error,
            );
        }
        self.flush_output().await?;
        self.complete_initial_frame(command.origin, command.owner_failed == Some(true));
        let shutdown_requested =
            server_shutdown_started || self.handler.request_shutdown_if_pending();
        if shutdown_requested {
            // The listener publishes its shutdown watch on a later
            // scheduler turn. Stop this exact queue synchronously so a
            // buffered frame cannot start after kill-server.
            self.shutdown_draining = true;
            self.input_closed = true;
            self.input_buffer.clear();
            self.queued_lines.clear();
            self.queued_input_bytes = 0;
            self.eof_transition = None;
            self.lifecycle.shutdown_handle.request_shutdown();
        } else if self.input_closed {
            arm_control_eof_transition(&mut self.eof_transition);
        }
        Ok(false)
    }
}
