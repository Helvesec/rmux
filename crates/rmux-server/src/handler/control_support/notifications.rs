use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn refresh_control_session(
        &self,
        session_name: &rmux_proto::SessionName,
    ) {
        let mut active_control = self.active_control.lock().await;
        for active in active_control.by_pid.values_mut() {
            if active.session_name.as_ref() != Some(session_name) {
                continue;
            }
            let _ = try_send_control_event(active, ControlServerEvent::Refresh);
        }
    }

    pub(in crate::handler) async fn refresh_control_session_for_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        let current = {
            let state = self.state.lock().await;
            state
                .sessions
                .session(session_name)
                .is_some_and(|session| session.id() == session_id)
        };
        if !current {
            return;
        }
        let mut active_control = self.active_control.lock().await;
        for active in active_control.by_pid.values_mut() {
            if active.session_name.as_ref() != Some(session_name)
                || active.session_id != Some(session_id)
            {
                continue;
            }
            let _ = try_send_control_event(active, ControlServerEvent::Refresh);
        }
    }

    pub(in crate::handler) async fn refresh_control_client_for_identity(
        &self,
        identity: ControlClientIdentity,
    ) -> Result<(), rmux_proto::RmuxError> {
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control
            .by_pid
            .get_mut(&identity.requester_pid())
            .filter(|active| {
                active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
            })
        else {
            return Err(attached_client_required("refresh-client"));
        };
        if !try_send_control_event(active, ControlServerEvent::Refresh) {
            return Err(attached_client_required("refresh-client"));
        }
        Ok(())
    }

    pub(in crate::handler) async fn apply_control_pane_output_actions_for_identity(
        &self,
        identity: ControlClientIdentity,
        actions: Vec<ControlPaneOutputAction>,
    ) -> Result<(), rmux_proto::RmuxError> {
        if actions.is_empty() {
            return Ok(());
        }
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control
            .by_pid
            .get_mut(&identity.requester_pid())
            .filter(|active| {
                active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
            })
        else {
            return Err(attached_client_required("refresh-client"));
        };
        let sent = if control_command_response_stream_is_active(identity) {
            send_control_command_response_event(
                identity,
                ControlCommandResponseEvent::PaneOutputActions(actions),
            )
        } else {
            try_send_control_event(active, ControlServerEvent::PaneOutputActions(actions))
        };
        if sent {
            Ok(())
        } else {
            Err(attached_client_required("refresh-client"))
        }
    }

    pub(in crate::handler) async fn apply_control_format_subscriptions_for_identity(
        &self,
        identity: ControlClientIdentity,
        definitions: Vec<String>,
    ) -> Result<(), rmux_proto::RmuxError> {
        if definitions.is_empty() {
            return Ok(());
        }
        validate_control_format_definition_batch(&definitions)
            .map_err(|error| rmux_proto::RmuxError::Server(error.into_message()))?;
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control
            .by_pid
            .get_mut(&identity.requester_pid())
            .filter(|active| {
                active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
            })
        else {
            return Err(attached_client_required("refresh-client"));
        };
        let mut projected = active.format_subscriptions.clone();
        projected
            .apply_definitions(definitions.clone())
            .map_err(|error| rmux_proto::RmuxError::Server(error.into_message()))?;
        if !try_send_control_event(active, ControlServerEvent::FormatSubscriptions(definitions)) {
            return Err(attached_client_required("refresh-client"));
        }
        active.format_subscriptions = projected;
        Ok(())
    }

    pub(crate) async fn changed_control_format_subscription_notifications(
        &self,
        session_name: Option<&rmux_proto::SessionName>,
        subscriptions: &mut ControlFormatSubscriptions,
    ) -> Result<ControlFormatNotificationBatch, ControlFormatLimitExceeded> {
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let state = self.state.lock().await;
        subscriptions.prepare_jobs(&self.socket_path(), state.status_jobs());
        subscriptions.changed_notifications_with_client_snapshots(
            &state,
            session_name,
            &client_loop_snapshots,
        )
    }

    pub(in crate::handler) async fn exit_control_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        reason: Option<String>,
    ) {
        let mut active_control = self.active_control.lock().await;
        for active in active_control.by_pid.values_mut() {
            if active.last_session.as_ref() == Some(session_name)
                && active.last_session_id == Some(session_id)
            {
                active.last_session = None;
                active.last_session_id = None;
            }
            if active.session_name.as_ref() != Some(session_name)
                || active.session_id != Some(session_id)
            {
                continue;
            }
            close_control_with_exit(active, reason.clone());
        }
    }

    pub(in crate::handler) async fn refresh_all_control_sessions(&self) {
        let session_names = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .values()
                .filter(|active| !active.closing.load(Ordering::SeqCst))
                .filter_map(|active| active.session_name.clone())
                .collect::<Vec<_>>()
        };

        for session_name in session_names {
            self.refresh_control_session(&session_name).await;
        }
    }

    pub(in crate::handler) async fn send_control_notification_to(
        &self,
        requester_pid: u32,
        line: String,
    ) {
        let mut active_control = self.active_control.lock().await;
        deliver_control_notification(&mut active_control, requester_pid, line);
    }

    pub(in crate::handler) async fn send_control_response_notification_to(
        &self,
        requester_pid: u32,
        line: String,
    ) {
        let mut active_control = self.active_control.lock().await;
        deliver_control_response_notification(&mut active_control, requester_pid, line);
    }

    pub(in crate::handler) async fn send_control_response_notification_to_queue(
        &self,
        identity: ControlClientIdentity,
        line: String,
    ) -> bool {
        let mut active_control = self.active_control.lock().await;
        if active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| {
                active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
            })
        {
            return deliver_control_response_notification(
                &mut active_control,
                identity.requester_pid(),
                line,
            );
        }
        false
    }

    pub(in crate::handler) async fn dispatch_control_notifications(
        &self,
        event: &QueuedLifecycleEvent,
    ) {
        let state = self.state.lock().await;
        let mut active_control = self.active_control.lock().await;
        if matches!(
            event.event,
            LifecycleEvent::WindowUnlinked { .. } | LifecycleEvent::SessionClosed { .. }
        ) {
            let live = state
                .sessions
                .iter()
                .flat_map(|session| session.windows().map(|(_, window)| window.id()))
                .collect();
            for active in active_control.by_pid.values_mut() {
                active.window_sizes.retain_live(&live);
            }
        }
        if let Some(notifications) = event.control_layout_notifications.as_ref() {
            window_notifications::deliver_control_layout_notifications(
                &mut active_control,
                notifications,
            );
            return;
        }
        let control_clients = control_clients_snapshot_locked(&state, &active_control);
        if control_clients.is_empty() {
            return;
        }

        let notifications = collect_control_notifications(
            &state,
            &event.event,
            event.control_session_identity,
            event.control_window_identity,
            &control_clients,
        );
        #[cfg(test)]
        self.pause_before_control_notification_delivery().await;
        for notification in notifications {
            deliver_control_notification(&mut active_control, notification.pid, notification.line);
        }
    }

    #[cfg(test)]
    pub(in crate::handler) fn install_control_notification_delivery_pause(
        &self,
    ) -> Arc<super::super::ControlNotificationDeliveryPause> {
        let pause = Arc::new(super::super::ControlNotificationDeliveryPause::default());
        *self
            .control_notification_delivery_pause
            .lock()
            .expect("control notification delivery pause") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    async fn pause_before_control_notification_delivery(&self) {
        let pause = self
            .control_notification_delivery_pause
            .lock()
            .expect("control notification delivery pause")
            .take();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }

    pub(in crate::handler) async fn refresh_control_sessions_for_event(
        &self,
        event: &LifecycleEvent,
    ) {
        match event {
            LifecycleEvent::PaneModeChanged { .. }
            | LifecycleEvent::WindowLayoutChanged { .. }
            | LifecycleEvent::WindowPaneChanged { .. }
            | LifecycleEvent::WindowUnlinked { .. }
            | LifecycleEvent::WindowLinked { .. }
            | LifecycleEvent::WindowRenamed { .. }
            | LifecycleEvent::ClientSessionChanged { .. }
            | LifecycleEvent::ClientResized { .. }
            | LifecycleEvent::ClientDetached { .. }
            | LifecycleEvent::SessionRenamed { .. }
            | LifecycleEvent::SessionCreated { .. }
            | LifecycleEvent::SessionWindowChanged { .. }
            | LifecycleEvent::PasteBufferChanged { .. }
            | LifecycleEvent::PasteBufferDeleted { .. } => {
                self.refresh_all_control_sessions().await;
            }
            LifecycleEvent::SessionClosed {
                session_name,
                session_id,
            } => {
                if let Some(session_id) = session_id {
                    self.exit_control_session_identity(
                        session_name,
                        SessionId::new(*session_id),
                        None,
                    )
                    .await;
                }
                self.refresh_all_control_sessions().await;
            }
            LifecycleEvent::ClientActive { .. }
            | LifecycleEvent::ClientAttached { .. }
            | LifecycleEvent::ClientFocusIn { .. }
            | LifecycleEvent::ClientFocusOut { .. }
            | LifecycleEvent::ClientLightTheme { .. }
            | LifecycleEvent::ClientDarkTheme { .. }
            | LifecycleEvent::AlertBell { .. }
            | LifecycleEvent::AlertActivity { .. }
            | LifecycleEvent::AlertSilence { .. }
            | LifecycleEvent::PaneExited { .. }
            | LifecycleEvent::PaneDied { .. }
            | LifecycleEvent::PaneFocusIn { .. }
            | LifecycleEvent::PaneFocusOut { .. }
            | LifecycleEvent::PaneSetClipboard { .. }
            | LifecycleEvent::PaneTitleChanged { .. }
            | LifecycleEvent::WindowResized { .. }
            | LifecycleEvent::AfterSelectWindow { .. }
            | LifecycleEvent::AfterSelectPane { .. }
            | LifecycleEvent::AfterSendKeys { .. }
            | LifecycleEvent::AfterSetOption { .. } => {}
        }
    }
}
