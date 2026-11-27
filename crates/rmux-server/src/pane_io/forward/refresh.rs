use super::*;
impl AttachForwarder {
    pub(super) async fn refresh(&mut self) -> io::Result<AttachStep> {
        if attach_shutdown_observable(&self.shutdown) {
            return Ok(AttachStep::Continue);
        }
        let Some(_control_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        self.pane_refresh.clear();
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
            PendingAttachAction::Write => {
                if self.locked {
                    return Ok(AttachStep::Continue);
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
                // Popups/menus own the visible surface until dismissal.
                // A timer queued before the overlay opened must not paint
                // the base pane underneath it and then restore the overlay.
                if self
                    .persistent_overlay
                    .as_ref()
                    .is_some_and(|overlay| overlay.defer_pane_updates)
                {
                    self.pane_refresh_requires_full = true;
                    return Ok(AttachStep::Continue);
                }
                let force_full_refresh = self.pane_refresh_requires_full
                    || self.persistent_overlay_visible
                    || self.persistent_overlay.is_some()
                    || self.current_target.live_pane.is_none();
                self.pane_refresh_requires_full = false;
                if force_full_refresh {
                    refresh_current_attach_client(&self.live_input).await;
                } else {
                    let pending_output =
                        collect_pending_attach_output_batch_metadata(&mut self.current_target);
                    let mut drained_sustained_output = false;
                    let mut live_generation = PaneOutputGeneration::unsubscribed();
                    let mut live_passthroughs = Vec::new();
                    if let Some(batch) = pending_output {
                        match batch {
                            AttachOutputBatch::Closed => {
                                self.current_target.pane_output = None;
                            }
                            AttachOutputBatch::Gap => {
                                self.pane_refresh_requires_full = true;
                                self.pane_refresh.schedule_now();
                                return Ok(AttachStep::Continue);
                            }
                            AttachOutputBatch::Events {
                                generation,
                                bytes: _,
                                passthroughs,
                                close_after_render,
                                sustained,
                                ..
                            } => {
                                drained_sustained_output = sustained;
                                live_generation = generation;
                                live_passthroughs = passthroughs;
                                if close_after_render {
                                    self.close_pane_output_after_refresh = true;
                                }
                            }
                        }
                    }
                    let replaceable_render = self.current_target.render_stream
                        && !drained_sustained_output
                        && !self.pane_refresh.is_sustained();
                    // Render first: the passthrough frame is built
                    // against whatever this render just published,
                    // so a positioned payload is held whenever that
                    // was still the frozen presentation screen.
                    let render_delta = self
                        .current_target
                        .live_pane
                        .as_mut()
                        .map(|pane| pane.render_frame_from_transcript(replaceable_render));
                    let passthrough_frame = take_passthrough_frame_for_client(
                        &self.current_target,
                        &mut self.deferred_passthroughs,
                        &mut self.pane_refresh,
                        live_generation,
                        live_passthroughs,
                    );
                    match render_delta {
                        Some(PaneRenderDelta::Incremental(delta)) => {
                            emit_live_render_frame(
                                &self.stream,
                                &mut self.current_target,
                                &delta,
                                replaceable_render,
                            )
                            .await?;
                            emit_attach_bytes(&self.stream, &passthrough_frame).await?;
                        }
                        Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                            refresh_current_attach_client(&self.live_input).await;
                            emit_attach_bytes(&self.stream, &passthrough_frame).await?;
                        }
                    }
                    let _ = self
                        .pane_refresh
                        .note_output_batch(drained_sustained_output);
                }
                if self.close_pane_output_after_refresh {
                    self.current_target.pane_output = None;
                    self.close_pane_output_after_refresh = false;
                }
            }
        }
        Ok(AttachStep::Continue)
    }
}
