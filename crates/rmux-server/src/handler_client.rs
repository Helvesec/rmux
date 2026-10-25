use std::path::PathBuf;
use std::sync::atomic::Ordering;

use rmux_core::formats::{is_truthy, FormatContext};
use rmux_proto::request::{ListClientsRequest, SuspendClientRequest};
use rmux_proto::{
    ErrorResponse, ListClientsResponse, Response, RmuxError, SuspendClientResponse,
    TerminalGeometry, TerminalSize,
};

use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;
use crate::pane_terminals::session_not_found;

use super::{
    attach_support::{ActiveAttachIdentity, ClientFlags, IncomingSizeClient},
    attached_client_matches_target, command_output_from_lines, control_client_target_pid,
    control_support::{current_control_queue_identity, ManagedClient},
    format_requester_uid, normalize_target_client, session_selection_prefers_live_process,
    sort_list_clients, validate_expected_attach_identity, RequestHandler, LIST_CLIENTS_TEMPLATE,
};

#[path = "handler_client/attach.rs"]
mod attach;
#[cfg(test)]
#[path = "handler_client/control_geometry_tests.rs"]
mod control_geometry_tests;
#[path = "handler_client/detach.rs"]
mod detach;
#[path = "handler_client/refresh.rs"]
mod refresh;
#[cfg(test)]
#[path = "handler_client/switch_atomicity_tests.rs"]
mod switch_atomicity_tests;
#[path = "handler_client/switching.rs"]
mod switching;

pub(in crate::handler) use switching::{
    capture_switch_client_target_identity, SwitchManagedClientIdentity, SwitchTargetSelection,
};

/// The client a session resize is being performed for.
///
/// This is a client that holds no `active_attach` registration to displace — a
/// first attach, or a control client, which is registered in `active_control`
/// instead. Such a command stands beside every attached vote rather than
/// replacing one, and it delivers no switch frame, so its resize is the only
/// shared-geometry write it makes.
///
/// An already-attached client moving between sessions is *not* resized here:
/// `commit_attached_session_switch` owns that move end to end, and a second
/// write from outside its lock region could not honour the same authority.
#[derive(Clone, Copy)]
pub(in crate::handler) struct AttachResizeClient {
    geometry: Option<TerminalGeometry>,
    flags: ClientFlags,
}

impl AttachResizeClient {
    pub(in crate::handler) fn new(client_size: Option<TerminalSize>, flags: ClientFlags) -> Self {
        Self {
            geometry: client_size.map(TerminalGeometry::from_size),
            flags,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct ManagedClientResolutionPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static MANAGED_CLIENT_RESOLUTION_PAUSE: std::sync::Mutex<
    Option<(u32, std::sync::Arc<ManagedClientResolutionPause>)>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(in crate::handler) fn install_managed_client_resolution_pause(
    pid: u32,
) -> std::sync::Arc<ManagedClientResolutionPause> {
    let pause = std::sync::Arc::new(ManagedClientResolutionPause::default());
    *MANAGED_CLIENT_RESOLUTION_PAUSE
        .lock()
        .expect("managed client resolution pause lock") = Some((pid, pause.clone()));
    pause
}

#[cfg(test)]
async fn pause_after_managed_client_resolution(client: ManagedClient) {
    let pid = match client {
        ManagedClient::Attach { pid, .. } => pid,
        ManagedClient::Control(identity) => identity.requester_pid(),
    };
    let pause = {
        let mut installed = MANAGED_CLIENT_RESOLUTION_PAUSE
            .lock()
            .expect("managed client resolution pause lock");
        let matches_pid = installed
            .as_ref()
            .is_some_and(|(paused_pid, _)| *paused_pid == pid);
        matches_pid.then(|| {
            installed
                .take()
                .expect("matching managed client resolution pause remains installed")
                .1
        })
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(not(test))]
async fn pause_after_managed_client_resolution(_client: ManagedClient) {}

impl RequestHandler {
    async fn managed_client_for_pid(
        &self,
        requester_pid: u32,
    ) -> Option<switching::SwitchManagedClientIdentity> {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let active_control = self.active_control.lock().await;
            return active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| {
                    active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
                })
                .map(|active| switching::SwitchManagedClientIdentity::Control {
                    pid: requester_pid,
                    control_id: active.id,
                });
        }
        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                return Some(switching::SwitchManagedClientIdentity::Attach {
                    pid: requester_pid,
                    attach_id: active.id,
                });
            }
        }
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .filter(|active| !active.closing.load(Ordering::SeqCst))
            .map(|active| switching::SwitchManagedClientIdentity::Control {
                pid: requester_pid,
                control_id: active.id,
            })
    }

    async fn set_attached_client_flags(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        mut flags: ClientFlags,
    ) -> Result<(), RmuxError> {
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| active.id == expected_attach_id)
            .ok_or_else(|| attached_client_required("attach-session"))?;
        if !active.can_write {
            flags = flags.with_read_only();
        }
        active.flags = flags;
        Ok(())
    }

    async fn set_control_client_ignore_size(&self, control_pid: u32) {
        if let Some(active) = self
            .active_control
            .lock()
            .await
            .by_pid
            .get_mut(&control_pid)
        {
            active.flags.ignore_size = true;
        }
    }

    pub(in crate::handler) async fn resolve_target_managed_client(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
        command_name: &str,
    ) -> Result<ManagedClient, RmuxError> {
        let expected_attach = validate_expected_attach_identity(self, requester_pid).await?;
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            self.validate_control_queue_session_identity(requester_pid, identity.control_id())
                .await?;
        }

        let Some(target_client) = target_client.map(normalize_target_client) else {
            let client = match expected_attach {
                Some(identity) => ManagedClient::Attach {
                    pid: identity.attach_pid(),
                    attach_id: identity.attach_id(),
                },
                None => {
                    self.resolve_managed_client(requester_pid, command_name)
                        .await?
                }
            };
            pause_after_managed_client_resolution(client).await;
            return Ok(client);
        };
        if target_client == "=" {
            let client = match expected_attach {
                Some(identity) => ManagedClient::Attach {
                    pid: identity.attach_pid(),
                    attach_id: identity.attach_id(),
                },
                None => {
                    self.resolve_managed_client(requester_pid, command_name)
                        .await?
                }
            };
            pause_after_managed_client_resolution(client).await;
            return Ok(client);
        }

        let attach_client = {
            let active_attach = self.active_attach.lock().await;
            if let Ok(pid) = target_client.parse::<u32>() {
                active_attach
                    .by_pid
                    .get(&pid)
                    .filter(|active| !active.closing.load(Ordering::SeqCst))
                    .map(|active| ManagedClient::Attach {
                        pid,
                        attach_id: active.id,
                    })
            } else {
                active_attach
                    .by_pid
                    .iter()
                    .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                    .find(|(_, active)| {
                        attached_client_matches_target(&active.client_name, target_client)
                    })
                    .map(|(&pid, active)| ManagedClient::Attach {
                        pid,
                        attach_id: active.id,
                    })
            }
        };
        if let Some(client) = attach_client {
            pause_after_managed_client_resolution(client).await;
            return Ok(client);
        }

        let control_client = if let Some(pid) = control_client_target_pid(target_client) {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .get(&pid)
                .filter(|active| !active.closing.load(Ordering::SeqCst))
                .map(|active| {
                    ManagedClient::Control(super::control_support::ControlClientIdentity::new(
                        pid, active.id,
                    ))
                })
        } else {
            None
        };
        if let Some(client) = control_client {
            pause_after_managed_client_resolution(client).await;
            return Ok(client);
        }

        Err(RmuxError::Server(format!(
            "can't find client: {target_client}"
        )))
    }

    pub(in crate::handler) async fn find_target_attach_client_pid(
        &self,
        requester_pid: u32,
        target_client: &str,
        command_name: &str,
    ) -> Result<Option<u32>, RmuxError> {
        let target_client = normalize_target_client(target_client);
        if target_client == "=" {
            return self
                .resolve_target_attach_client_pid(requester_pid, Some(target_client), command_name)
                .await
                .map(Some);
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Ok(pid) = target_client.parse::<u32>() {
                if active_attach.by_pid.contains_key(&pid) {
                    return Ok(Some(pid));
                }
            } else if let Some((&pid, _)) = active_attach.by_pid.iter().find(|(_, active)| {
                attached_client_matches_target(&active.client_name, target_client)
            }) {
                return Ok(Some(pid));
            }
        }

        let active_control = self.active_control.lock().await;
        if let Some(pid) = control_client_target_pid(target_client) {
            if active_control.by_pid.contains_key(&pid) {
                return Err(RmuxError::Server(format!(
                    "{command_name} requires an attached client"
                )));
            }
        }

        Ok(None)
    }

    pub(in crate::handler) async fn find_display_message_client(
        &self,
        requester_pid: u32,
        target_client: &str,
    ) -> Result<Option<ManagedClient>, RmuxError> {
        let target_client = normalize_target_client(target_client);
        if target_client == "=" {
            return self
                .resolve_target_managed_client(
                    requester_pid,
                    Some(target_client),
                    "display-message",
                )
                .await
                .map(Some);
        }

        {
            let active_attach = self.active_attach.lock().await;
            let attached = if let Ok(pid) = target_client.parse::<u32>() {
                active_attach.by_pid.get(&pid).and_then(|active| {
                    (!active.closing.load(Ordering::SeqCst)).then_some(ManagedClient::Attach {
                        pid,
                        attach_id: active.id,
                    })
                })
            } else {
                active_attach
                    .by_pid
                    .iter()
                    .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                    .find(|(_, active)| {
                        attached_client_matches_target(&active.client_name, target_client)
                    })
                    .map(|(&pid, active)| ManagedClient::Attach {
                        pid,
                        attach_id: active.id,
                    })
            };
            if attached.is_some() {
                return Ok(attached);
            }
        }

        let active_control = self.active_control.lock().await;
        Ok(control_client_target_pid(target_client).and_then(|pid| {
            active_control.by_pid.get(&pid).and_then(|active| {
                (!active.closing.load(Ordering::SeqCst)).then_some(ManagedClient::Control(
                    super::control_support::ControlClientIdentity::new(pid, active.id),
                ))
            })
        }))
    }

    pub(in crate::handler) async fn resolve_target_attach_client_pid(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
        command_name: &str,
    ) -> Result<u32, RmuxError> {
        match self
            .resolve_target_managed_client(requester_pid, target_client, command_name)
            .await?
        {
            ManagedClient::Attach {
                pid: attach_pid, ..
            } => Ok(attach_pid),
            ManagedClient::Control(_) => Err(RmuxError::Server(format!(
                "{command_name} requires an attached client"
            ))),
        }
    }

    pub(in crate::handler) async fn resolve_target_attach_client_identity(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
        command_name: &str,
    ) -> Result<ActiveAttachIdentity, RmuxError> {
        let (attach_pid, attach_id) = match self
            .resolve_target_managed_client(requester_pid, target_client, command_name)
            .await?
        {
            ManagedClient::Attach { pid, attach_id } => (pid, attach_id),
            ManagedClient::Control(_) => {
                return Err(RmuxError::Server(format!(
                    "{command_name} requires an attached client"
                )))
            }
        };
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| active.id == attach_id && !active.closing.load(Ordering::SeqCst))
            .map(|active| active.identity(attach_pid))
            .ok_or_else(|| attached_client_required(command_name))
    }

    async fn update_session_cwd_from_template(
        &self,
        session_name: &rmux_proto::SessionName,
        template: &str,
    ) -> Result<(), RmuxError> {
        let rendered = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let context = RuntimeFormatContext::new(FormatContext::from_session(session))
                .with_state(&state)
                .with_session(session);
            render_runtime_template(template, &context, false)
        };

        let mut state = self.state.lock().await;
        let session = state
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        session.set_cwd((!rendered.is_empty()).then(|| PathBuf::from(rendered)));
        Ok(())
    }

    pub(in crate::handler) async fn preferred_session_name(
        &self,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        let sessions = {
            let state = self.state.lock().await;
            let mut sessions = state
                .sessions
                .iter()
                .map(|(session_name, session)| {
                    let active_window = session.active_window_index();
                    let active_pane = session.window().active_pane_index();
                    (
                        session_name.clone(),
                        session.id(),
                        state
                            .pane_pid_in_window(session_name, active_window, active_pane)
                            .ok()
                            .map(session_selection_prefers_live_process),
                        session.recency(),
                    )
                })
                .collect::<Vec<_>>();
            sessions.sort_by(|(left, ..), (right, ..)| left.as_str().cmp(right.as_str()));
            sessions
        };
        let Some((_first_session, ..)) = sessions.first().cloned() else {
            return Err(RmuxError::Server("no sessions".to_owned()));
        };

        let mut preferred = Vec::new();
        for (session_name, session_id, live_process, recency) in &sessions {
            if self.attached_count(session_name).await == 0 {
                preferred.push((session_name.clone(), *session_id, *live_process, *recency));
            }
        }

        let candidates = if preferred.is_empty() {
            sessions
        } else {
            preferred
        };
        let candidates = if candidates
            .iter()
            .any(|(_, _, live_process, ..)| live_process.unwrap_or(false))
        {
            candidates
                .into_iter()
                .filter(|(_, _, live_process, ..)| live_process.unwrap_or(false))
                .collect::<Vec<_>>()
        } else {
            candidates
        };

        // Unattached preference and the live-process filter above have already
        // narrowed the candidate set, so what remains is a pure recency rank,
        // matching `cmd_find_session_better`. A previous attach no longer
        // outranks a later lifetime event: `touch_attached` mints a recency
        // token of its own, so attach history is expressed by the same order
        // instead of by a separate leading key.
        let (session_name, ..) = candidates
            .into_iter()
            .max_by(
                |(left_name, left_id, _, left_recency),
                 (right_name, right_id, _, right_recency)| {
                    left_recency
                        .cmp(right_recency)
                        .then(left_id.cmp(right_id))
                        .then(right_name.as_str().cmp(left_name.as_str()))
                },
            )
            .ok_or_else(|| RmuxError::Server("no sessions".to_owned()))?;

        Ok(session_name)
    }

    async fn resize_session_for_attach_client(
        &self,
        session_name: &rmux_proto::SessionName,
        client: AttachResizeClient,
    ) -> Result<(), RmuxError> {
        self.resize_session_geometry_for_attach_client(session_name, client, None, None)
            .await
    }

    /// Applies `client`'s geometry to `session_name`, counting the client once.
    ///
    /// `client` holds no attach registration, so it displaces none: it enters the
    /// selection beside every attached vote and erases nothing. A client that
    /// does hold one is moved by `commit_attached_session_switch` instead, which
    /// selects, mutates and delivers under a single lock region.
    async fn resize_session_geometry_for_attach_client(
        &self,
        session_name: &rmux_proto::SessionName,
        client: AttachResizeClient,
        expected_session_id: Option<rmux_proto::SessionId>,
        expected_switch_target: Option<&SwitchTargetSelection>,
    ) -> Result<(), RmuxError> {
        let AttachResizeClient {
            geometry: client_geometry,
            flags: client_flags,
        } = client;
        let Some(client_geometry) =
            client_geometry.filter(|geometry| geometry.size.cols > 0 && geometry.size.rows > 0)
        else {
            return Ok(());
        };
        let client_size = client_geometry.size;

        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let switch_window_target = expected_switch_target.map(SwitchTargetSelection::window_target);
        for _ in 0..4 {
            // This client owns no attach registration to renew, so its arrival
            // order is simply "now": a first attach is registered moments later
            // with the very sequence this reads, and a control client is ordered
            // from `active_control` by `set_control_session_with_expected_identity`.
            let incoming_client = Some(IncomingSizeClient::joining(
                None,
                client_size,
                client_flags,
                self.current_client_size_sequence(),
            ));
            let selection = match switch_window_target.as_ref() {
                Some(target) => {
                    self.selected_attached_window_size(target, incoming_client)
                        .await?
                }
                None => {
                    self.selected_attached_session_size(session_name, incoming_client)
                        .await?
                }
            };
            self.pause_after_attached_size_selection().await;
            if expected_session_id.is_some_and(|expected| selection.session_id != expected) {
                return Err(crate::pane_terminals::session_not_found(session_name));
            }
            let mut state = self.state.lock().await;
            if state.sessions.session(session_name).is_none() {
                return Err(crate::pane_terminals::session_not_found(session_name));
            }
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
                session_name,
                &selection,
                switch_window_target.is_none(),
            ) {
                continue;
            }
            if let Some(selection) = expected_switch_target {
                let expected_session_id = expected_session_id
                    .expect("a switch target selection carries a stable session identity");
                selection.validate_for_session_identity(
                    &state,
                    session_name,
                    expected_session_id,
                )?;
            }
            if !client_flags.contains(ClientFlags::IGNORESIZE) {
                state.set_attached_terminal_pixels(session_name, client_geometry.pixels);
            }
            let result = match switch_window_target.as_ref() {
                Some(target) => state.mutate_session_and_resize_window_terminal(
                    session_name,
                    target.window_index(),
                    |session| {
                        session.touch_attached();
                        selection.apply_to_window(session, target.window_index())
                    },
                ),
                None => state.mutate_session_and_resize_active_window_geometry(
                    session_name,
                    |session| {
                        session.touch_attached();
                        selection.apply_to_active_window(session);
                        Ok(())
                    },
                ),
            };
            drop(active_control);
            drop(active_attach);
            return result;
        }
        Err(RmuxError::Server(format!(
            "session {session_name} active window changed during attached-size selection"
        )))
    }

    pub(in crate::handler) async fn handle_list_clients(
        &self,
        requester_pid: u32,
        request: ListClientsRequest,
    ) -> Response {
        let socket_path = self.socket_path();
        let requester_uid = self.requester_uid(requester_pid).await;
        let mut clients = self.list_clients_snapshot().await;
        clients.retain(|client| client.session_name.is_some());
        if let Some(target_session) = request.target_session.as_ref() {
            clients.retain(|client| client.session_name.as_ref() == Some(target_session));
        }
        sort_list_clients(
            &mut clients,
            request.sort_order.as_deref(),
            request.reversed,
        );

        let state = self.state.lock().await;
        let lines = clients
            .iter()
            .filter_map(|client| {
                let context = client.format_bindings().apply(
                    RuntimeFormatContext::new(FormatContext::new())
                        .with_state(&state)
                        .with_socket_path(&socket_path),
                );
                let context = context
                    .with_named_value(
                        "session_id",
                        client
                            .session_id
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    )
                    .with_named_value(
                        "session_name",
                        client
                            .session_name
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    )
                    .with_named_value("uid", format_requester_uid(requester_uid));
                if let Some(filter) = request.filter.as_deref() {
                    let expanded = render_runtime_template(filter, &context, false);
                    if !is_truthy(&expanded) {
                        return None;
                    }
                }

                Some(render_runtime_template(
                    request.format.as_deref().unwrap_or(LIST_CLIENTS_TEMPLATE),
                    &context,
                    false,
                ))
            })
            .collect::<Vec<_>>();

        Response::ListClients(ListClientsResponse {
            match_count: lines.len(),
            output: command_output_from_lines(&lines),
        })
    }

    pub(in crate::handler) async fn handle_suspend_client(
        &self,
        requester_pid: u32,
        request: SuspendClientRequest,
    ) -> Response {
        let attach_pid = match self
            .resolve_target_attach_client_pid(
                requester_pid,
                request.target_client.as_deref(),
                "suspend-client",
            )
            .await
        {
            Ok(attach_pid) => attach_pid,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Response::Error(ErrorResponse {
                error: attached_client_required("suspend-client"),
            });
        };
        active.suspended = true;
        let session_name = active.session_name.clone();
        let stale_client = active
            .control_tx
            .send(AttachControl::Suspend)
            .is_err()
            .then(|| active.identity(attach_pid));
        drop(active_attach);
        if let Some(stale_client) = stale_client {
            let removed_stale_clients = self
                .remove_attached_clients_for_session(&session_name, vec![stale_client])
                .await;
            if !removed_stale_clients.is_empty() {
                let _ = self
                    .reconcile_attached_session_size_and_emit(&session_name)
                    .await;
            }
        }

        Response::SuspendClient(SuspendClientResponse {
            target_client: attach_pid.to_string(),
        })
    }
}
