use super::*;
impl AttachForwarder {
    pub(super) async fn status(&mut self) -> io::Result<AttachStep> {
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        let Some(_control_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
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
                reschedule_status_refresh_for_target(
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
        if self.closing.load(Ordering::SeqCst) {
            log_attach_exit(
                &self.live_input,
                &self.current_target,
                AttachExitReason::AttachClosingFlag,
            );
            let _ = emit_attach_stop(&self.stream, &self.current_target).await;
            return Ok(AttachStep::Exit);
        }
        let session_name = self.current_target.session_name.clone();
        if self.locked {
            reschedule_status_refresh_for_session(
                &mut self.status_refresh,
                &self.live_input,
                &session_name,
            )
            .await;
            return Ok(AttachStep::Continue);
        }
        let _ = self
            .live_input
            .handler
            .refresh_attached_client_status(self.live_input.attach_pid(), &session_name)
            .await;
        reschedule_status_refresh_for_session(
            &mut self.status_refresh,
            &self.live_input,
            &session_name,
        )
        .await;
        Ok(AttachStep::Continue)
    }
}
