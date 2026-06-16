use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmux_core::LifecycleEvent;
use rmux_os::identity::UserIdentity;
use tokio::sync::mpsc;

use super::RequestHandler;
use crate::control::{ControlClientFlags, ControlModeUpgrade, ControlServerEvent};
use crate::control_notifications::{
    collect_control_notifications, ControlClientSnapshot, PreparedControlNotification,
};
use crate::handler_support::{ambiguous_attached_client, attached_client_required};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::PaneOutputSender;
#[cfg(test)]
use crate::server_access::current_owner_uid;

#[path = "handler_control/session_attach.rs"]
mod session_attach;

#[derive(Debug, Default)]
pub(super) struct ActiveControlState {
    next_id: u64,
    pub(super) by_pid: HashMap<u32, ActiveControl>,
}

#[derive(Debug)]
pub(super) struct ActiveControl {
    pub(super) id: u64,
    pub(super) session_name: Option<rmux_proto::SessionName>,
    pub(super) last_session: Option<rmux_proto::SessionName>,
    pub(super) flags: ControlClientFlags,
    pub(super) uid: u32,
    pub(super) user: UserIdentity,
    pub(super) can_write: bool,
    pub(super) terminal_context: OuterTerminalContext,
    event_tx: mpsc::UnboundedSender<ControlServerEvent>,
    closing: Arc<AtomicBool>,
}

pub(crate) struct ControlRegistration {
    pub(crate) event_tx: mpsc::UnboundedSender<ControlServerEvent>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) uid: u32,
    pub(crate) user: UserIdentity,
    pub(crate) can_write: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ManagedClient {
    Attach(u32),
    Control(u32),
}

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn register_control_with_closing(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        event_tx: mpsc::UnboundedSender<ControlServerEvent>,
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
    }

    pub(crate) async fn register_control_with_access(
        &self,
        requester_pid: u32,
        upgrade: ControlModeUpgrade,
        registration: ControlRegistration,
    ) -> u64 {
        let mut active_control = self.active_control.lock().await;
        let control_id = active_control.next_id;
        active_control.next_id += 1;
        if let Some(previous) = active_control.by_pid.insert(
            requester_pid,
            ActiveControl {
                id: control_id,
                session_name: None,
                last_session: None,
                flags: ControlClientFlags::default(),
                uid: registration.uid,
                user: registration.user,
                can_write: registration.can_write,
                terminal_context: upgrade.terminal_context,
                event_tx: registration.event_tx,
                closing: registration.closing,
            },
        ) {
            previous.closing.store(true, Ordering::SeqCst);
            let _ = previous.event_tx.send(ControlServerEvent::Exit(None));
        }
        drop(active_control);

        for line in self.take_startup_config_error_notifications().await {
            self.send_control_notification_to(requester_pid, line).await;
        }

        control_id
    }

    pub(crate) async fn finish_control(&self, requester_pid: u32, control_id: u64) {
        let removed_session = {
            let mut active_control = self.active_control.lock().await;
            if active_control
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.id == control_id)
            {
                active_control
                    .by_pid
                    .remove(&requester_pid)
                    .and_then(|active| active.session_name)
            } else {
                None
            }
        };
        if let Some(session_name) = removed_session {
            self.destroy_unattached_sessions(vec![session_name]).await;
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

    pub(super) async fn attached_count_after_switch(
        &self,
        session_name: &rmux_proto::SessionName,
        client: ManagedClient,
    ) -> usize {
        let attached_count = self.attached_count(session_name).await;

        match client {
            ManagedClient::Attach(attach_pid) => {
                let active_attach = self.active_attach.lock().await;
                if active_attach
                    .by_pid
                    .get(&attach_pid)
                    .is_some_and(|active| &active.session_name == session_name)
                {
                    attached_count
                } else {
                    attached_count.saturating_add(1)
                }
            }
            ManagedClient::Control(control_pid) => {
                let active_control = self.active_control.lock().await;
                if active_control
                    .by_pid
                    .get(&control_pid)
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
        new_name: &rmux_proto::SessionName,
    ) {
        let mut active_control = self.active_control.lock().await;
        for active in active_control.by_pid.values_mut() {
            if active.session_name.as_ref() == Some(session_name) {
                active.session_name = Some(new_name.clone());
                let _ = active
                    .event_tx
                    .send(ControlServerEvent::SessionChanged(Some(new_name.clone())));
            }
            if active.last_session.as_ref() == Some(session_name) {
                active.last_session = Some(new_name.clone());
            }
        }
    }

    pub(super) async fn current_session_candidate(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        {
            let active_attach = self.active_attach.lock().await;
            if let Some(candidate) = active_attach.current_session_candidate(requester_pid) {
                return Some(candidate);
            }
        }

        let active_control = self.active_control.lock().await;
        active_control.current_session_candidate(requester_pid)
    }

    pub(super) async fn resolve_managed_client(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<ManagedClient, rmux_proto::RmuxError> {
        {
            let active_attach = self.active_attach.lock().await;
            if active_attach.by_pid.contains_key(&requester_pid) {
                return Ok(ManagedClient::Attach(requester_pid));
            }
        }
        {
            let active_control = self.active_control.lock().await;
            if active_control.by_pid.contains_key(&requester_pid) {
                return Ok(ManagedClient::Control(requester_pid));
            }
        }

        let attach_candidates = {
            let active_attach = self.active_attach.lock().await;
            active_attach.by_pid.keys().copied().collect::<Vec<_>>()
        };
        let control_candidates = {
            let active_control = self.active_control.lock().await;
            active_control.by_pid.keys().copied().collect::<Vec<_>>()
        };

        match attach_candidates.len() + control_candidates.len() {
            0 if command_name == "show-messages" => Err(rmux_proto::RmuxError::Message(
                "no current client".to_owned(),
            )),
            0 => Err(attached_client_required(command_name)),
            1 => {
                if let Some(pid) = attach_candidates.first().copied() {
                    Ok(ManagedClient::Attach(pid))
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
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.session_name.clone())
    }

    pub(crate) async fn is_control_client(&self, requester_pid: u32) -> bool {
        let active_control = self.active_control.lock().await;
        active_control.by_pid.contains_key(&requester_pid)
    }

    pub(super) async fn set_control_session(
        &self,
        requester_pid: u32,
        next_session_name: Option<rmux_proto::SessionName>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
            return Err(attached_client_required("control session"));
        };
        let previous = active.session_name.clone();
        if let (Some(previous_session), Some(next_session)) =
            (previous.as_ref(), next_session_name.as_ref())
        {
            if previous_session != next_session {
                active.last_session = Some(previous_session.clone());
            }
        }
        active.session_name = next_session_name.clone();
        if active
            .event_tx
            .send(ControlServerEvent::SessionChanged(next_session_name))
            .is_err()
        {
            active.closing.store(true, Ordering::SeqCst);
            active_control.by_pid.remove(&requester_pid);
            return Err(attached_client_required("control session"));
        }
        Ok(previous)
    }

    pub(super) async fn refresh_control_session(&self, session_name: &rmux_proto::SessionName) {
        let mut active_control = self.active_control.lock().await;
        active_control.by_pid.retain(|_, active| {
            if active.session_name.as_ref() != Some(session_name) {
                return true;
            }
            active.event_tx.send(ControlServerEvent::Refresh).is_ok()
        });
    }

    pub(super) async fn exit_control_session(
        &self,
        session_name: &rmux_proto::SessionName,
        reason: Option<String>,
    ) {
        let mut active_control = self.active_control.lock().await;
        active_control.by_pid.retain(|_, active| {
            if active.session_name.as_ref() != Some(session_name) {
                return true;
            }
            active.closing.store(true, Ordering::SeqCst);
            active
                .event_tx
                .send(ControlServerEvent::Exit(reason.clone()))
                .is_ok()
        });
    }

    pub(super) async fn detach_control_clients_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
        reason: Option<String>,
    ) -> Vec<u32> {
        let mut active_control = self.active_control.lock().await;
        let control_pids = active_control
            .by_pid
            .iter()
            .filter_map(|(&pid, active)| {
                (active.session_name.as_ref() == Some(session_name)).then_some(pid)
            })
            .collect::<Vec<_>>();

        for control_pid in &control_pids {
            let Some(active) = active_control.by_pid.get(control_pid) else {
                continue;
            };
            active.closing.store(true, Ordering::SeqCst);
            let _ = active
                .event_tx
                .send(ControlServerEvent::Exit(reason.clone()));
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

    pub(super) async fn send_control_notification(&self, line: String) {
        let mut active_control = self.active_control.lock().await;
        let targets = active_control.by_pid.keys().copied().collect::<Vec<_>>();
        for requester_pid in targets {
            deliver_control_notification(&mut active_control, requester_pid, line.clone());
        }
    }

    pub(super) async fn send_control_notification_to(&self, requester_pid: u32, line: String) {
        let mut active_control = self.active_control.lock().await;
        deliver_control_notification(&mut active_control, requester_pid, line);
    }

    pub(super) async fn send_control_notifications(
        &self,
        notifications: Vec<PreparedControlNotification>,
    ) {
        if notifications.is_empty() {
            return;
        }

        {
            let active_control = self.active_control.lock().await;
            if let Some(line) = uniform_broadcast_line(&active_control, &notifications) {
                drop(active_control);
                self.send_control_notification(line).await;
                return;
            }
        }

        let mut active_control = self.active_control.lock().await;
        for notification in notifications {
            deliver_control_notification(&mut active_control, notification.pid, notification.line);
        }
    }

    pub(super) async fn dispatch_control_notifications(&self, event: &LifecycleEvent) {
        let control_clients = self.control_clients_snapshot().await;
        if control_clients.is_empty() {
            return;
        }

        let notifications = {
            let state = self.state.lock().await;
            collect_control_notifications(&state, event, &control_clients)
        };
        self.send_control_notifications(notifications).await;
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
            | LifecycleEvent::ClientDetached { .. }
            | LifecycleEvent::SessionRenamed { .. }
            | LifecycleEvent::SessionCreated { .. }
            | LifecycleEvent::SessionWindowChanged { .. }
            | LifecycleEvent::PasteBufferChanged { .. }
            | LifecycleEvent::PasteBufferDeleted { .. } => {
                self.refresh_all_control_sessions().await;
            }
            LifecycleEvent::SessionClosed { session_name, .. } => {
                self.exit_control_session(session_name, None).await;
                self.refresh_all_control_sessions().await;
            }
            LifecycleEvent::ClientAttached { .. }
            | LifecycleEvent::AlertBell { .. }
            | LifecycleEvent::AlertActivity { .. }
            | LifecycleEvent::AlertSilence { .. }
            | LifecycleEvent::PaneExited { .. }
            | LifecycleEvent::PaneDied { .. }
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
        let mut active_control = self.active_control.lock().await;
        let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
            return Err(attached_client_required("detach-client"));
        };
        let session_name = active.session_name.clone();
        active.closing.store(true, Ordering::SeqCst);
        if active
            .event_tx
            .send(ControlServerEvent::Exit(reason))
            .is_err()
        {
            active_control.by_pid.remove(&requester_pid);
        }
        Ok(session_name)
    }

    pub(crate) async fn control_client_flags(
        &self,
        requester_pid: u32,
    ) -> Option<ControlClientFlags> {
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .map(|active| active.flags)
    }

    pub(crate) async fn control_last_session(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .and_then(|active| active.last_session.clone())
    }

    pub(crate) async fn control_session_panes(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> Result<Vec<(u32, PaneOutputSender)>, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        state.session_pane_outputs(session_name)
    }

    async fn control_clients_snapshot(&self) -> Vec<ControlClientSnapshot> {
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .iter()
            .map(|(&pid, active)| ControlClientSnapshot {
                pid,
                session_name: active.session_name.clone(),
            })
            .collect()
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

fn deliver_control_notification(
    active_control: &mut ActiveControlState,
    requester_pid: u32,
    line: String,
) {
    let Some(active) = active_control.by_pid.get_mut(&requester_pid) else {
        return;
    };
    if active
        .event_tx
        .send(ControlServerEvent::Notification(line))
        .is_err()
    {
        active.closing.store(true, Ordering::SeqCst);
        active_control.by_pid.remove(&requester_pid);
    }
}

fn uniform_broadcast_line(
    active_control: &ActiveControlState,
    notifications: &[PreparedControlNotification],
) -> Option<String> {
    let first = notifications.first()?;
    if notifications.len() != active_control.by_pid.len()
        || notifications
            .iter()
            .any(|notification| notification.line != first.line)
    {
        return None;
    }

    let targets = notifications
        .iter()
        .map(|notification| notification.pid)
        .collect::<HashSet<_>>();
    (targets.len() == active_control.by_pid.len()
        && active_control
            .by_pid
            .keys()
            .all(|requester_pid| targets.contains(requester_pid)))
    .then(|| first.line.clone())
}

impl ActiveControlState {
    pub(super) fn attached_count(&self, session_name: &rmux_proto::SessionName) -> usize {
        self.by_pid
            .values()
            .filter(|active| active.session_name.as_ref() == Some(session_name))
            .count()
    }

    fn current_session_candidate(&self, requester_pid: u32) -> Option<rmux_proto::SessionName> {
        if let Some(active) = self.by_pid.get(&requester_pid) {
            return active.session_name.clone();
        }

        if self.by_pid.len() == 1 {
            return self
                .by_pid
                .values()
                .next()
                .and_then(|active| active.session_name.clone());
        }

        None
    }
}
