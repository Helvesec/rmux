use super::{
    drain_ready_pane_events, ensure_control_newline, io,
    reconcile_control_format_subscription_tick, reconcile_control_read_demand,
    refresh_subscription_for_pane, refresh_subscriptions, ControlFormatLimitExceeded,
    ControlPaneOutputAction, ControlPaneOutputRefresh, ControlServerEvent, PaneSubscriptionStart,
    PaneSubscriptionState, ServerEventContext,
};

#[cfg(any(unix, windows))]
pub(super) async fn handle_server_event(
    event: ControlServerEvent,
    context: &mut ServerEventContext<'_>,
    command_active: bool,
) -> io::Result<bool> {
    match event {
        ControlServerEvent::SessionChanged(next_session) => {
            if command_active {
                context.deferred.defer_session_change(next_session, None);
                return Ok(false);
            }
            *context.session_name = next_session;
            refresh_subscriptions(
                context.handler,
                context.control_identity,
                context.session_name.as_ref(),
                PaneSubscriptionState {
                    subscriptions: context.subscriptions,
                    reader_demands: context.reader_demands,
                    pane_output_states: context.pane_output_states,
                },
                context.pane_event_tx.clone(),
                PaneSubscriptionStart::Oldest,
            )
            .await;
            reconcile_control_format_subscription_tick(
                context.format_subscription_tick,
                context.format_subscriptions,
            );
        }
        ControlServerEvent::SessionChangedAt {
            session_name: next_session,
            pane_sequences,
        } => {
            if command_active {
                context
                    .deferred
                    .defer_session_change(Some(next_session), Some(pane_sequences));
                return Ok(false);
            }
            *context.session_name = Some(next_session);
            refresh_subscriptions(
                context.handler,
                context.control_identity,
                context.session_name.as_ref(),
                PaneSubscriptionState {
                    subscriptions: context.subscriptions,
                    reader_demands: context.reader_demands,
                    pane_output_states: context.pane_output_states,
                },
                context.pane_event_tx.clone(),
                PaneSubscriptionStart::Sequences(&pane_sequences),
            )
            .await;
            reconcile_control_format_subscription_tick(
                context.format_subscription_tick,
                context.format_subscriptions,
            );
        }
        ControlServerEvent::FormatSubscriptions(definitions) => {
            if let Err(error) = context.format_subscriptions.apply_definitions(definitions) {
                return close_control_for_format_subscription_limit(context, command_active, error)
                    .await;
            }
            reconcile_control_format_subscription_tick(
                context.format_subscription_tick,
                context.format_subscriptions,
            );
        }
        ControlServerEvent::PaneOutputActions(actions) => {
            apply_control_pane_output_actions(actions, context).await;
            context.flush_output().await?;
        }
        ControlServerEvent::Refresh => {
            refresh_subscriptions(
                context.handler,
                context.control_identity,
                context.session_name.as_ref(),
                PaneSubscriptionState {
                    subscriptions: context.subscriptions,
                    reader_demands: context.reader_demands,
                    pane_output_states: context.pane_output_states,
                },
                context.pane_event_tx.clone(),
                PaneSubscriptionStart::Oldest,
            )
            .await;
            reconcile_control_format_subscription_tick(
                context.format_subscription_tick,
                context.format_subscriptions,
            );
        }
        ControlServerEvent::Notification(line) => {
            if command_active || context.deferred.exit_reason.is_some() {
                context.deferred.defer_notification(line);
                return Ok(false);
            }
            if write_control_notification(line, context).await? {
                return Ok(true);
            }
        }
        ControlServerEvent::Exit(reason) => {
            if command_active || !context.deferred.notifications.is_empty() {
                context.deferred.exit_reason = Some(reason);
                return Ok(false);
            }
            context.output_queue.enqueue_exit(reason.as_deref());
            context.flush_output().await?;
            return Ok(true);
        }
    }

    refresh_control_read_context(context).await;
    Ok(false)
}

#[cfg(any(unix, windows))]
pub(super) async fn refresh_control_read_context(context: &mut ServerEventContext<'_>) {
    *context.flags = context
        .handler
        .control_client_flags(context.requester_pid)
        .await
        .unwrap_or(*context.flags);
    #[cfg(unix)]
    context.pane_event_tx.set_pause_after(*context.flags);
    reconcile_control_read_demand(
        context.handler,
        context.session_name.as_ref(),
        *context.flags,
        context.pane_output_states,
        context.unrestricted_read_demand,
        context.attached_client_read_presence,
    )
    .await;
}

#[cfg(any(unix, windows))]
pub(super) async fn apply_control_pane_output_actions(
    actions: Vec<ControlPaneOutputAction>,
    context: &mut ServerEventContext<'_>,
) {
    let refreshes = context
        .pane_output_states
        .apply_actions(&actions, context.output_queue);
    for (action, refresh) in actions.into_iter().zip(refreshes) {
        let start = match refresh {
            ControlPaneOutputRefresh::Now | ControlPaneOutputRefresh::None => {
                PaneSubscriptionStart::Now
            }
        };
        refresh_subscription_for_pane(
            context.handler,
            context.control_identity,
            context.session_name.as_ref(),
            PaneSubscriptionState {
                subscriptions: context.subscriptions,
                reader_demands: context.reader_demands,
                pane_output_states: context.pane_output_states,
            },
            context.pane_event_tx.clone(),
            action.pane_id(),
            start,
        )
        .await;
        // Install the output cursor before releasing OFF's global read block.
        // Otherwise a reader can publish unread PTY bytes during the lookup
        // above and a new `Now` cursor will skip them.
        if let Some(attached_client_read_presence) = context.attached_client_read_presence.as_ref()
        {
            attached_client_read_presence.set_pane_off(
                action.pane_id(),
                context.pane_output_states.blocks_reading(action.pane_id()),
            );
        }
    }
}

#[cfg(any(unix, windows))]
pub(super) async fn write_changed_format_subscription_notifications(
    context: &mut ServerEventContext<'_>,
) -> io::Result<bool> {
    let notifications = match context
        .handler
        .changed_control_format_subscription_notifications_for_identity(
            context.control_identity,
            context.session_name.as_ref(),
            context.format_subscriptions,
        )
        .await
    {
        Ok(notifications) => notifications,
        Err(error) => {
            return close_control_for_format_subscription_limit(context, false, error).await;
        }
    };
    for notification in notifications.into_notifications() {
        if write_control_notification(notification.into_line(), context).await? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(any(unix, windows))]
pub(super) async fn close_control_for_format_subscription_limit(
    context: &mut ServerEventContext<'_>,
    command_active: bool,
    error: ControlFormatLimitExceeded,
) -> io::Result<bool> {
    let reason = error.into_message();
    if command_active {
        context.deferred.defer_limit_exit(reason);
        return Ok(false);
    }
    context.output_queue.enqueue_exit(Some(&reason));
    context.flush_output().await?;
    Ok(true)
}

#[cfg(any(unix, windows))]
pub(super) async fn write_control_notification(
    line: String,
    context: &mut ServerEventContext<'_>,
) -> io::Result<bool> {
    let line = context
        .handler
        .refresh_control_window_notification(context.control_identity, line)
        .await;
    if !context.output_queue.response_open
        && drain_ready_pane_events(
            context.pane_event_rx,
            &context.pane_event_tx,
            context.output_queue,
            context.paused_panes,
            context.pane_output_states,
            *context.flags,
        )
        .await?
    {
        context.flush_output().await?;
        return Ok(true);
    }
    context
        .output_queue
        .enqueue_line(ensure_control_newline(line.into_bytes()), false);
    context.flush_output().await?;
    Ok(false)
}
