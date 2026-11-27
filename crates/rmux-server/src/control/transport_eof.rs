use super::transport_state::ControlTransport;
use super::{
    close_active_control_command_on_eof, drain_control_queue_after_eof, io, ControlQueueDrainLease,
    EofDrainContext, Ordering,
};

impl ControlTransport {
    pub(super) async fn finish_eof(mut self) -> io::Result<()> {
        // The transport can close as soon as the active guard has
        // been terminated, but every complete frame accepted before
        // EOF still belongs to this exact control registration. Keep
        // its queue lease until those finite frames have drained.
        let deferred_exit_reason = self.events.deferred_server_events.exit_reason.take();
        let lease = match self.eof_queue_lease {
            Some(lease) => lease,
            None => {
                self.handler
                    .begin_control_queue_drain(self.control_identity)
                    .await
            }
        };
        // Exit is terminal even when its event was consumed while the
        // active command was still running. A missing exact
        // registration or an already-closing lifecycle is terminal
        // too: the Exit producer may have detached and removed this
        // control client even when its event channel was full.
        // Acquire the lease when possible so a same-PID replacement
        // still waits for the active frame, but never turn its absence
        // into a product error after the transport has reached EOF.
        let stop_after_active = deferred_exit_reason.is_some()
            || lease == ControlQueueDrainLease::Unavailable
            || self.lifecycle.closing.load(Ordering::SeqCst);
        let closed = close_active_control_command_on_eof(
            &mut self.current_command,
            &mut self.current_response_frame,
            &mut self.events.output_queue,
            &mut self.write_half,
            self.events.flags,
            &mut self.events.paused_panes,
            deferred_exit_reason.as_ref().and_then(Option::as_deref),
        )
        .await;
        let transport_drain_result =
            if closed.transport_result.is_ok() && !self.events.output_queue.is_transport_closed() {
                self.transport_drain
                    .finish(&self.handler, self.control_identity)
                    .await
            } else {
                Ok(())
            };
        #[cfg(windows)]
        {
            // Tokio cannot half-close a named pipe. Release both split
            // halves after the peer has consumed the terminal `%exit`
            // so transport EOF remains the unambiguous completion
            // signal while the finite command queue drains below.
            drop(self.read_half);
            drop(self.write_half);
        }
        let mut drain_context = EofDrainContext {
            server_events: &mut self.server_events,
            events_open: true,
            handler: &self.handler,
            control_identity: self.control_identity,
            shutdown: &mut self.shutdown,
            shutdown_handle: &self.lifecycle.shutdown_handle,
        };
        let drain_result = drain_control_queue_after_eof(
            closed.task,
            &mut self.queued_lines,
            &mut self.queued_input_bytes,
            stop_after_active,
            &mut drain_context,
        )
        .await;
        drain_result?;
        transport_drain_result?;
        closed.transport_result
    }
}
