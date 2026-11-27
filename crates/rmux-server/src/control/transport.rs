#[cfg(windows)]
use super::consume_control_eof_marker;
use super::transport_state::ControlTransport;
use super::{
    acquire_control_eof_queue_lease, append_control_input_with_rejection,
    arm_control_eof_transition, control_control_waits_for_attached_session,
    flush_deferred_server_events, handle_control_command_response_event, handle_pane_event,
    handle_server_event, io, reconcile_control_read_demand, reconcile_ready_server_events,
    refresh_subscriptions, reset_control_format_subscription_tick_after_evaluation,
    wait_for_control_eof_transition, wait_for_control_format_subscription_tick,
    write_changed_format_subscription_notifications, Ordering, PaneSubscriptionStart,
    PaneSubscriptionState,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

impl ControlTransport {
    pub(super) async fn flush_output(&mut self) -> io::Result<()> {
        self.events
            .context(
                &self.handler,
                self.control_identity,
                &mut self.write_half,
                (&self.pane_event_tx, &mut self.pane_event_rx),
            )
            .flush_output()
            .await
    }

    pub(super) async fn run(mut self) -> io::Result<()> {
        #[cfg(unix)]
        self.pane_event_tx.set_pause_after(self.events.flags);
        refresh_subscriptions(
            &self.handler,
            self.control_identity,
            self.events.attachment.session_name(),
            PaneSubscriptionState {
                subscriptions: &mut self.events.subscriptions,
                reader_demands: &mut self.events.reader_demands,
                pane_output_states: &mut self.events.pane_output_states,
            },
            self.pane_event_tx.clone(),
            PaneSubscriptionStart::Now,
        )
        .await;
        reconcile_control_read_demand(
            &self.handler,
            self.events.attachment.session_name(),
            self.events.flags,
            &mut self.events.pane_output_states,
            &mut self.events.unrestricted_read_demand,
            &mut self.events.attached_client_read_presence,
        )
        .await;
        while let Ok(event) = self.server_events.try_recv() {
            let mut event_context = self.events.context(
                &self.handler,
                self.control_identity,
                &mut self.write_half,
                (&self.pane_event_tx, &mut self.pane_event_rx),
            );
            if handle_server_event(event, &mut event_context, false).await? {
                if !self.events.output_queue.is_transport_closed() {
                    self.transport_drain
                        .finish(&self.handler, self.control_identity)
                        .await?;
                }
                self.write_half.finish(0).await?;
                return Ok(());
            }
        }

        let transport_result: io::Result<()> = 'control_transport: loop {
            if !self.shutdown_draining && self.shutdown.has_changed().unwrap_or(true) {
                self.shutdown_draining = true;
                self.input_closed = true;
                self.input_buffer.clear();
                self.queued_lines.clear();
                self.queued_input_bytes = 0;
                self.eof_transition = None;
                if let Some(command) = self.current_command.as_ref() {
                    command.eof_cancellation.cancel_for_eof();
                }
            }
            if self.current_command.is_none() {
                let reconcile_ready_control_events = self.initial_command_completion_pending
                    || (self.input_closed
                        && !control_control_waits_for_attached_session(
                            self.mode,
                            &self.events.attachment,
                        ));
                let mut event_context = self.events.context(
                    &self.handler,
                    self.control_identity,
                    &mut self.write_half,
                    (&self.pane_event_tx, &mut self.pane_event_rx),
                );
                if flush_deferred_server_events(&mut event_context).await? {
                    break 'control_transport Ok(());
                }
                if reconcile_ready_control_events {
                    // A successful attach publishes its session-change event before
                    // the command task returns. When completion and both futures are
                    // ready, the biased select below deliberately observes command
                    // completion first. Reconcile the bounded snapshot of events
                    // already published before deciding whether the completed CLI
                    // batch is unattached or treating an EOF-bound client as
                    // terminal. New arrivals are left to the select, so a continuous
                    // producer cannot starve either terminal decision.
                    if reconcile_ready_server_events(&mut self.server_events, &mut event_context)
                        .await?
                    {
                        break 'control_transport Ok(());
                    }
                }
            }
            self.eof_completion.observe_attachment(
                self.events.attachment.is_attached(),
                self.current_command.is_some() || !self.queued_lines.is_empty(),
            );
            if self.events.output_queue.is_transport_closed()
                && self.input_closed
                && self.current_command.is_none()
                && self.queued_lines.is_empty()
            {
                // A terminal write failure makes an already-closed input queue
                // reclaimable, including persistent attached `-CC`. It does not
                // itself prove read EOF: complete frames may still be buffered in
                // the pipe, so open input must keep draining until the read side
                // independently closes.
                break 'control_transport Ok(());
            }
            if self.shutdown_draining && self.current_command.is_none() {
                // tmux 3.7b ends a control client of a server that shuts down
                // (SIGTERM, SIGINT, kill-server) with a bare `%exit`, after the
                // `%sessions-changed` of every session the exit closed. Those
                // notifications are queued before the shutdown watch fires.
                let mut event_context = self.events.context(
                    &self.handler,
                    self.control_identity,
                    &mut self.write_half,
                    (&self.pane_event_tx, &mut self.pane_event_rx),
                );
                if reconcile_ready_server_events(&mut self.server_events, &mut event_context)
                    .await?
                {
                    break 'control_transport Ok(());
                }
                self.events.output_queue.enqueue_exit(None);
                self.flush_output().await?;
                break 'control_transport Ok(());
            }
            if self.lifecycle.closing.load(Ordering::SeqCst) && self.current_command.is_none() {
                // A completed destruction command closes the registration after
                // publishing its notifications. Drain that finite FIFO before Exit.
                let mut event_context = self.events.context(
                    &self.handler,
                    self.control_identity,
                    &mut self.write_half,
                    (&self.pane_event_tx, &mut self.pane_event_rx),
                );
                if reconcile_ready_server_events(&mut self.server_events, &mut event_context)
                    .await?
                {
                    break 'control_transport Ok(());
                }
                self.events.output_queue.enqueue_exit(None);
                self.flush_output().await?;
                break 'control_transport Ok(());
            }
            if self.initial_command_completion_pending && self.current_command.is_none() {
                self.initial_command_completion_pending = false;
                if !self.events.attachment.is_attached() {
                    self.normal_exit = true;
                    self.events.output_queue.enqueue_exit(None);
                    self.flush_output().await?;
                    self.shutdown_write_half_after_drain = true;
                    break 'control_transport Ok(());
                }
            }
            if !self.shutdown_draining
                && self.input_closed
                && self.current_command.is_none()
                && self.queued_lines.is_empty()
                && !control_control_waits_for_attached_session(self.mode, &self.events.attachment)
            {
                // Any incomplete line remaining in input_buffer after EOF is
                // discarded, matching tmux's `evbuffer_readln` semantics. EOF
                // itself is promoted to a bare `%exit\n` so the control-mode
                // transcript is terminated by a guard-tuple-free exit line,
                // matching tmux's `server_client_control_mode` close path.
                self.events.output_queue.enqueue_exit(None);
                self.flush_output().await?;
                break 'control_transport Ok(());
            }

            if self.start_queued_commands().await? {
                break 'control_transport Ok(());
            }
            if self.initial_command_completion_pending && self.current_command.is_none() {
                continue;
            }
            if self.shutdown_draining && self.current_command.is_none() {
                continue;
            }
            if !self.shutdown_draining
                && self.input_closed
                && self.current_command.is_none()
                && self.queued_lines.is_empty()
                && !control_control_waits_for_attached_session(self.mode, &self.events.attachment)
            {
                self.events.output_queue.enqueue_exit(None);
                self.flush_output().await?;
                break 'control_transport Ok(());
            }

            tokio::select! {
                biased;

                result = self.shutdown.changed(), if !self.shutdown_draining => {
                    let _ = result;
                    self.shutdown_draining = true;
                    self.input_closed = true;
                    self.input_buffer.clear();
                    self.queued_lines.clear();
                    self.queued_input_bytes = 0;
                    self.eof_transition = None;
                    if let Some(command) = self.current_command.as_ref() {
                        command.eof_cancellation.cancel_for_eof();
                    }
                }
                result = self.read_half.read(&mut self.read_buffer), if !self.input_closed && !self.shutdown_draining => {
                    let bytes_read = result?;
                    if bytes_read == 0 {
                        self.input_closed = true;
                        acquire_control_eof_queue_lease(
                            &mut self.eof_queue_lease,
                            &self.handler,
                            self.control_identity,
                        )
                        .await;
                        if let Some(command) = self.current_command.as_ref() {
                            command.eof_cancellation.cancel_for_eof();
                        }
                        #[cfg(windows)]
                        self.eof_completion.observe_transport_eof(
                            self.events.attachment.is_attached(),
                            self.current_command.is_some() || !self.queued_lines.is_empty(),
                        );
                        arm_control_eof_transition(&mut self.eof_transition);
                    } else {
                        append_control_input_with_rejection(
                            &mut self.input_buffer,
                            &mut self.queued_lines,
                            &mut self.queued_input_bytes,
                            &self.read_buffer[..bytes_read],
                            &mut self.write_half,
                        ).await?;
                        #[cfg(windows)]
                        if consume_control_eof_marker(
                            &mut self.input_buffer,
                            &mut self.queued_lines,
                            &mut self.queued_input_bytes,
                        ) {
                            self.input_closed = true;
                            acquire_control_eof_queue_lease(
                                &mut self.eof_queue_lease,
                                &self.handler,
                                self.control_identity,
                            )
                            .await;
                            if let Some(command) = self.current_command.as_ref() {
                                command.eof_cancellation.cancel_for_eof();
                            }
                            self.eof_completion.observe_stdin_eof_marker(
                                self.events.attachment.is_attached(),
                                self.current_command.is_some() || !self.queued_lines.is_empty(),
                            );
                            arm_control_eof_transition(&mut self.eof_transition);
                        }
                    }
                }
                Some(event) = async {
                    match self.current_response_notifications.as_mut() {
                        Some(notifications) => notifications.recv().await,
                        None => std::future::pending().await,
                    }
                }, if self.current_command.is_some() => {
                    let mut event_context = self.events.context(
                        &self.handler, self.control_identity, &mut self.write_half,
                        (&self.pane_event_tx, &mut self.pane_event_rx),
                    );
                    let command = self.current_command
                        .as_mut()
                        .expect("response events belong to an active command");
                    let response_frame = self.current_response_frame
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
                        break 'control_transport Ok(());
                    }
                }
                result = async {
                    match self.current_command.as_mut() {
                        Some(command) => match command.task.as_mut() {
                            Some(task) => Some(task.await),
                            None => std::future::pending().await,
                        },
                        None => std::future::pending().await,
                    }
                } => {
                    let Some(task_result) = result else {
                        continue;
                    };
                    if self.finish_command(task_result).await? {
                        break 'control_transport Ok(());
                    }
                }
                _ = wait_for_control_eof_transition(&mut self.eof_transition),
                    if !self.shutdown_draining
                        && self.input_closed
                        && self.current_command.is_some()
                        && !self.eof_command_ready
                        && !self.mode.is_control_control()
                        && self.eof_completion.allows_detached_drain_transition() =>
                {
                    return self.finish_eof().await;
                }
                _ = wait_for_control_format_subscription_tick(&mut self.events.format_subscription_tick),
                    if !self.shutdown_draining && self.current_command.is_none() =>
                {
                    let mut event_context = self.events.context(
                        &self.handler, self.control_identity, &mut self.write_half,
                        (&self.pane_event_tx, &mut self.pane_event_rx),
                    );
                    // A completed command may have already acknowledged
                    // refresh-client -B or a session mutation while its
                    // ControlServerEvent is still queued for this transport. If an
                    // overdue tick wins the next biased select, reconcile the
                    // bounded ready snapshot first so the tick observes the
                    // current local registry and session without starving later
                    // ticks under a continuous event producer.
                    if reconcile_ready_server_events(&mut self.server_events, &mut event_context)
                        .await?
                    {
                        break 'control_transport Ok(());
                    }
                    if write_changed_format_subscription_notifications(&mut event_context).await? {
                        break 'control_transport Ok(());
                    }
                    reset_control_format_subscription_tick_after_evaluation(
                        event_context.format_subscription_tick,
                        event_context.format_subscriptions,
                    );
                }
                Some(event) = self.server_events.recv() => {
                    let mut event_context = self.events.context(
                        &self.handler, self.control_identity, &mut self.write_half,
                        (&self.pane_event_tx, &mut self.pane_event_rx),
                    );
                    if handle_server_event(event, &mut event_context, self.current_command.is_some()).await?
                    {
                        break 'control_transport Ok(());
                    }
                }
                Some(event) = self.pane_event_rx.recv(), if !self.events.output_queue.response_open => {
                    let pane_id = event.pane_id();
                    let lagged =
                        handle_pane_event(
                            event,
                            &mut self.events.output_queue,
                            &mut self.events.paused_panes,
                            &mut self.events.pane_output_states,
                            self.events.flags,
                        )?;
                    if self.events.pane_output_states.is_paused(pane_id) {
                        refresh_subscriptions(
                            &self.handler,
                            self.control_identity,
                            self.events.attachment.session_name(),
                            PaneSubscriptionState {
                                subscriptions: &mut self.events.subscriptions,
                                reader_demands: &mut self.events.reader_demands,
                                pane_output_states: &mut self.events.pane_output_states,
                            },
                            self.pane_event_tx.clone(),
                            PaneSubscriptionStart::Now,
                        )
                        .await;
                    }
                    self.flush_output().await?;
                    if lagged {
                        break 'control_transport Ok(());
                    }
                }
            }
        };
        if transport_result.is_ok() && !self.events.output_queue.is_transport_closed() {
            let status = if self.normal_exit {
                self.exit_status
            } else {
                0
            };
            self.write_half.finish(status).await?;
            self.transport_drain
                .finish(&self.handler, self.control_identity)
                .await?;
        }
        if self.shutdown_write_half_after_drain {
            self.write_half.shutdown().await?;
        }
        transport_result
    }
}
