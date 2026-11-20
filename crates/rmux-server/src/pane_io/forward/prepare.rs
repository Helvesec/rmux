use super::*;
impl AttachForwarder {
    pub(super) async fn prepare(&mut self) -> io::Result<AttachStep> {
        if self.shutdown_draining || attach_shutdown_observable(&self.shutdown) {
            if self.closing.load(Ordering::SeqCst) {
                if let Some(control) = take_pending_terminal_attach_control(
                    &mut self.deferred_controls,
                    self.attach_controls.as_mut(),
                    &self.control_backlog,
                ) {
                    // Terminal controls only finish the already-committed pane-output and
                    // transport sequence. They never enter RequestHandler mutation paths, so
                    // honoring one here preserves the finite exit banner without admitting a
                    // post-shutdown attach mutation.
                    let reason = finish_terminal_attach_control(
                        control,
                        &self.stream,
                        &mut self.current_target,
                        &mut self.deferred_passthroughs,
                        self.shutdown_pending_output_batch.take(),
                    )
                    .await?;
                    log_attach_exit(&self.live_input, &self.current_target, reason);
                    return Ok(AttachStep::Exit);
                }
            }
            let reason = if self.pending_shutdown_requested {
                AttachExitReason::PendingServerShutdown
            } else {
                AttachExitReason::ServerShutdown
            };
            log_attach_exit(&self.live_input, &self.current_target, reason);
            let _ = emit_server_exited_attach_stop(
                &self.stream,
                &self.current_target,
                self.server_exit_notice,
            )
            .await;
            return Ok(AttachStep::Exit);
        }
        // Socket reads and attach-control refreshes can both remain
        // continuously ready. Service an expired input ambiguity before
        // either queue so its deadline is a real upper bound rather than
        // merely another selectable wakeup.
        let Some(pending_escape_batch) =
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
        drop(pending_escape_batch);
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        let Some(control_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        synchronize_persistent_overlay_epoch(
            &self.persistent_overlay_epoch,
            &self.stream,
            &self.current_target,
            self.attach_controls.as_mut(),
            &mut self.deferred_controls,
            &self.control_backlog,
            &mut self.persistent_overlay,
            &mut self.persistent_overlay_visible,
            &mut self.persistent_overlay_state_id,
        )
        .await?;
        match self.apply_queued_controls().await? {
            AttachStep::Wait => {}
            step => return Ok(step),
        }
        drop(control_batch);
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        // A pending repaint must not stop input from reaching the pane.
        // The repaint is rendered from the current transcript when its
        // deadline fires, so fresh input can safely pull the deadline in.
        for _ in 0..MAX_IMMEDIATE_ATTACH_READS {
            match try_read_socket_bytes(&self.stream, &mut self.decoder)? {
                TryAttachRead::Read => {}
                TryAttachRead::Closed => {
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
                TryAttachRead::WouldBlock => break,
            }
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
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        let Some(control_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        prime_persistent_overlay_barriers(
            &mut self.persistent_overlay_state_id,
            self.attach_controls.as_mut(),
            &mut self.deferred_controls,
            &self.control_backlog,
        );
        match self.apply_queued_controls().await? {
            AttachStep::Wait => {}
            step => return Ok(step),
        }
        drop(control_batch);
        self.pending_shutdown_requested |= self.live_input.handler.request_shutdown_if_pending();
        Ok(AttachStep::Wait)
    }

    async fn apply_queued_controls(&mut self) -> io::Result<AttachStep> {
        match self.apply_pending_controls().await? {
            PendingAttachAction::Exit(PendingAttachExit { reason, .. }) => {
                finish_pending_attach_exit(
                    reason,
                    &self.stream,
                    &mut self.current_target,
                    &mut self.deferred_passthroughs,
                )
                .await?;
                log_attach_exit(&self.live_input, &self.current_target, reason);
                return Ok(AttachStep::Exit);
            }
            PendingAttachAction::Continue { target_changed } => {
                reschedule_status_refresh_if_target_changed(
                    target_changed,
                    &mut self.status_refresh,
                    &self.live_input,
                    &self.current_target,
                )
                .await;
                clear_close_pane_output_after_refresh_if_target_changed(
                    target_changed,
                    &mut self.close_pane_output_after_refresh,
                );
                clear_deferred_passthroughs_if_target_changed(
                    target_changed,
                    &mut self.deferred_passthroughs,
                );
                flush_deferred_passthroughs(
                    &self.stream,
                    &self.current_target,
                    &mut self.deferred_passthroughs,
                    &mut self.pane_refresh,
                    self.persistent_overlay_visible,
                    self.persistent_overlay.is_some(),
                )
                .await?;
                return Ok(AttachStep::Continue);
            }
            PendingAttachAction::InteractiveInput => {
                mark_attach_interactive_input(
                    &mut self.pane_refresh,
                    &mut self.last_client_input_at,
                );
                absorb_transient_terminal_prefix(
                    &self.live_input,
                    &mut self.pending_input,
                    &mut self.pending_escape_flush,
                )
                .await;
                self.pane_refresh.schedule_now();
                return Ok(AttachStep::Continue);
            }
            PendingAttachAction::Refresh {
                target_changed,
                source_invalidated,
            } => {
                reschedule_status_refresh_if_target_changed(
                    target_changed,
                    &mut self.status_refresh,
                    &self.live_input,
                    &self.current_target,
                )
                .await;
                clear_close_pane_output_after_refresh_if_target_changed(
                    target_changed,
                    &mut self.close_pane_output_after_refresh,
                );
                clear_deferred_passthroughs_if_target_changed(
                    source_invalidated,
                    &mut self.deferred_passthroughs,
                );
                schedule_attach_render_refresh(
                    &mut self.pane_refresh,
                    &mut self.pane_refresh_requires_full,
                    &self.live_input,
                )
                .await;
                return Ok(AttachStep::Continue);
            }
            PendingAttachAction::Write => {}
        }
        Ok(AttachStep::Wait)
    }

    pub(super) async fn apply_pending_controls(&mut self) -> io::Result<PendingAttachAction> {
        apply_pending_attach_controls(
            &mut self.deferred_controls,
            self.attach_controls.as_mut(),
            &self.control_backlog,
            &mut self.current_target,
            &self.stream,
            &mut self.deferred_passthroughs,
            &mut self.pane_refresh,
            &mut self.render_generation,
            &mut self.overlay_generation,
            &mut self.persistent_overlay,
            &mut self.persistent_overlay_visible,
            &mut self.persistent_overlay_state_id,
            &mut self.locked,
            Some(PendingAttachInputState::new(
                &mut self.pending_input,
                &mut self.pending_escape_flush,
            )),
        )
        .await
    }
}
