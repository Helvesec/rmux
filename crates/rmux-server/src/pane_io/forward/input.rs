use super::*;
impl AttachForwarder {
    pub(super) async fn socket(&mut self, result: io::Result<bool>) -> io::Result<AttachStep> {
        if !result? {
            return attach_eof::finish_attach_eof(
                &self.stream,
                &mut self.decoder,
                &self.live_input,
                &self.shutdown,
                &self.closing,
                &mut self.current_target,
                &mut self.pending_input,
                &mut self.active_emit_cache,
                &mut self.locked,
                &mut self.pending_escape_flush,
            )
            .await
            .map(|()| AttachStep::Exit);
        }
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        let Some(socket_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        process_attach_socket_messages(
            &mut self.decoder,
            &self.stream,
            &self.live_input,
            &self.closing,
            &mut self.current_target,
            &mut self.pending_input,
            &mut self.active_emit_cache,
            &mut self.locked,
            &mut self.pane_refresh,
            &mut self.pending_escape_flush,
            &mut self.last_client_input_at,
        )
        .await?;
        drop(socket_batch);
        Ok(AttachStep::Continue)
    }
}

impl AttachForwarder {
    pub(super) async fn escape(&mut self) -> io::Result<AttachStep> {
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        let Some(_pending_escape_batch) =
            begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        flush_due_pending_escape_input(
            &mut self.pending_escape_flush,
            &self.live_input,
            &mut self.pending_input,
            self.locked,
        )
        .await?;
        Ok(AttachStep::Continue)
    }
}
