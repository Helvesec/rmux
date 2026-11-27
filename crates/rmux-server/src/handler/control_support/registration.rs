use super::*;

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn register_control_with_closing(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        event_tx: mpsc::Sender<ControlServerEvent>,
        closing: Arc<AtomicBool>,
    ) -> u64 {
        self.register_control_with_access(
            requester_pid,
            upgrade,
            ControlRegistration {
                event_tx,
                closing,
                uid: current_owner_uid(),
                user: self.server_owner_identity(),
                can_write: true,
            },
        )
        .await
        .expect("test control registration must not outlive the bounded queue drain")
    }

    #[cfg(test)]
    pub(crate) async fn register_control_with_access(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        registration: ControlRegistration,
    ) -> Result<u64, ControlRegistrationError> {
        self.register_control_with_access_timeout(
            requester_pid,
            upgrade,
            registration,
            None,
            CONTROL_QUEUE_DRAIN_REGISTRATION_TIMEOUT,
        )
        .await
    }

    pub(crate) async fn register_control_with_server_access(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        registration: ControlRegistration,
        admission: ServerAccessAdmission,
    ) -> Result<u64, ControlRegistrationError> {
        self.register_control_with_access_timeout(
            requester_pid,
            upgrade,
            registration,
            Some(admission),
            CONTROL_QUEUE_DRAIN_REGISTRATION_TIMEOUT,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_control_with_access_timeout_for_test(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        registration: ControlRegistration,
        drain_timeout: Duration,
    ) -> Result<u64, ControlRegistrationError> {
        self.register_control_with_access_timeout(
            requester_pid,
            upgrade,
            registration,
            None,
            drain_timeout,
        )
        .await
    }

    async fn register_control_with_access_timeout(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        registration: ControlRegistration,
        admission: Option<ServerAccessAdmission>,
        drain_timeout: Duration,
    ) -> Result<u64, ControlRegistrationError> {
        #[cfg(test)]
        if admission.is_some() {
            pause_before_access_registration(AccessRegistrationKind::Control, requester_pid).await;
        }
        let drain_deadline = tokio::time::Instant::now() + drain_timeout;
        let (control_id, replaced_session, detached_event) = loop {
            let activity_sequence = self.next_client_activity_sequence();
            // Session lifecycle events and the logical client replacement
            // share the established state -> active-control lock order. This
            // keeps the detach ticket at the exact replacement boundary.
            let mut state = self.state.lock().await;
            let mut active_control = self.active_control.lock().await;
            let drain_finished = active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| active.queue_draining)
                .map(|active| active.queue_drain_finished.subscribe());
            if let Some(mut drain_finished) = drain_finished {
                drop(active_control);
                drop(state);
                if tokio::time::timeout_at(
                    drain_deadline,
                    drain_finished.wait_for(|finished| *finished),
                )
                .await
                .is_err()
                {
                    return Err(ControlRegistrationError::QueueDrainTimedOut { requester_pid });
                }
                continue;
            }

            let (control_id, replaced_session, detached_event) = {
                // Keep the client-state lock across policy revalidation and
                // publication. ACL mutations commit to the store first, then take
                // this lock to update or disconnect every published control client.
                let server_access = admission.as_ref().map(|_| {
                    self.server_access
                        .lock()
                        .expect("server access mutex must not be poisoned")
                });
                let can_write = match (server_access.as_ref(), admission.as_ref()) {
                    (Some(server_access), Some(admission)) => server_access
                        .revalidate_admission(admission, &registration.user)
                        .ok_or(ControlRegistrationError::AccessRevoked)?
                        .can_write(),
                    (None, None) => registration.can_write,
                    _ => unreachable!("an admission and its policy guard are created together"),
                };

                let control_id = self.next_client_creation_sequence();
                let (queue_drain_finished, _queue_drain_pending) = watch::channel(false);
                let previous = active_control.by_pid.insert(
                    requester_pid,
                    ActiveControl {
                        _colour_presence: state
                            .control_colour_clients
                            .register(registration.closing.clone()),
                        id: control_id,
                        last_activity_sequence: activity_sequence,
                        activity_at: current_client_activity_timestamp(),
                        client_width: DEFAULT_CONTROL_CLIENT_WIDTH,
                        client_height: DEFAULT_CONTROL_CLIENT_HEIGHT,
                        size_declared: false,
                        window_sizes: ControlWindowSizes::default(),
                        size_sequence: 0,
                        session_name: None,
                        session_id: None,
                        last_session: None,
                        last_session_id: None,
                        flags: ControlClientFlags::default(),
                        uid: registration.uid,
                        user: registration.user,
                        can_write,
                        terminal_context: upgrade.terminal_context,
                        format_subscriptions: ControlFormatSubscriptions::default(),
                        event_tx: registration.event_tx,
                        closing: registration.closing,
                        client_detached_prepared: false,
                        queue_draining: false,
                        queue_drain_finished,
                    },
                );
                let (replaced_session, detached_event) = if let Some(previous) = previous {
                    let replaced_session = previous.session_name.clone().zip(previous.session_id);
                    let detached_event =
                        replaced_session
                            .as_ref()
                            .and_then(|(session_name, session_id)| {
                                (!previous.client_detached_prepared).then(|| {
                                    let mut event = super::super::prepare_lifecycle_event(
                                        &mut state,
                                        &LifecycleEvent::ClientDetached {
                                            session_name: session_name.clone(),
                                            client_name: Some(control_client_name(requester_pid)),
                                        },
                                    );
                                    event.control_session_identity = Some(*session_id);
                                    event
                                })
                            });
                    close_control_with_exit(&previous, None);
                    (replaced_session, detached_event)
                } else {
                    (None, None)
                };
                drop(server_access);
                (control_id, replaced_session, detached_event)
            };
            drop(active_control);
            drop(state);
            break (control_id, replaced_session, detached_event);
        };

        if let Some(event) = detached_event {
            self.emit_prepared(event).await;
        }
        if let Some(session_identity) = replaced_session {
            self.destroy_unattached_sessions(vec![session_identity])
                .await;
        }

        for line in self.take_startup_config_error_notifications().await {
            self.send_control_notification_to(requester_pid, line).await;
        }

        Ok(control_id)
    }

    pub(crate) async fn begin_control_queue_drain(
        &self,
        identity: ControlClientIdentity,
    ) -> ControlQueueDrainLease {
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get_mut(&identity.requester_pid()) else {
            return ControlQueueDrainLease::Unavailable;
        };
        if active.id != identity.control_id() {
            return ControlQueueDrainLease::Unavailable;
        }
        // The active frame may itself have marked the control client closing
        // immediately before EOF (for example by emitting Exit). Its exact
        // registration still owns all already-accepted frames until the
        // drain observes that Exit and stops.
        active.queue_draining = true;
        ControlQueueDrainLease::Acquired
    }

    pub(crate) async fn control_queue_identity_is_open(
        &self,
        identity: ControlClientIdentity,
    ) -> bool {
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| {
                active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
            })
    }

    pub(crate) async fn finish_control(&self, requester_pid: u32, control_id: u64) {
        let (removed, removed_session, detached_event) = {
            let mut state = self.state.lock().await;
            let mut active_control = self.active_control.lock().await;
            let removed_control = if active_control
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.id == control_id)
            {
                active_control.by_pid.remove(&requester_pid)
            } else {
                None
            };
            match removed_control {
                Some(removed) => {
                    removed.queue_drain_finished.send_replace(true);
                    let prepare_detached = !removed.client_detached_prepared;
                    let removed_session = removed.session_name.zip(removed.session_id);
                    let detached_event =
                        removed_session
                            .as_ref()
                            .and_then(|(session_name, session_id)| {
                                prepare_detached.then(|| {
                                    let mut event = super::super::prepare_lifecycle_event(
                                        &mut state,
                                        &LifecycleEvent::ClientDetached {
                                            session_name: session_name.clone(),
                                            client_name: Some(control_client_name(requester_pid)),
                                        },
                                    );
                                    event.control_session_identity = Some(*session_id);
                                    event
                                })
                            });
                    (true, removed_session, detached_event)
                }
                None => (false, None, None),
            }
        };
        if let Some(event) = detached_event {
            self.emit_prepared(event).await;
        }
        if let Some(session_identity) = removed_session {
            let _ = self
                .reconcile_attached_session_identity_size_and_emit(session_identity.1)
                .await;
            self.destroy_unattached_sessions(vec![session_identity])
                .await;
        }
        if removed {
            self.refresh_theme_sources().await;
            let _ = self.queue_shutdown_if_server_unattached().await;
            let _ = self.request_shutdown_if_pending();
        }
    }

    async fn take_startup_config_error_notifications(&self) -> Vec<String> {
        let mut errors = self.startup_config_errors.lock().await;
        errors
            .drain(..)
            .flat_map(|error| match error {
                rmux_proto::RmuxError::Server(message) => message
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(|line| format!("%config-error {line}"))
                    .collect::<Vec<_>>(),
                other => vec![format!("%config-error {other}")],
            })
            .collect()
    }
}
