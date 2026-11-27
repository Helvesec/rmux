use super::{
    mpsc, ControlClientFlags, ControlClientIdentity, ControlFormatSubscriptionTick,
    ControlFormatSubscriptions, ControlOutputQueue, ControlPaneOutputStates,
    ControlQueueCommandOrigin, ControlWriter, HashMap, HashSet, PaneEvent, PaneEventSender,
    PaneOutputAttachedClientReadDemandGuard, PaneOutputReadDemandGuard, PaneSubscription,
    RequestHandler, SessionName, VecDeque, MAX_DEFERRED_CONTROL_NOTIFICATIONS,
    MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES,
};

#[cfg(any(unix, windows))]
pub(super) struct ServerEventContext<'a> {
    pub(super) handler: &'a RequestHandler,
    pub(super) control_identity: ControlClientIdentity,
    pub(super) requester_pid: u32,
    pub(super) session_name: &'a mut Option<SessionName>,
    pub(super) subscriptions: &'a mut HashMap<u32, PaneSubscription>,
    pub(super) reader_demands: &'a mut HashMap<u32, PaneOutputReadDemandGuard>,
    pub(super) pane_output_states: &'a mut ControlPaneOutputStates,
    pub(super) format_subscriptions: &'a mut ControlFormatSubscriptions,
    pub(super) format_subscription_tick: &'a mut Option<ControlFormatSubscriptionTick>,
    pub(super) pane_event_tx: PaneEventSender,
    pub(super) pane_event_rx: &'a mut mpsc::Receiver<PaneEvent>,
    pub(super) output_queue: &'a mut ControlOutputQueue,
    pub(super) write_half: &'a mut ControlWriter,
    pub(super) paused_panes: &'a mut HashSet<u32>,
    pub(super) flags: &'a mut ControlClientFlags,
    pub(super) unrestricted_read_demand: &'a mut Option<PaneOutputReadDemandGuard>,
    pub(super) attached_client_read_presence:
        &'a mut Option<PaneOutputAttachedClientReadDemandGuard>,
    pub(super) deferred: &'a mut DeferredServerEvents,
}

#[cfg(any(unix, windows))]
impl ServerEventContext<'_> {
    pub(super) async fn flush_output(&mut self) -> std::io::Result<()> {
        super::flush_output_queue(
            self.output_queue,
            self.write_half,
            *self.flags,
            self.paused_panes,
            (cfg!(unix) && self.flags.pause_after_millis.is_some())
                .then_some((self.pane_event_rx, self.pane_output_states)),
        )
        .await
    }
}

#[derive(Debug, Default)]
#[cfg(any(unix, windows))]
pub(super) struct DeferredServerEvents {
    pub(super) notifications: VecDeque<String>,
    pub(super) notification_bytes: usize,
    pub(super) session_change: Option<DeferredSessionChange>,
    pub(super) exit_reason: Option<Option<String>>,
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(super) struct DeferredSessionChange {
    pub(super) session_name: Option<SessionName>,
    pub(super) pane_sequences: Option<Vec<(u32, u64)>>,
}

#[cfg(any(unix, windows))]
impl DeferredServerEvents {
    pub(super) fn defer_notification(&mut self, line: String) {
        if self.exit_reason.is_some() {
            return;
        }
        let next_bytes = self.notification_bytes.saturating_add(line.len());
        if self.notifications.len() >= MAX_DEFERRED_CONTROL_NOTIFICATIONS
            || next_bytes > MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES
        {
            self.notifications.clear();
            self.notification_bytes = 0;
            self.exit_reason = Some(Some("control notification queue exceeded".to_owned()));
            return;
        }
        self.notification_bytes = next_bytes;
        self.notifications.push_back(line);
    }

    pub(super) fn defer_limit_exit(&mut self, reason: String) {
        self.notifications.clear();
        self.notification_bytes = 0;
        self.session_change = None;
        self.exit_reason = Some(Some(reason));
    }

    pub(super) fn pop_notification(&mut self) -> Option<String> {
        let line = self.notifications.pop_front()?;
        self.notification_bytes = self.notification_bytes.saturating_sub(line.len());
        Some(line)
    }

    pub(super) fn defer_session_change(
        &mut self,
        next_session: Option<SessionName>,
        pane_sequences: Option<Vec<(u32, u64)>>,
    ) {
        if self.exit_reason.is_some() {
            return;
        }
        self.session_change = Some(DeferredSessionChange {
            session_name: next_session,
            pane_sequences,
        });
    }
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(super) struct ActiveControlResponseFrame {
    pub(super) open: bool,
    pub(super) origin: Option<ControlQueueCommandOrigin>,
}

#[cfg(any(unix, windows))]
impl ActiveControlResponseFrame {
    pub(super) const fn owner() -> Self {
        Self {
            open: true,
            origin: None,
        }
    }
}
