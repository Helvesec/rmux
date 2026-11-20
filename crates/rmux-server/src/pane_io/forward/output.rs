use super::*;
impl AttachForwarder {
    pub(super) async fn output(
        &mut self,
        result: io::Result<Option<types::DrainedPaneOutput>>,
    ) -> io::Result<AttachStep> {
        let Some(item) = result? else {
            self.current_target.pane_output = None;
            return Ok(AttachStep::Continue);
        };
        let Some(_output_batch) = begin_attach_mutation_batch(&self.live_input, &self.shutdown)
        else {
            self.shutdown_pending_output_batch = Some(collect_attach_output_batch(
                item,
                self.current_target.pane_output.as_mut(),
            ));
            self.shutdown_draining = true;
            return Ok(AttachStep::Continue);
        };
        #[cfg(unix)]
        if let rmux_core::events::OutputCursorItem::Event(event) = &item.item {
            if event.passthroughs().is_empty() {
                match consume_predicted_echo(&mut self.current_target, event.bytes()) {
                    PredictedEcho::Consumed => return Ok(AttachStep::Continue),
                    PredictedEcho::Mismatch => {
                        self.pane_refresh_requires_full = true;
                        self.pane_refresh.schedule_immediate();
                    }
                    PredictedEcho::NoPrediction => {}
                }
            }
        }
        let item = if self.deferred_controls.is_empty()
            && self.control_backlog.load(Ordering::Acquire) == 0
            && !self.locked
            && !self.closing.load(Ordering::SeqCst)
            && !self.persistent_overlay_visible
            && self.persistent_overlay.is_none()
            && self.deferred_passthroughs.is_empty()
            && should_treat_attach_output_as_interactive(self.last_client_input_at)
        {
            let types::DrainedPaneOutput { generation, item } = item;
            let item =
                match item {
                    rmux_core::events::OutputCursorItem::Event(event)
                        if !event.is_empty()
                            && event.byte_len() <= 512
                            && event.passthroughs().is_empty()
                            && self.pane_refresh.can_bypass_small_plain_output()
                            && self.current_target.live_pane.as_ref().is_some_and(|pane| {
                                pane.can_forward_plain_bytes(event.bytes())
                            }) =>
                    {
                        let snapshot_synced =
                            self.current_target.live_pane.as_mut().is_some_and(|pane| {
                                pane.apply_forwarded_plain_bytes(event.bytes())
                            });
                        if snapshot_synced {
                            emit_attach_bytes(&self.stream, event.bytes()).await?;
                            let _ = self.pane_refresh.note_output_batch(false);
                            return Ok(AttachStep::Continue);
                        }
                        rmux_core::events::OutputCursorItem::Event(event)
                    }
                    other => other,
                };
            types::DrainedPaneOutput { generation, item }
        } else {
            item
        };
        let pending_output_batch =
            match collect_attach_output_batch(item, self.current_target.pane_output.as_mut()) {
                AttachOutputBatch::Closed => {
                    self.current_target.pane_output = None;
                    return Ok(AttachStep::Continue);
                }
                AttachOutputBatch::Gap => {
                    if self
                        .persistent_overlay
                        .as_ref()
                        .is_some_and(|overlay| overlay.defer_pane_updates)
                    {
                        self.pane_refresh_requires_full = true;
                        return Ok(AttachStep::Continue);
                    }
                    if self.current_target.live_pane.is_none()
                        || self.persistent_overlay_visible
                        || self.persistent_overlay.is_some()
                    {
                        self.pane_refresh_requires_full = true;
                    }
                    self.pane_refresh.schedule_sustained();
                    return Ok(AttachStep::Continue);
                }
                batch @ AttachOutputBatch::Events { .. } => batch,
            };
        #[cfg(test)]
        pause_after_pane_output_batch_drain(&self.current_target.session_name).await;
        match self.apply_pending_controls().await? {
            PendingAttachAction::Exit(PendingAttachExit {
                reason,
                drop_pending_output,
                snapshot_covered_output_before_sequence,
            }) => {
                // The receiver cursor already owns this batch. Hand it to the
                // terminal exit drain unless an earlier control deliberately
                // invalidated output from the old attach target.
                finish_pending_attach_exit_with_batch(
                    reason,
                    &self.stream,
                    &mut self.current_target,
                    &mut self.deferred_passthroughs,
                    pending_attach_exit_output_batch(
                        drop_pending_output,
                        snapshot_covered_output_before_sequence,
                        pending_output_batch,
                    ),
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
                let AttachOutputBatch::Events {
                    generation,
                    passthroughs,
                    close_after_render,
                    ..
                } = pending_output_batch
                else {
                    unreachable!("closed and gap batches return before control dispatch")
                };
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
                if source_invalidated {
                    defer_passthroughs_after_source_invalidation(
                        &mut self.deferred_passthroughs,
                        passthroughs,
                    );
                } else {
                    defer_passthroughs(&mut self.deferred_passthroughs, generation, passthroughs);
                }
                schedule_attach_render_refresh(
                    &mut self.pane_refresh,
                    &mut self.pane_refresh_requires_full,
                    &self.live_input,
                )
                .await;
                if close_after_render && !source_invalidated {
                    self.current_target.pane_output = None;
                }
                return Ok(AttachStep::Continue);
            }
            PendingAttachAction::Write => {
                let AttachOutputBatch::Events {
                    generation,
                    bytes: raw_output_bytes,
                    passthroughs,
                    close_after_render,
                    sustained: sustained_output,
                    ..
                } = pending_output_batch
                else {
                    unreachable!("closed and gap batches return before control dispatch")
                };
                if self.locked {
                    if close_after_render {
                        self.current_target.pane_output = None;
                    }
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
                if self.persistent_overlay_visible || self.persistent_overlay.is_some() {
                    defer_passthroughs(&mut self.deferred_passthroughs, generation, passthroughs);
                    self.pane_refresh_requires_full = true;
                    if !self
                        .persistent_overlay
                        .as_ref()
                        .is_some_and(|overlay| overlay.defer_pane_updates)
                    {
                        self.pane_refresh.schedule_now();
                    }
                    if close_after_render {
                        self.current_target.pane_output = None;
                    }
                    return Ok(AttachStep::Continue);
                }
                // A non-empty queue owns work that only the
                // render-then-emit path below can discharge, so no
                // bypass may run while it holds anything: the
                // release chunk that converges the screen is
                // exactly the event that must flush it.
                if passthroughs.is_empty()
                    && self.deferred_passthroughs.is_empty()
                    && self.current_target.live_pane.is_some()
                {
                    let interactive_output =
                        should_treat_attach_output_as_interactive(self.last_client_input_at);
                    let small_plain_output = raw_output_bytes.len() <= 512;
                    let output_can_bypass_render =
                        small_plain_output && self.pane_refresh.can_bypass_small_plain_output();
                    if !sustained_output
                        && output_can_bypass_render
                        && try_forward_plain_output(
                            &self.stream,
                            &mut self.current_target,
                            &raw_output_bytes,
                        )
                        .await?
                    {
                        let _ = self.pane_refresh.note_output_batch(sustained_output);
                        if close_after_render {
                            self.current_target.pane_output = None;
                        }
                        return Ok(AttachStep::Continue);
                    }
                    if interactive_output {
                        self.pane_refresh.note_interactive_output();
                        match self
                            .current_target
                            .live_pane
                            .as_mut()
                            .map(|pane| pane.render_interactive_frame_from_transcript())
                        {
                            Some(PaneRenderDelta::Incremental(delta)) => {
                                emit_live_render_frame(
                                    &self.stream,
                                    &mut self.current_target,
                                    &delta,
                                    false,
                                )
                                .await?;
                                if close_after_render {
                                    self.current_target.pane_output = None;
                                }
                                return Ok(AttachStep::Continue);
                            }
                            Some(PaneRenderDelta::RequiresFullRefresh) | None => {
                                self.pane_refresh_requires_full = true;
                                self.pane_refresh.schedule_immediate();
                            }
                        }
                    } else if self.pane_refresh.note_output_batch(sustained_output) {
                        self.pane_refresh.schedule_sustained();
                    } else {
                        self.pane_refresh.schedule_now();
                    }
                    if close_after_render {
                        self.close_pane_output_after_refresh = true;
                    }
                    return Ok(AttachStep::Continue);
                }
                let replaceable_render = self.current_target.render_stream;
                // Render first: the passthrough frame is built
                // against whatever this render just published, so a
                // positioned payload is held whenever that was
                // still the frozen presentation screen.
                let render_delta = self
                    .current_target
                    .live_pane
                    .as_mut()
                    .map(|pane| pane.render_frame_from_transcript(replaceable_render));
                let passthrough_frame = take_passthrough_frame_for_client(
                    &self.current_target,
                    &mut self.deferred_passthroughs,
                    &mut self.pane_refresh,
                    generation,
                    passthroughs,
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
                        self.pane_refresh_requires_full = true;
                        self.pane_refresh.schedule_now();
                        emit_attach_bytes(&self.stream, &passthrough_frame).await?;
                    }
                }
                if close_after_render {
                    self.current_target.pane_output = None;
                }
            }
        }
        Ok(AttachStep::Continue)
    }
}
