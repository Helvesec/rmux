use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn dispatch_lifecycle_hook(
        &self,
        mut event: QueuedLifecycleEvent,
    ) {
        self.dispatch_lifecycle_control_effects_once(&mut event)
            .await;

        if event.hooks.is_empty() {
            return;
        }
        let historical_pane = matches!(event.event, LifecycleEvent::PaneFocusOut { .. });
        let current_target = if let Some(lease) = event.retained_current_target.clone() {
            let state = self.state.lock().await;
            match lease.resolve(&state) {
                _ if historical_pane => HookDispatchCurrentTarget::HistoricalPane(lease),
                LeaseResolution::Live(_) | LeaseResolution::Retired(_) => {
                    HookDispatchCurrentTarget::Retainable(lease)
                }
                LeaseResolution::Replaced => {
                    warn!(
                        hook = ?event.hook_name,
                        "skipping lifecycle hook dispatch because its retained target was replaced"
                    );
                    return;
                }
            }
        } else if let Some(identity) = event.stable_current_target_identity.clone() {
            if self.stable_hook_command_target(&identity).await.is_some() {
                HookDispatchCurrentTarget::Stable(identity)
            } else if lifecycle_event_allows_missing_stable_target(&event.event, &identity) {
                HookDispatchCurrentTarget::MissingStable
            } else {
                warn!(
                    hook = ?event.hook_name,
                    "skipping lifecycle hook dispatch because its stable target was replaced or removed"
                );
                return;
            }
        } else {
            HookDispatchCurrentTarget::Dynamic(self.lifecycle_dispatch_current_target(&event).await)
        };
        // tmux checks the notification's state once, when it inserts the hooks.
        let notification_state = {
            let state = self.state.lock().await;
            if event
                .notification_state
                .as_ref()
                .is_some_and(|identity| identity.holds_live_pane_state(&state))
            {
                NotificationState::Valid
            } else {
                NotificationState::Invalid
            }
        };

        self.execute_hook_dispatches(
            std::process::id(),
            event.hooks,
            current_target,
            event.formats,
            HookExecutionContext::lifecycle_with_notification_state(
                event.hook_name,
                notification_state,
            ),
            "lifecycle",
        )
        .await;
    }

    pub(super) async fn dispatch_lifecycle_control_effects(&self, event: &QueuedLifecycleEvent) {
        self.refresh_theme_sources().await;
        self.dispatch_control_notifications(event).await;
        // A server exit ends no control client per closed session: every client
        // still hears the later closes, then the shutdown drain's bare `%exit`.
        if !event.server_exit {
            self.refresh_control_sessions_for_event(&event.event).await;
        }
    }

    pub(super) async fn dispatch_lifecycle_control_effects_once(
        &self,
        event: &mut QueuedLifecycleEvent,
    ) {
        if event.control_effects_dispatched {
            return;
        }
        self.dispatch_lifecycle_control_effects(event).await;
        event.control_effects_dispatched = true;
    }
}

impl RequestHandler {
    pub(super) fn execute_hook_dispatches(
        &self,
        requester_pid: u32,
        hooks: Vec<HookDispatch>,
        current_target: HookDispatchCurrentTarget,
        formats: Vec<(String, String)>,
        execution: HookExecutionContext,
        source: &'static str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let hook_name = execution.hook();
            with_hook_execution(execution, formats, async {
                for hook in hooks {
                    let command_current_target = match &current_target {
                        HookDispatchCurrentTarget::Stable(identity) => {
                            let Some(target) = self.stable_hook_command_target(identity).await
                            else {
                                warn!(
                                    hook = ?hook_name,
                                    source,
                                    "stopping lifecycle hook dispatch because its stable target no longer exists"
                                );
                                break;
                            };
                            Some(target)
                        }
                        HookDispatchCurrentTarget::Retainable(lease) => {
                            let state = self.state.lock().await;
                            match lease.resolve(&state) {
                                LeaseResolution::Live(_) | LeaseResolution::Retired(_) => None,
                                LeaseResolution::Replaced => {
                                    warn!(
                                        hook = ?hook_name,
                                        source,
                                        "stopping lifecycle hook dispatch because its retained target was replaced"
                                    );
                                    break;
                                }
                            }
                        }
                        HookDispatchCurrentTarget::HistoricalPane(lease) => {
                            let state = self.state.lock().await;
                            match lease.resolve(&state) {
                                LeaseResolution::Live(target) => Some(target),
                                LeaseResolution::Retired(_) | LeaseResolution::Replaced => None,
                            }
                        }
                        HookDispatchCurrentTarget::ExactPane(identity) => {
                            let state = self.state.lock().await;
                            let Some(target) = exact_pane_hook_target(&state, *identity) else {
                                warn!(
                                    hook = ?hook_name,
                                    source,
                                    "stopping inline hook dispatch because its exact pane target no longer exists"
                                );
                                break;
                            };
                            Some(target)
                        }
                        HookDispatchCurrentTarget::ExactSession(session_id) => {
                            let identity = StableLifecycleTargetIdentity::Session {
                                session_id: *session_id,
                            };
                            let Some(target) = self.stable_hook_command_target(&identity).await
                            else {
                                warn!(
                                    hook = ?hook_name,
                                    source,
                                    "stopping inline hook dispatch because its exact session target no longer exists"
                                );
                                break;
                            };
                            Some(target)
                        }
                        HookDispatchCurrentTarget::Dynamic(target) => {
                            self.valid_hook_command_target(target.clone()).await
                        }
                        HookDispatchCurrentTarget::MissingStable => None,
                    };
                    let retained_lease = match &current_target {
                        HookDispatchCurrentTarget::Retainable(lease)
                        | HookDispatchCurrentTarget::HistoricalPane(lease) => {
                            Some(Arc::clone(lease))
                        }
                        _ => None,
                    };
                    let execution = if matches!(
                        current_target,
                        HookDispatchCurrentTarget::HistoricalPane(_)
                    ) {
                        self.execute_historical_hook_command_with_target_binding(
                            requester_pid,
                            hook.command(),
                            command_current_target,
                            retained_lease,
                        )
                        .await
                    } else {
                        self.execute_hook_command_with_target_binding(
                            requester_pid,
                            hook.command(),
                            command_current_target,
                            retained_lease,
                        )
                        .await
                    };
                    if let Err(error) = execution {
                        warn!(hook = ?hook_name, source, "failed to execute hook command: {error}");
                    }
                }
            })
            .await;
        })
    }
}
