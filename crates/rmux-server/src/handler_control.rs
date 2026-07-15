use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_core::LifecycleEvent;
use rmux_os::identity::UserIdentity;
use rmux_proto::SessionId;
use tokio::sync::{mpsc, watch};

use super::{
    client_support::SwitchTargetSelection, update_environment_from_client, QueuedLifecycleEvent,
    RequestHandler,
};
use crate::control::{ControlClientFlags, ControlModeUpgrade, ControlServerEvent};
use crate::control_notifications::{collect_control_notifications, ControlClientSnapshot};
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::PaneOutputSender;
use crate::pane_terminals::HandlerState;
#[cfg(test)]
use crate::server_access::current_owner_uid;

#[path = "handler_control/session_attach.rs"]
mod session_attach;

const CONTROL_QUEUE_DRAIN_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlRegistrationError {
    QueueDrainTimedOut { requester_pid: u32 },
}

impl ControlRegistrationError {
    pub(crate) fn into_rmux_error(self) -> rmux_proto::RmuxError {
        match self {
            Self::QueueDrainTimedOut { requester_pid } => rmux_proto::RmuxError::Server(format!(
                "timed out waiting for the previous control queue for client {requester_pid} to drain"
            )),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ActiveControlState {
    next_id: u64,
    pub(super) by_pid: HashMap<u32, ActiveControl>,
}

#[derive(Debug)]
pub(super) struct ActiveControl {
    pub(super) id: u64,
    pub(super) session_name: Option<rmux_proto::SessionName>,
    pub(super) session_id: Option<SessionId>,
    pub(super) last_session: Option<rmux_proto::SessionName>,
    pub(super) last_session_id: Option<SessionId>,
    pub(super) flags: ControlClientFlags,
    pub(super) uid: u32,
    pub(super) user: UserIdentity,
    pub(super) can_write: bool,
    pub(super) terminal_context: OuterTerminalContext,
    event_tx: mpsc::Sender<ControlServerEvent>,
    pub(super) closing: Arc<AtomicBool>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlOutputStart {
    Current,
    Oldest,
}

struct ControlSessionUpdate<'a> {
    target_selection: Option<SwitchTargetSelection>,
    client_environment: Option<&'a HashMap<String, String>>,
    output_start: ControlOutputStart,
}

impl<'a> ControlSessionUpdate<'a> {
    fn existing(
        target_selection: Option<SwitchTargetSelection>,
        client_environment: Option<&'a HashMap<String, String>>,
    ) -> Self {
        Self {
            target_selection,
            client_environment,
            output_start: ControlOutputStart::Current,
        }
    }

    fn created() -> Self {
        Self {
            target_selection: None,
            client_environment: None,
            output_start: ControlOutputStart::Oldest,
        }
    }
}

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
                user: UserIdentity::Uid(current_owner_uid()),
                can_write: true,
            },
        )
        .await
        .expect("test control registration must not outlive the bounded queue drain")
    }

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
            drain_timeout,
        )
        .await
    }

    async fn register_control_with_access_timeout(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        registration: ControlRegistration,
        drain_timeout: Duration,
    ) -> Result<u64, ControlRegistrationError> {
        let drain_deadline = tokio::time::Instant::now() + drain_timeout;
        let control_id = loop {
            let mut active_control = self.active_control.lock().await;
            let drain_finished = active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| active.queue_draining)
                .map(|active| active.queue_drain_finished.subscribe());
            if let Some(mut drain_finished) = drain_finished {
                drop(active_control);
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

            let control_id = active_control.next_id;
            active_control.next_id += 1;
            let (queue_drain_finished, _queue_drain_pending) = watch::channel(false);
            if let Some(previous) = active_control.by_pid.insert(
                requester_pid,
                ActiveControl {
                    id: control_id,
                    session_name: None,
                    session_id: None,
                    last_session: None,
                    last_session_id: None,
                    flags: ControlClientFlags::default(),
                    uid: registration.uid,
                    user: registration.user,
                    can_write: registration.can_write,
                    terminal_context: upgrade.terminal_context,
                    event_tx: registration.event_tx,
                    closing: registration.closing,
                    queue_draining: false,
                    queue_drain_finished,
                },
            ) {
                previous.closing.store(true, Ordering::SeqCst);
                let _ = try_send_control_event(&previous, ControlServerEvent::Exit(None));
            }
            break control_id;
        };

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
        let (removed, removed_session) = {
            let mut active_control = self.active_control.lock().await;
            if active_control
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.id == control_id)
            {
                let removed = active_control
                    .by_pid
                    .remove(&requester_pid)
                    .expect("validated control registration remains present");
                removed.queue_drain_finished.send_replace(true);
                (true, removed.session_name.zip(removed.session_id))
            } else {
                (false, None)
            }
        };
        if let Some(session_identity) = removed_session {
            self.destroy_unattached_sessions(vec![session_identity])
                .await;
        }
        if removed {
            let _ = self.request_shutdown_if_pending();
        }
    }

    pub(super) async fn attached_count(&self, session_name: &rmux_proto::SessionName) -> usize {
        let attach_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_count(session_name)
        };
        let control_count = {
            let active_control = self.active_control.lock().await;
            active_control.attached_count(session_name)
        };

        attach_count.saturating_add(control_count)
    }

    pub(super) async fn attached_count_for_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
    ) -> usize {
        let attach_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .values()
                .filter(|active| {
                    &active.session_name == session_name
                        && active.session_id == session_id
                        && !active.suspended
                })
                .count()
        };
        let control_count = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .values()
                .filter(|active| {
                    active.session_name.as_ref() == Some(session_name)
                        && active.session_id == Some(session_id)
                })
                .count()
        };

        attach_count.saturating_add(control_count)
    }

    pub(super) async fn attached_count_after_switch(
        &self,
        session_name: &rmux_proto::SessionName,
        client: ManagedClient,
    ) -> usize {
        let attached_count = self.attached_count(session_name).await;

        match client {
            ManagedClient::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                let active_attach = self.active_attach.lock().await;
                if active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                    active.id == attach_id && &active.session_name == session_name
                }) {
                    attached_count
                } else {
                    attached_count.saturating_add(1)
                }
            }
            ManagedClient::Control(identity) => {
                let active_control = self.active_control.lock().await;
                if active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|active| active.id == identity.control_id())
                    .and_then(|active| active.session_name.as_ref())
                    .is_some_and(|active| active == session_name)
                {
                    attached_count
                } else {
                    attached_count.saturating_add(1)
                }
            }
        }
    }

    pub(super) async fn rename_control_session(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        new_name: &rmux_proto::SessionName,
    ) {
        let mut active_control = self.active_control.lock().await;
        active_control.by_pid.retain(|_, active| {
            if active.session_name.as_ref() == Some(session_name)
                && active.session_id == Some(session_id)
            {
                active.session_name = Some(new_name.clone());
                if !try_send_control_event(
                    active,
                    ControlServerEvent::SessionChanged(Some(new_name.clone())),
                ) {
                    return false;
                }
            }
            if active.last_session.as_ref() == Some(session_name)
                && active.last_session_id == Some(session_id)
            {
                active.last_session = Some(new_name.clone());
            }
            true
        });
    }

    pub(super) async fn current_session_candidate(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let state = self.state.lock().await;
            let active_control = self.active_control.lock().await;
            Self::validate_control_queue_identity_locked(
                &state,
                &active_control,
                requester_pid,
                identity.control_id(),
            )
            .ok()?;
            return active_control
                .by_pid
                .get(&requester_pid)
                .and_then(|active| active.session_name.clone());
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Some(candidate) = active_attach.current_session_candidate(requester_pid) {
                return Some(candidate);
            }
        }

        let candidate = {
            let active_control = self.active_control.lock().await;
            active_control.current_session_candidate(requester_pid)
        }?;
        let state = self.state.lock().await;
        state
            .sessions
            .session(&candidate.0)
            .is_some_and(|session| session.id() == candidate.1)
            .then_some(candidate.0)
    }

    pub(super) async fn validate_control_queue_session_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
    ) -> Result<(), rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            requester_pid,
            expected_control_id,
        )
    }

    pub(super) fn validate_control_queue_identity_locked(
        state: &HandlerState,
        active_control: &ActiveControlState,
        requester_pid: u32,
        expected_control_id: u64,
    ) -> Result<(), rmux_proto::RmuxError> {
        let Some(active) = active_control.by_pid.get(&requester_pid) else {
            return Err(attached_client_required("control command"));
        };
        if active.id != expected_control_id {
            return Err(attached_client_required("control command"));
        }
        if active.closing.load(Ordering::SeqCst) {
            return Err(rmux_proto::RmuxError::Server(
                "control client is closing".to_owned(),
            ));
        }
        match (active.session_name.as_ref(), active.session_id) {
            (None, None) => Ok(()),
            (Some(session_name), Some(session_id))
                if state
                    .sessions
                    .session(session_name)
                    .is_some_and(|session| session.id() == session_id) =>
            {
                Ok(())
            }
            (Some(session_name), _) => Err(rmux_proto::RmuxError::SessionNotFound(
                session_name.to_string(),
            )),
            (None, Some(_)) => Err(rmux_proto::RmuxError::Server(
                "control client has an invalid session identity".to_owned(),
            )),
        }
    }

    #[cfg(test)]
    pub(super) async fn control_queue_client_id(
        &self,
        requester_pid: u32,
    ) -> Result<u64, rmux_proto::RmuxError> {
        let active_control = self.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .ok_or_else(|| attached_client_required("control command"))?;
        if active.closing.load(Ordering::SeqCst) {
            return Err(rmux_proto::RmuxError::Server(
                "control client is closing".to_owned(),
            ));
        }
        Ok(active.id)
    }

    pub(super) async fn resolve_managed_client(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<ManagedClient, rmux_proto::RmuxError> {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let state = self.state.lock().await;
            let active_control = self.active_control.lock().await;
            Self::validate_control_queue_identity_locked(
                &state,
                &active_control,
                requester_pid,
                identity.control_id(),
            )?;
            return Ok(ManagedClient::Control(identity));
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(Ordering::SeqCst))
            {
                return Ok(ManagedClient::Attach {
                    pid: requester_pid,
                    attach_id: active.id,
                });
            }
        }
        {
            let active_control = self.active_control.lock().await;
            if let Some(active) = active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(Ordering::SeqCst))
            {
                return Ok(ManagedClient::Control(ControlClientIdentity::new(
                    requester_pid,
                    active.id,
                )));
            }
        }

        let attach_candidates = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                .map(|(&pid, active)| (pid, active.id))
                .collect::<Vec<_>>()
        };
        let control_candidates = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .iter()
                .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                .map(|(&pid, active)| ControlClientIdentity::new(pid, active.id))
                .collect::<Vec<_>>()
        };

        match attach_candidates.len() + control_candidates.len() {
            0 if command_name == "show-messages" => Err(rmux_proto::RmuxError::Message(
                "no current client".to_owned(),
            )),
            0 => Err(attached_client_required(command_name)),
            1 => {
                if let Some((pid, attach_id)) = attach_candidates.first().copied() {
                    Ok(ManagedClient::Attach { pid, attach_id })
                } else {
                    Ok(ManagedClient::Control(
                        control_candidates
                            .first()
                            .copied()
                            .expect("single control candidate"),
                    ))
                }
            }
            _ => Err(ambiguous_attached_client(command_name)),
        }
    }

    pub(crate) async fn control_session_name(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        let expected_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .filter(|active| {
                expected_control_id.is_none_or(|expected| {
                    active.id == expected && !active.closing.load(Ordering::SeqCst)
                })
            })
            .and_then(|active| active.session_name.clone())
    }

    pub(in crate::handler) async fn control_queue_can_write(
        &self,
        identity: ControlClientIdentity,
    ) -> bool {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        if Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )
        .is_err()
        {
            return false;
        }
        active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| active.can_write)
    }

    pub(crate) async fn is_control_client(&self, requester_pid: u32) -> bool {
        let expected_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .is_some_and(|active| {
                expected_control_id.is_none_or(|expected| active.id == expected)
                    && !active.closing.load(Ordering::SeqCst)
            })
    }

    #[cfg(test)]
    pub(super) async fn set_control_session(
        &self,
        requester_pid: u32,
        next_session_name: Option<rmux_proto::SessionName>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            next_session_name,
            None,
            None,
            ControlSessionUpdate::existing(None, None),
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn set_control_session_identity(
        &self,
        requester_pid: u32,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            Some(next_session_name),
            Some(expected_session_id),
            None,
            ControlSessionUpdate::existing(None, None),
        )
        .await
    }

    pub(super) async fn set_control_session_for_client_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
        target_selection: Option<SwitchTargetSelection>,
        client_environment: Option<&HashMap<String, String>>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            Some(next_session_name),
            Some(expected_session_id),
            Some(expected_control_id),
            ControlSessionUpdate::existing(target_selection, client_environment),
        )
        .await
    }

    pub(in crate::handler) async fn set_created_control_session_for_client_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            Some(next_session_name),
            Some(expected_session_id),
            Some(expected_control_id),
            ControlSessionUpdate::created(),
        )
        .await
    }

    pub(in crate::handler) async fn switch_control_session_after_destroy(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        expected_current_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Option<rmux_proto::SessionName> {
        let mut state = self.state.lock().await;
        let target_session_name = state
            .sessions
            .session_by_id(target_session_id)?
            .name()
            .clone();
        let pane_sequences = current_pane_output_sequences(&state, &target_session_name).ok()?;
        let mut active_control = self.active_control.lock().await;
        let active = active_control
            .by_pid
            .get_mut(&requester_pid)
            .filter(|active| {
                active.id == expected_control_id
                    && active.session_id == Some(expected_current_session_id)
                    && !active.closing.load(Ordering::SeqCst)
            })?;
        let (_, delivered) = update_control_session(
            active,
            Some(target_session_name.clone()),
            Some(target_session_id),
            Some(pane_sequences),
        );
        if !delivered {
            active_control.by_pid.remove(&requester_pid);
            return None;
        }
        state
            .sessions
            .session_mut(&target_session_name)
            .expect("stable destroy-switch target remains locked")
            .touch_attached();
        Some(target_session_name)
    }

    async fn set_control_session_with_expected_identity(
        &self,
        requester_pid: u32,
        next_session_name: Option<rmux_proto::SessionName>,
        expected_session_id: Option<SessionId>,
        expected_control_id: Option<u64>,
        update: ControlSessionUpdate<'_>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        let ControlSessionUpdate {
            target_selection,
            client_environment,
            output_start,
        } = update;
        let exact_client_identity = expected_control_id.is_some();
        let command_name = if exact_client_identity {
            "switch-client"
        } else {
            "control session"
        };
        let touch_attached = exact_client_identity;
        let queued_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        if expected_control_id
            .zip(queued_control_id)
            .is_some_and(|(explicit, queued)| explicit != queued)
        {
            return Err(attached_client_required(command_name));
        }
        let expected_control_id = expected_control_id.or(queued_control_id);
        let mut state = self.state.lock().await;
        let next_session_id = match next_session_name.as_ref() {
            Some(session_name) => {
                let session_id = state
                    .sessions
                    .session(session_name)
                    .ok_or_else(|| {
                        rmux_proto::RmuxError::SessionNotFound(session_name.to_string())
                    })?
                    .id();
                if expected_session_id.is_some_and(|expected| expected != session_id) {
                    return Err(rmux_proto::RmuxError::SessionNotFound(
                        session_name.to_string(),
                    ));
                }
                Some(session_id)
            }
            None => None,
        };
        let pane_sequences = match output_start {
            ControlOutputStart::Current => next_session_name
                .as_ref()
                .map(|session_name| current_pane_output_sequences(&state, session_name))
                .transpose()?,
            ControlOutputStart::Oldest => None,
        };
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
            return Err(attached_client_required(command_name));
        };
        if expected_control_id.is_some_and(|expected| active.id != expected)
            || active.closing.load(Ordering::SeqCst)
        {
            return Err(attached_client_required(command_name));
        }
        if let Some(selection) = target_selection.as_ref() {
            let session_name = next_session_name
                .as_ref()
                .expect("a switch target selection carries a session");
            let session_id = next_session_id
                .expect("a switch target selection carries a stable session identity");
            selection.validate_for_session_identity(&state, session_name, session_id)?;
        }
        let (previous, delivered) = update_control_session(
            active,
            next_session_name.clone(),
            next_session_id,
            pane_sequences,
        );
        if !delivered {
            active_control.by_pid.remove(&requester_pid);
            return Err(attached_client_required(command_name));
        }
        if let (Some(session_name), Some(client_environment)) =
            (next_session_name.as_ref(), client_environment)
        {
            update_environment_from_client(&mut state, session_name, client_environment);
        }
        if let Some(selection) = target_selection.as_ref() {
            selection
                .apply_to_state(&mut state)
                .expect("prevalidated switch selection remains applicable while locked");
        }
        if touch_attached {
            let session_name = next_session_name
                .as_ref()
                .expect("switch-client always carries a target session");
            state
                .sessions
                .session_mut(session_name)
                .expect("target session stayed locked across the control update")
                .touch_attached();
        }
        Ok(previous)
    }

    pub(in crate::handler) async fn attach_control_session_for_queue(
        &self,
        identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<SessionId>,
    ) -> Result<bool, rmux_proto::RmuxError> {
        self.attach_control_session_for_queue_with_output_start(
            identity,
            session_name,
            expected_session_id,
            false,
        )
        .await
    }

    pub(in crate::handler) async fn attach_created_control_session_for_queue(
        &self,
        identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<SessionId>,
    ) -> Result<bool, rmux_proto::RmuxError> {
        self.attach_control_session_for_queue_with_output_start(
            identity,
            session_name,
            expected_session_id,
            true,
        )
        .await
    }

    async fn attach_control_session_for_queue_with_output_start(
        &self,
        identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<SessionId>,
        replay_from_oldest: bool,
    ) -> Result<bool, rmux_proto::RmuxError> {
        let mut state = self.state.lock().await;
        let mut active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )?;

        let session_id = state
            .sessions
            .session(session_name)
            .ok_or_else(|| rmux_proto::RmuxError::SessionNotFound(session_name.to_string()))?
            .id();
        if expected_session_id.is_some_and(|expected| expected != session_id) {
            return Err(rmux_proto::RmuxError::SessionNotFound(
                session_name.to_string(),
            ));
        }
        if active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| {
                active.session_name.as_ref() == Some(session_name)
                    && active.session_id == Some(session_id)
            })
        {
            return Ok(false);
        }

        let pane_sequences = if replay_from_oldest {
            None
        } else {
            Some(current_pane_output_sequences(&state, session_name)?)
        };

        let delivered = {
            let active = active_control
                .by_pid
                .get_mut(&identity.requester_pid())
                .expect("validated control client remains registered while locked");
            update_control_session(
                active,
                Some(session_name.clone()),
                Some(session_id),
                pane_sequences,
            )
            .1
        };
        if !delivered {
            active_control.by_pid.remove(&identity.requester_pid());
            return Err(attached_client_required("control session"));
        }
        state
            .sessions
            .session_mut(session_name)
            .expect("validated session remains present while locked")
            .touch_attached();
        Ok(true)
    }

    pub(super) async fn refresh_control_session(&self, session_name: &rmux_proto::SessionName) {
        let mut active_control = self.active_control.lock().await;
        active_control.by_pid.retain(|_, active| {
            if active.session_name.as_ref() != Some(session_name) {
                return true;
            }
            try_send_control_event(active, ControlServerEvent::Refresh)
        });
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
        active_control.by_pid.retain(|_, active| {
            if active.session_name.as_ref() != Some(session_name)
                || active.session_id != Some(session_id)
            {
                return true;
            }
            try_send_control_event(active, ControlServerEvent::Refresh)
        });
    }

    pub(super) async fn refresh_control_client_for_identity(
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
            active_control.by_pid.remove(&identity.requester_pid());
            return Err(attached_client_required("refresh-client"));
        }
        Ok(())
    }

    pub(super) async fn exit_control_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        reason: Option<String>,
    ) {
        let mut active_control = self.active_control.lock().await;
        active_control.by_pid.retain(|_, active| {
            if active.last_session.as_ref() == Some(session_name)
                && active.last_session_id == Some(session_id)
            {
                active.last_session = None;
                active.last_session_id = None;
            }
            if active.session_name.as_ref() != Some(session_name)
                || active.session_id != Some(session_id)
            {
                return true;
            }
            active.closing.store(true, Ordering::SeqCst);
            try_send_control_event(active, ControlServerEvent::Exit(reason.clone()))
        });
    }

    pub(super) async fn detach_control_clients_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
        reason: Option<String>,
    ) -> Vec<u32> {
        let session_id = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Vec::new();
            };
            session.id()
        };
        let mut active_control = self.active_control.lock().await;
        let control_pids = active_control
            .by_pid
            .iter()
            .filter_map(|(&pid, active)| {
                (active.session_name.as_ref() == Some(session_name)
                    && active.session_id == Some(session_id))
                .then_some(pid)
            })
            .collect::<Vec<_>>();

        for control_pid in &control_pids {
            let Some(active) = active_control.by_pid.get(control_pid) else {
                continue;
            };
            active.closing.store(true, Ordering::SeqCst);
            let _ = try_send_control_event(active, ControlServerEvent::Exit(reason.clone()));
        }
        for control_pid in &control_pids {
            active_control.by_pid.remove(control_pid);
        }

        control_pids
    }

    pub(super) async fn refresh_all_control_sessions(&self) {
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

    pub(super) async fn send_control_notification_to(&self, requester_pid: u32, line: String) {
        let mut active_control = self.active_control.lock().await;
        deliver_control_notification(&mut active_control, requester_pid, line);
    }

    pub(in crate::handler) async fn send_control_notification_to_queue(
        &self,
        identity: ControlClientIdentity,
        line: String,
    ) {
        let mut active_control = self.active_control.lock().await;
        if active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| {
                active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
            })
        {
            deliver_control_notification(&mut active_control, identity.requester_pid(), line);
        }
    }

    pub(super) async fn dispatch_control_notifications(&self, event: &QueuedLifecycleEvent) {
        let state = self.state.lock().await;
        let mut active_control = self.active_control.lock().await;
        let control_clients = control_clients_snapshot_locked(&state, &active_control);
        if control_clients.is_empty() {
            return;
        }

        let notifications = collect_control_notifications(
            &state,
            &event.event,
            event.control_session_identity,
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
    ) -> Arc<super::ControlNotificationDeliveryPause> {
        let pause = Arc::new(super::ControlNotificationDeliveryPause::default());
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

    pub(super) async fn refresh_control_sessions_for_event(&self, event: &LifecycleEvent) {
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

    pub(super) async fn exit_control_client(
        &self,
        requester_pid: u32,
        reason: Option<String>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.exit_control_client_with_expected_id(requester_pid, None, reason)
            .await
    }

    pub(super) async fn exit_control_client_for_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        reason: Option<String>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.exit_control_client_with_expected_id(requester_pid, Some(expected_control_id), reason)
            .await
    }

    async fn exit_control_client_with_expected_id(
        &self,
        requester_pid: u32,
        expected_control_id: Option<u64>,
        reason: Option<String>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
            return Err(attached_client_required("detach-client"));
        };
        if expected_control_id.is_some_and(|expected| active.id != expected) {
            return Err(attached_client_required("detach-client"));
        }
        let session_name = active.session_name.clone();
        active.closing.store(true, Ordering::SeqCst);
        if !try_send_control_event(active, ControlServerEvent::Exit(reason)) {
            active_control.by_pid.remove(&requester_pid);
        }
        Ok(session_name)
    }

    pub(crate) async fn control_client_flags(
        &self,
        requester_pid: u32,
    ) -> Option<ControlClientFlags> {
        let expected_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .filter(|active| {
                expected_control_id.is_none_or(|expected| {
                    active.id == expected && !active.closing.load(Ordering::SeqCst)
                })
            })
            .map(|active| active.flags)
    }

    pub(crate) async fn control_session_panes(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> Result<Vec<(u32, PaneOutputSender)>, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        state.session_pane_outputs(session_name)
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

fn update_control_session(
    active: &mut ActiveControl,
    next_session_name: Option<rmux_proto::SessionName>,
    next_session_id: Option<SessionId>,
    pane_sequences: Option<Vec<(u32, u64)>>,
) -> (Option<rmux_proto::SessionName>, bool) {
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
    let event = match (next_session_name, pane_sequences) {
        (Some(session_name), Some(pane_sequences)) => ControlServerEvent::SessionChangedAt {
            session_name,
            pane_sequences,
        },
        (next_session_name, None) => ControlServerEvent::SessionChanged(next_session_name),
        (None, Some(_)) => unreachable!("pane cursors require a control session"),
    };
    let delivered = try_send_control_event(active, event);
    (previous, delivered)
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
) {
    let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
        return;
    };
    if !try_send_control_event(active, ControlServerEvent::Notification(line)) {
        active_control.by_pid.remove(&requester_pid);
    }
}

fn try_send_control_event(active: &ActiveControl, event: ControlServerEvent) -> bool {
    // Callers hold `active_control`: never await capacity for a client that is not draining.
    if active.event_tx.try_send(event).is_ok() {
        return true;
    }

    active.closing.store(true, Ordering::SeqCst);
    false
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
