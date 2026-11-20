mod client_lookup;
mod notifications;
mod pane_output;
mod registration;
mod session_switch;

use crate::client_activity::ClientActivityTimestamp;
use std::collections::{HashMap, HashSet};

mod control_response_stream;
mod detach;
pub(crate) use control_response_stream::ControlCommandResponseSink;
mod window_sizes;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use window_sizes::ControlWindowSizes;
mod window_notifications;
pub(in crate::handler) use window_notifications::{
    freeze_control_layout_notifications, ControlLayoutNotification,
};

use rmux_core::LifecycleEvent;
use rmux_os::identity::UserIdentity;
use rmux_proto::{SessionId, TerminalSize};
use tokio::sync::{mpsc, watch};

use super::{
    attach_support::{switch_control_session_for_client, ControlResizeClient},
    client_support::SwitchTargetSelection,
    current_client_activity_timestamp, update_environment_from_client, QueuedLifecycleEvent,
    RequestHandler, SelectionTargetTransitionSnapshot,
};
use crate::client_names::control_client_name;
use crate::control::{
    validate_control_format_definition_batch, ControlClientFlags, ControlCommandResponseEvent,
    ControlFormatLimitExceeded, ControlFormatNotificationBatch, ControlFormatSubscriptions,
    ControlModeUpgrade, ControlPaneOutputAction, ControlServerEvent,
};
use crate::control_notifications::{collect_control_notifications, ControlClientSnapshot};
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{
    PaneOutputAttachedClientReadDemandGuard, PaneOutputReadDemandGuard, PaneOutputSender,
};
use crate::pane_terminals::HandlerState;
use crate::server_access::ServerAccessAdmission;
#[cfg(test)]
use crate::server_access::{
    current_owner_uid, pause_before_access_registration, AccessRegistrationKind,
};

mod session_attach;
mod session_update;
use session_update::ControlSessionUpdate;

const CONTROL_QUEUE_DRAIN_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CONTROL_CLIENT_WIDTH: u16 = 80;
const DEFAULT_CONTROL_CLIENT_HEIGHT: u16 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlRegistrationError {
    QueueDrainTimedOut { requester_pid: u32 },
    AccessRevoked,
}

impl ControlRegistrationError {
    pub(crate) fn into_rmux_error(self) -> rmux_proto::RmuxError {
        match self {
            Self::QueueDrainTimedOut { requester_pid } => rmux_proto::RmuxError::Server(format!(
                "timed out waiting for the previous control queue for client {requester_pid} to drain"
            )),
            Self::AccessRevoked => {
                rmux_proto::RmuxError::Server("access not allowed".to_owned())
            }
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ActiveControlState {
    pub(super) by_pid: HashMap<u32, ActiveControl>,
}

#[derive(Debug)]
pub(super) struct ActiveControl {
    _colour_presence: Arc<crate::control_colours::ControlColourPresence>,
    pub(super) id: u64,
    pub(super) last_activity_sequence: u64,
    pub(super) activity_at: ClientActivityTimestamp,
    pub(super) client_width: u16,
    pub(super) client_height: u16,
    /// Whether this client ever announced a size with `refresh-client -C`.
    ///
    /// A control client owns no window geometry until it has: tmux 3.7b's
    /// `ignore_client_size()` skips a `CLIENT_CONTROL` client that has no
    /// `CLIENT_SIZECHANGED`, so `client_width`/`client_height` stay at
    /// `DEFAULT_CONTROL_CLIENT_WIDTH`/`DEFAULT_CONTROL_CLIENT_HEIGHT` — a
    /// placeholder for reporting only — and never reach a policy. Measured
    /// 2026-07-25 across `window-size` latest/largest/smallest/manual: a
    /// control client attaching to a 200x50 session leaves it at 200x50 in
    /// tmux, whether or not another client is attached.
    pub(super) size_declared: bool,
    pub(super) window_sizes: ControlWindowSizes,
    pub(super) size_sequence: u64,
    pub(super) session_name: Option<rmux_proto::SessionName>,
    pub(super) session_id: Option<SessionId>,
    pub(super) last_session: Option<rmux_proto::SessionName>,
    pub(super) last_session_id: Option<SessionId>,
    pub(super) flags: ControlClientFlags,
    pub(super) uid: u32,
    pub(super) user: UserIdentity,
    pub(super) can_write: bool,
    pub(super) terminal_context: OuterTerminalContext,
    format_subscriptions: ControlFormatSubscriptions,
    event_tx: mpsc::Sender<ControlServerEvent>,
    pub(super) closing: Arc<AtomicBool>,
    client_detached_prepared: bool,
    queue_draining: bool,
    queue_drain_finished: watch::Sender<bool>,
}

pub(crate) struct ControlRegistration {
    pub(crate) event_tx: mpsc::Sender<ControlServerEvent>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) uid: u32,
    pub(crate) user: UserIdentity,
    pub(crate) can_write: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlQueueDrainLease {
    Acquired,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManagedClient {
    Attach { pid: u32, attach_id: u64 },
    Control(ControlClientIdentity),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlClientIdentity {
    requester_pid: u32,
    control_id: u64,
}

pub(super) struct ControlClientDetachOutcome {
    pub(in crate::handler) lifecycle_event: Option<QueuedLifecycleEvent>,
}

pub(super) struct ControlClientsDetachOutcome {
    pub(in crate::handler) lifecycle_events: Vec<QueuedLifecycleEvent>,
}

struct DestroyControlSwitchCommitOutcome {
    target_session_name: rmux_proto::SessionName,
    client_session_changed: QueuedLifecycleEvent,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct ControlSwitchPostCommitPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static CONTROL_SWITCH_POST_COMMIT_PAUSE: std::sync::Mutex<
    Vec<(u32, Arc<ControlSwitchPostCommitPause>)>,
> = std::sync::Mutex::new(Vec::new());

impl ControlClientIdentity {
    pub(crate) const fn new(requester_pid: u32, control_id: u64) -> Self {
        Self {
            requester_pid,
            control_id,
        }
    }

    pub(crate) const fn requester_pid(self) -> u32 {
        self.requester_pid
    }

    pub(crate) const fn control_id(self) -> u64 {
        self.control_id
    }
}

tokio::task_local! {
    static CONTROL_QUEUE_IDENTITY: ControlClientIdentity;
    static CONTROL_QUEUE_EOF_CANCELLATION: ControlQueueEofCancellation;
    static CONTROL_COMMAND_RESPONSE_SINK: ControlCommandResponseSink;
    static CONTROL_COMMAND_RESPONSE_CAPTURE: ();
}

#[derive(Debug, Clone)]
pub(crate) struct ControlQueueEofCancellation {
    identity: ControlClientIdentity,
    cancelled: watch::Sender<bool>,
    wait_cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum ControlQueueEofAction {
    Continue,
    StopFrame,
}

impl ControlQueueEofCancellation {
    pub(crate) fn new(identity: ControlClientIdentity) -> Self {
        let (cancelled, _receiver) = watch::channel(false);
        Self {
            identity,
            cancelled,
            wait_cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn cancel_for_eof(&self) {
        self.cancelled.send_replace(true);
    }

    pub(in crate::handler) fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    pub(in crate::handler) fn mark_wait_cancelled(&self) {
        self.wait_cancelled.store(true, Ordering::SeqCst);
    }

    fn action(&self) -> ControlQueueEofAction {
        if self.wait_cancelled.load(Ordering::SeqCst) {
            ControlQueueEofAction::StopFrame
        } else {
            ControlQueueEofAction::Continue
        }
    }

    pub(in crate::handler) async fn cancelled(&self) {
        let mut receiver = self.cancelled.subscribe();
        if *receiver.borrow_and_update() {
            return;
        }
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

pub(crate) async fn with_control_queue_identity<T, F>(
    identity: ControlClientIdentity,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    CONTROL_QUEUE_IDENTITY.scope(identity, future).await
}

pub(crate) async fn with_control_queue_eof_cancellation<T, F>(
    cancellation: ControlQueueEofCancellation,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    CONTROL_QUEUE_EOF_CANCELLATION
        .scope(cancellation, future)
        .await
}

pub(crate) async fn with_control_command_response_sink<T, F>(
    sink: ControlCommandResponseSink,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    CONTROL_COMMAND_RESPONSE_SINK.scope(sink, future).await
}

pub(in crate::handler) async fn with_control_command_response_capture<T, F>(future: F) -> T
where
    F: Future<Output = T>,
{
    CONTROL_COMMAND_RESPONSE_CAPTURE.scope((), future).await
}

fn current_control_command_response_sink() -> Option<ControlCommandResponseSink> {
    CONTROL_COMMAND_RESPONSE_CAPTURE.try_with(|()| ()).ok()?;
    CONTROL_COMMAND_RESPONSE_SINK.try_with(Clone::clone).ok()
}

pub(crate) fn control_command_response_stream_is_active(identity: ControlClientIdentity) -> bool {
    CONTROL_COMMAND_RESPONSE_SINK
        .try_with(|sink| sink.identity == identity && !sink.closing.load(Ordering::SeqCst))
        .unwrap_or(false)
}

pub(crate) fn control_command_response_has_individual_guards() -> bool {
    CONTROL_COMMAND_RESPONSE_SINK
        .try_with(|sink| sink.individual_guards)
        .unwrap_or(false)
}

pub(crate) fn send_control_command_response_event(
    identity: ControlClientIdentity,
    event: ControlCommandResponseEvent,
) -> bool {
    CONTROL_COMMAND_RESPONSE_SINK
        .try_with(|sink| (sink.identity == identity).then(|| sink.try_send(event)))
        .ok()
        .flatten()
        .unwrap_or(false)
}

pub(in crate::handler) fn current_control_queue_identity(
    requester_pid: u32,
) -> Option<ControlClientIdentity> {
    CONTROL_QUEUE_IDENTITY
        .try_with(|identity| (identity.requester_pid() == requester_pid).then_some(*identity))
        .ok()
        .flatten()
}

pub(in crate::handler) fn current_control_queue_eof_cancellation(
) -> Option<ControlQueueEofCancellation> {
    let identity = CONTROL_QUEUE_IDENTITY.try_with(|identity| *identity).ok()?;
    CONTROL_QUEUE_EOF_CANCELLATION
        .try_with(|cancellation| (cancellation.identity == identity).then(|| cancellation.clone()))
        .ok()
        .flatten()
}

pub(in crate::handler) fn control_queue_eof_action(
    identity: Option<ControlClientIdentity>,
) -> ControlQueueEofAction {
    let identity = identity.or_else(|| CONTROL_QUEUE_IDENTITY.try_with(|identity| *identity).ok());
    let Some(identity) = identity else {
        return ControlQueueEofAction::Continue;
    };
    CONTROL_QUEUE_EOF_CANCELLATION
        .try_with(|cancellation| {
            if cancellation.identity == identity {
                cancellation.action()
            } else {
                ControlQueueEofAction::Continue
            }
        })
        .unwrap_or(ControlQueueEofAction::Continue)
}

impl RequestHandler {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::handler) async fn switch_control_session_after_destroy(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        expected_current_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Option<rmux_proto::SessionName> {
        let (target_session_name, client_session_changed) = self
            .prepare_control_session_switch_after_destroy(
                requester_pid,
                expected_control_id,
                expected_current_session_id,
                target_session_id,
            )
            .await?;
        self.emit_prepared(client_session_changed).await;
        Some(target_session_name)
    }
}

struct PreparedControlSessionUpdate {
    event: ControlServerEvent,
    permit: mpsc::OwnedPermit<ControlServerEvent>,
}

fn prepare_control_session_update(
    active: &ActiveControl,
    next_session_name: Option<rmux_proto::SessionName>,
    pane_sequences: Option<Vec<(u32, u64)>>,
) -> Option<PreparedControlSessionUpdate> {
    if active.closing.load(Ordering::SeqCst) {
        return None;
    }
    let event = match (next_session_name, pane_sequences) {
        (Some(session_name), Some(pane_sequences)) => ControlServerEvent::SessionChangedAt {
            session_name,
            pane_sequences,
        },
        (next_session_name, None) => ControlServerEvent::SessionChanged(next_session_name),
        (None, Some(_)) => unreachable!("pane cursors require a control session"),
    };
    let permit = match active.event_tx.clone().try_reserve_owned() {
        Ok(permit) => permit,
        Err(_) => {
            active.closing.store(true, Ordering::SeqCst);
            return None;
        }
    };
    Some(PreparedControlSessionUpdate { event, permit })
}

fn commit_control_session_update(
    active: &mut ActiveControl,
    next_session_name: Option<rmux_proto::SessionName>,
    next_session_id: Option<SessionId>,
    prepared: PreparedControlSessionUpdate,
) -> Option<rmux_proto::SessionName> {
    let previous = active.session_name.clone();
    if let (Some(previous_session), Some(previous_session_id), Some(next_session), Some(next_id)) = (
        previous.as_ref(),
        active.session_id,
        next_session_name.as_ref(),
        next_session_id,
    ) {
        if previous_session != next_session || previous_session_id != next_id {
            active.last_session = Some(previous_session.clone());
            active.last_session_id = Some(previous_session_id);
        }
    }
    active.session_name = next_session_name.clone();
    active.session_id = next_session_id;
    let _ = prepared.permit.send(prepared.event);
    previous
}

fn update_control_session(
    active: &mut ActiveControl,
    next_session_name: Option<rmux_proto::SessionName>,
    next_session_id: Option<SessionId>,
    pane_sequences: Option<Vec<(u32, u64)>>,
) -> (Option<rmux_proto::SessionName>, bool) {
    let previous = active.session_name.clone();
    let Some(prepared) =
        prepare_control_session_update(active, next_session_name.clone(), pane_sequences)
    else {
        return (previous, false);
    };
    (
        commit_control_session_update(active, next_session_name, next_session_id, prepared),
        true,
    )
}

fn current_pane_output_sequences(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
) -> Result<Vec<(u32, u64)>, rmux_proto::RmuxError> {
    state.session_pane_outputs(session_name).map(|outputs| {
        outputs
            .into_iter()
            .map(|(pane_id, sender)| {
                let (sequence, ()) = sender.capture_with_next_sequence(|| ());
                (pane_id, sequence)
            })
            .collect()
    })
}

fn deliver_control_notification(
    active_control: &mut ActiveControlState,
    requester_pid: u32,
    line: String,
) -> bool {
    let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
        return false;
    };
    try_send_control_event(active, ControlServerEvent::Notification(line))
}

fn deliver_control_response_notification(
    active_control: &mut ActiveControlState,
    requester_pid: u32,
    line: String,
) -> bool {
    let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
        return false;
    };
    let target_identity = ControlClientIdentity::new(requester_pid, active.id);
    if let Some(sink) =
        current_control_command_response_sink().filter(|sink| sink.identity == target_identity)
    {
        if active.closing.load(Ordering::SeqCst) {
            return false;
        }
        if sink.try_send(ControlCommandResponseEvent::Notification(line)) {
            return true;
        }
        active.closing.store(true, Ordering::SeqCst);
        return false;
    }
    try_send_control_event(active, ControlServerEvent::Notification(line))
}

fn try_send_control_event(active: &ActiveControl, event: ControlServerEvent) -> bool {
    // Callers hold `active_control`: never await capacity for a client that is not draining.
    if active.closing.load(Ordering::SeqCst) {
        return false;
    }
    if active.event_tx.try_send(event).is_ok() {
        return true;
    }

    active.closing.store(true, Ordering::SeqCst);
    false
}

fn close_control_with_exit(active: &ActiveControl, reason: Option<String>) {
    if active.closing.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = active.event_tx.try_send(ControlServerEvent::Exit(reason));
}

fn control_clients_snapshot_locked(
    state: &HandlerState,
    active_control: &ActiveControlState,
) -> Vec<ControlClientSnapshot> {
    active_control
        .by_pid
        .iter()
        .filter_map(|(&pid, active)| {
            let session_name = match (&active.session_name, active.session_id) {
                (None, None) => None,
                (Some(_), Some(session_id)) => {
                    Some(state.sessions.session_by_id(session_id)?.name().clone())
                }
                (None, Some(_)) | (Some(_), None) => return None,
            };
            Some(ControlClientSnapshot { pid, session_name })
        })
        .collect()
}

impl ActiveControlState {
    pub(super) fn attached_count(&self, session_name: &rmux_proto::SessionName) -> usize {
        self.by_pid
            .values()
            .filter(|active| active.session_name.as_ref() == Some(session_name))
            .count()
    }

    fn current_session_candidate(
        &self,
        requester_pid: u32,
    ) -> Option<(rmux_proto::SessionName, SessionId)> {
        if let Some(active) = self.by_pid.get(&requester_pid) {
            if active.closing.load(Ordering::SeqCst) {
                return None;
            }
            return active.session_name.clone().zip(active.session_id);
        }

        if self.by_pid.len() == 1 {
            return self
                .by_pid
                .values()
                .next()
                .filter(|active| !active.closing.load(Ordering::SeqCst))
                .and_then(|active| active.session_name.clone().zip(active.session_id));
        }

        None
    }
}
