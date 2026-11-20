use super::*;
impl AttachForwarder {
    pub(super) async fn control(
        &mut self,
        control: Option<AttachControl>,
    ) -> io::Result<AttachStep> {
        if attach_shutdown_observable(&self.shutdown) {
            if let Some(control) = control {
                self.deferred_controls.push_front(control);
            }
            return Ok(AttachStep::Continue);
        }
        let Some(_control_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            if let Some(control) = control {
                self.deferred_controls.push_front(control);
            }
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        // Dismissal publishes its persistent-overlay epoch before
        // the fresh base frame is ready. This select arm may have
        // already received a stale tree control while that epoch
        // advanced, so fence it again immediately before dispatch.
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
        match control {
            Some(AttachControl::Detach) => {
                log_attach_exit(
                    &self.live_input,
                    &self.current_target,
                    AttachExitReason::AttachControlDetach,
                );
                let _ = emit_detached_attach_stop(&self.stream, &self.current_target).await;
                return Ok(AttachStep::Exit);
            }
            Some(AttachControl::Exited) => {
                finish_pending_attach_exit(
                    AttachExitReason::AttachControlExited,
                    &self.stream,
                    &mut self.current_target,
                    &mut self.deferred_passthroughs,
                )
                .await?;
                log_attach_exit(
                    &self.live_input,
                    &self.current_target,
                    AttachExitReason::AttachControlExited,
                );
                return Ok(AttachStep::Exit);
            }
            Some(AttachControl::DetachKill) => {
                log_attach_exit(
                    &self.live_input,
                    &self.current_target,
                    AttachExitReason::AttachControlDetachKill,
                );
                emit_attach_stop(&self.stream, &self.current_target).await?;
                emit_attach_message(&self.stream, &AttachMessage::DetachKill).await?;
                return Ok(AttachStep::Exit);
            }
            Some(AttachControl::DetachExecShellCommand(command)) => {
                log_attach_exit(
                    &self.live_input,
                    &self.current_target,
                    AttachExitReason::AttachControlDetachExec,
                );
                emit_attach_stop(&self.stream, &self.current_target).await?;
                emit_attach_message(
                    &self.stream,
                    &AttachMessage::DetachExecShellCommand(command),
                )
                .await?;
                return Ok(AttachStep::Exit);
            }
            Some(AttachControl::Refresh) => {
                schedule_attach_render_refresh(
                    &mut self.pane_refresh,
                    &mut self.pane_refresh_requires_full,
                    &self.live_input,
                )
                .await;
            }
            Some(AttachControl::InteractiveInput) => {
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
            }
            Some(AttachControl::Switch(next_target)) => {
                let (next_target, switch_count) = coalesce_render_switches(
                    next_target,
                    &mut self.deferred_controls,
                    self.attach_controls.as_mut(),
                    &self.control_backlog,
                );
                let drop_live_output = !preserves_live_output(&self.current_target, &next_target);
                let (pending_generation, pending_passthroughs) = if drop_live_output {
                    (PaneOutputGeneration::unsubscribed(), Vec::new())
                } else {
                    take_pending_live_passthroughs(
                        &mut self.current_target,
                        next_target.pane_output_start_sequence,
                    )
                };
                if is_stale_persistent_switch(
                    self.persistent_overlay_state_id,
                    next_target.as_ref(),
                ) {
                    self.render_generation = self.render_generation.saturating_add(switch_count);
                    return Ok(AttachStep::Continue);
                }
                self.close_pane_output_after_refresh = false;
                self.render_generation = self.render_generation.saturating_add(switch_count);
                PendingAttachInputState::new(
                    &mut self.pending_input,
                    &mut self.pending_escape_flush,
                )
                .clear_if_pane_source_changed(&self.current_target, next_target.as_ref());
                clear_deferred_passthroughs_if_target_changed(
                    drop_live_output,
                    &mut self.deferred_passthroughs,
                );
                let pending_overlay = take_pending_persistent_overlay_for_state(
                    self.attach_controls.as_mut(),
                    &mut self.deferred_controls,
                    next_target.persistent_overlay_state_id,
                    self.render_generation,
                    self.overlay_generation,
                    &self.control_backlog,
                );
                let (replacement_frame, clear_screen) = switch_persistent_overlay_frame(
                    &mut self.persistent_overlay,
                    &mut self.persistent_overlay_visible,
                    pending_overlay.as_ref(),
                    self.persistent_overlay_state_id,
                    self.current_target.persistent_overlay_state_id,
                    next_target.as_ref(),
                );
                if let Some(overlay) = pending_overlay.as_ref() {
                    self.overlay_generation = overlay.overlay_generation;
                }
                switch_attach_target(
                    &self.stream,
                    &mut self.current_target,
                    *next_target,
                    clear_screen,
                    replacement_frame.as_deref(),
                )
                .await?;
                if !pending_passthroughs.is_empty() || !self.deferred_passthroughs.is_empty() {
                    // The replacement frame was published above, so
                    // this is the frame the queue must decide
                    // against.
                    let passthrough_frame = take_passthrough_frame_for_client(
                        &self.current_target,
                        &mut self.deferred_passthroughs,
                        &mut self.pane_refresh,
                        pending_generation,
                        pending_passthroughs,
                    );
                    emit_attach_bytes(&self.stream, &passthrough_frame).await?;
                }
                self.status_refresh.reschedule(
                    self.live_input
                        .handler
                        .attached_status_interval(&self.current_target.session_name)
                        .await,
                );
                if let Some(overlay) = pending_overlay {
                    update_persistent_overlay_cache(
                        &mut self.persistent_overlay,
                        &mut self.persistent_overlay_visible,
                        &overlay,
                    );
                }
                self.persistent_overlay_state_id = self.current_target.persistent_overlay_state_id;
                if let Some(barrier_state_id) = self.persistent_overlay_state_id {
                    discard_stale_persistent_overlays(
                        self.attach_controls.as_mut(),
                        &mut self.deferred_controls,
                        barrier_state_id,
                        &self.control_backlog,
                    );
                }
            }
            Some(AttachControl::AdvancePersistentOverlayState(state_id)) => {
                let previous_overlay_state_id = self.persistent_overlay_state_id;
                advance_persistent_overlay_state(
                    &mut self.persistent_overlay_state_id,
                    self.attach_controls.as_mut(),
                    &mut self.deferred_controls,
                    state_id,
                    &self.control_backlog,
                );
                redraw_after_persistent_overlay_state_advance(
                    &self.stream,
                    &self.current_target,
                    &mut self.persistent_overlay,
                    &mut self.persistent_overlay_visible,
                    previous_overlay_state_id,
                    self.persistent_overlay_state_id,
                    persistent_overlay_replacement_pending(
                        &self.deferred_controls,
                        self.persistent_overlay_state_id,
                    ),
                )
                .await?;
            }
            Some(AttachControl::Overlay(overlay)) => {
                if !accept_persistent_overlay_state(&mut self.persistent_overlay_state_id, &overlay)
                {
                    return Ok(AttachStep::Continue);
                }
                let persistent_clear = overlay.persistent && overlay.frame.is_empty();
                if persistent_clear
                    || should_emit_overlay(
                        self.render_generation,
                        &mut self.overlay_generation,
                        &overlay,
                    )
                {
                    let Some(frame) = cache_and_choose_overlay_frame(
                        &mut self.persistent_overlay,
                        &mut self.persistent_overlay_visible,
                        &overlay,
                        persistent_clear,
                        &self.deferred_controls,
                        self.persistent_overlay_state_id,
                        &self.current_target,
                    ) else {
                        return Ok(AttachStep::Continue);
                    };
                    emit_render_frame(&self.stream, &self.current_target.outer_terminal, &frame)
                        .await?;
                    flush_deferred_passthroughs(
                        &self.stream,
                        &self.current_target,
                        &mut self.deferred_passthroughs,
                        &mut self.pane_refresh,
                        self.persistent_overlay_visible,
                        self.persistent_overlay.is_some(),
                    )
                    .await?;
                }
            }
            Some(AttachControl::CoalescedPopupOverlay(_)) => {
                unreachable!("received popup overlays are materialized before dispatch")
            }
            Some(AttachControl::Write(bytes)) => {
                emit_attach_bytes(&self.stream, &bytes).await?;
            }
            Some(AttachControl::ClipboardWrite { bytes, reservation }) => {
                emit_attach_bytes(&self.stream, &bytes).await?;
                drop(reservation);
            }
            Some(AttachControl::LockShellCommand(command)) => {
                PendingAttachInputState::new(
                    &mut self.pending_input,
                    &mut self.pending_escape_flush,
                )
                .clear();
                self.locked = true;
                emit_attach_stop(&self.stream, &self.current_target).await?;
                emit_attach_message(&self.stream, &AttachMessage::LockShellCommand(command))
                    .await?;
            }
            Some(AttachControl::Suspend) => {
                PendingAttachInputState::new(
                    &mut self.pending_input,
                    &mut self.pending_escape_flush,
                )
                .clear();
                self.locked = true;
                emit_attach_stop(&self.stream, &self.current_target).await?;
                emit_attach_message(&self.stream, &AttachMessage::Suspend).await?;
            }
            None => self.attach_controls = None,
        }
        Ok(AttachStep::Continue)
    }
}
