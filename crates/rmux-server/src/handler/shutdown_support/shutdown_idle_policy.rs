use super::{IdleShutdownState, PendingShutdownReason, RequestHandler};
use rmux_proto::OptionName;
use std::sync::atomic::Ordering;

impl RequestHandler {
    #[cfg(unix)]
    pub(crate) async fn shutdown_abandoned_startup(&self) {
        if self.has_persistent_web_listener() {
            return;
        }
        let state = self.state.lock().await;
        if !state.sessions.is_empty()
            || state.options.resolve(None, OptionName::ExitEmpty) != Some("on")
        {
            return;
        }
        self.queue_shutdown_request(PendingShutdownReason::AbandonedStartup);
        drop(state);
        let _ = self.request_shutdown_if_pending();
    }

    pub(in crate::handler) async fn queue_shutdown_if_server_unattached(&self) -> bool {
        if self
            .state
            .lock()
            .await
            .options
            .resolve(None, OptionName::ExitUnattached)
            != Some("on")
        {
            return false;
        }
        self.queue_shutdown_if_server_empty().await
    }

    pub(crate) async fn request_shutdown_if_server_empty(&self) -> bool {
        if !self.queue_shutdown_if_server_empty().await {
            return false;
        }

        self.request_shutdown_if_pending()
    }

    pub(in crate::handler) async fn queue_shutdown_if_server_empty(&self) -> bool {
        let reason = {
            let state = self.state.lock().await;
            if state.options.resolve(None, OptionName::ExitEmpty) != Some("on") {
                None
            } else if state.sessions.is_empty() {
                Some(PendingShutdownReason::ExitEmpty)
            } else if state.options.resolve(None, OptionName::ExitUnattached) == Some("on") {
                let attached = self.active_attach.lock().await;
                let control = self.active_control.lock().await;
                let has_client = !attached.by_pid.is_empty()
                    || control.by_pid.values().any(|active| {
                        active.session_name.is_some() && !active.closing.load(Ordering::SeqCst)
                    });
                (!has_client).then_some(PendingShutdownReason::ExitUnattached)
            } else {
                None
            }
        };
        if let Some(reason) = reason {
            self.queue_shutdown_request(reason);
        }
        reason.is_some()
    }

    pub(super) fn pending_idle_shutdown_state(
        &self,
        reason: PendingShutdownReason,
        excluded_connection_id: Option<u64>,
    ) -> IdleShutdownState {
        let Ok(state) = self.state.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        let empty = state.sessions.is_empty();
        let applies = match reason {
            #[cfg(unix)]
            PendingShutdownReason::AbandonedStartup => {
                empty
                    && state.options.resolve(None, OptionName::ExitEmpty) == Some("on")
                    && !self.has_persistent_web_listener()
            }
            PendingShutdownReason::ExitEmpty => {
                empty && state.options.resolve(None, OptionName::ExitEmpty) == Some("on")
            }
            PendingShutdownReason::ExitUnattached => {
                state.options.resolve(None, OptionName::ExitEmpty) == Some("on")
                    && state.options.resolve(None, OptionName::ExitUnattached) == Some("on")
            }
            PendingShutdownReason::SeamlessUpgradeIdle => empty,
            PendingShutdownReason::KillServer => unreachable!("forced shutdown has no idle policy"),
        };
        if !applies {
            return IdleShutdownState::Stale;
        }
        drop(state);

        let Ok(active_attach) = self.active_attach.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if !active_attach.by_pid.is_empty() {
            return IdleShutdownState::Stale;
        }
        drop(active_attach);

        // Closing an attached client removes its registry entry before the
        // forwarder has drained the terminal exit frame to the client. Keep
        // exit-empty pending until that wire hand-off has completed.
        if self.active_attach_forwarders.load(Ordering::SeqCst) != 0 {
            return IdleShutdownState::Unknown;
        }

        if self.active_detached_requests.load(Ordering::SeqCst) != 0 {
            return IdleShutdownState::Unknown;
        }

        let Ok(active_detached_connections) = self.active_detached_connections.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if active_detached_connections
            .iter()
            .any(|connection_id| Some(*connection_id) != excluded_connection_id)
        {
            return match reason {
                // A detached connection can have finished the mutation and
                // received its response while an attached client's terminal
                // exit is still draining. Keep exit-empty pending until that
                // connection closes, then re-evaluate the server state.
                PendingShutdownReason::ExitEmpty | PendingShutdownReason::ExitUnattached => {
                    IdleShutdownState::Unknown
                }
                #[cfg(unix)]
                PendingShutdownReason::AbandonedStartup => IdleShutdownState::Unknown,
                PendingShutdownReason::SeamlessUpgradeIdle => IdleShutdownState::Stale,
                PendingShutdownReason::KillServer => {
                    unreachable!("kill-server does not use the idle shutdown state machine")
                }
            };
        }
        drop(active_detached_connections);

        // A client may have started web and closed between the first policy
        // check and the connection drain. Observe its published listener again.
        #[cfg(unix)]
        if reason == PendingShutdownReason::AbandonedStartup && self.has_persistent_web_listener() {
            return IdleShutdownState::Stale;
        }

        let Ok(active_control) = self.active_control.try_lock() else {
            return IdleShutdownState::Unknown;
        };
        if active_control.by_pid.is_empty() {
            return IdleShutdownState::StillApplies;
        }
        if matches!(reason, PendingShutdownReason::ExitUnattached)
            && active_control
                .by_pid
                .values()
                .any(|active| !active.closing.load(Ordering::SeqCst))
        {
            return IdleShutdownState::Stale;
        }
        if active_control
            .by_pid
            .values()
            .any(|active| active.session_name.is_none() && !active.closing.load(Ordering::SeqCst))
        {
            return IdleShutdownState::Stale;
        }
        // A control client bound to the now-missing last session is removed by
        // the ordered lifecycle path. Treat that short hand-off as in flight:
        // cancelling exit-empty here would lose the only shutdown request
        // before finish_control removes the registration.
        IdleShutdownState::Unknown
    }
}

impl PendingShutdownReason {
    /// kill-server waits for the endpoint to disappear as its completion, and a
    /// seamless upgrade launches its replacement without the startup lock.
    /// Every other idle exit may release its endpoint as soon as it stops
    /// accepting, reserving it through the startup lock instead.
    pub(in crate::handler) fn releases_endpoint_early(self) -> bool {
        match self {
            #[cfg(unix)]
            Self::AbandonedStartup => true,
            Self::ExitEmpty | Self::ExitUnattached => true,
            Self::KillServer | Self::SeamlessUpgradeIdle => false,
        }
    }
}
