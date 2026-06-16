use std::path::PathBuf;

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
    attach_support::ClientFlags, attached_client_matches_target, command_output_from_lines,
    control_support::ManagedClient, format_client_uid, format_client_user, format_requester_uid,
    normalize_target_client, session_selection_prefers_live_process, sort_list_clients,
    RequestHandler, LIST_CLIENTS_TEMPLATE,
};

#[path = "handler_client/attach.rs"]
mod attach;
#[path = "handler_client/detach.rs"]
mod detach;
#[path = "handler_client/refresh.rs"]
mod refresh;
#[path = "handler_client/switching.rs"]
mod switching;

impl RequestHandler {
    async fn managed_client_for_pid(&self, requester_pid: u32) -> Option<ManagedClient> {
        {
            let active_attach = self.active_attach.lock().await;
            if active_attach.by_pid.contains_key(&requester_pid) {
                return Some(ManagedClient::Attach(requester_pid));
            }
        }
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .contains_key(&requester_pid)
            .then_some(ManagedClient::Control(requester_pid))
    }

    async fn set_attached_client_flags(
        &self,
        attach_pid: u32,
        mut flags: ClientFlags,
    ) -> Result<(), RmuxError> {
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .ok_or_else(|| attached_client_required("attach-session"))?;
        if !active.can_write {
            flags = flags.with_read_only();
        }
        active.flags = flags;
        Ok(())
    }

    pub(in crate::handler) async fn resolve_target_managed_client(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
        command_name: &str,
    ) -> Result<ManagedClient, RmuxError> {
        let Some(target_client) = target_client.map(normalize_target_client) else {
            return self
                .resolve_managed_client(requester_pid, command_name)
                .await;
        };
        if target_client == "=" {
            return self
                .resolve_managed_client(requester_pid, command_name)
                .await;
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Ok(pid) = target_client.parse::<u32>() {
                if active_attach.by_pid.contains_key(&pid) {
                    return Ok(ManagedClient::Attach(pid));
                }
            } else if let Some((&pid, _)) = active_attach
                .by_pid
                .iter()
                .find(|(pid, _)| attached_client_matches_target(**pid, target_client))
            {
                return Ok(ManagedClient::Attach(pid));
            }
        }

        let active_control = self.active_control.lock().await;
        if let Ok(pid) = target_client.parse::<u32>() {
            if active_control.by_pid.contains_key(&pid) {
                return Ok(ManagedClient::Control(pid));
            }
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
            } else if let Some((&pid, _)) = active_attach
                .by_pid
                .iter()
                .find(|(pid, _)| attached_client_matches_target(**pid, target_client))
            {
                return Ok(Some(pid));
            }
        }

        let active_control = self.active_control.lock().await;
        if let Ok(pid) = target_client.parse::<u32>() {
            if active_control.by_pid.contains_key(&pid) {
                return Err(RmuxError::Server(format!(
                    "{command_name} requires an attached client"
                )));
            }
        }

        Ok(None)
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
            ManagedClient::Attach(attach_pid) => Ok(attach_pid),
            ManagedClient::Control(_) => Err(RmuxError::Server(format!(
                "{command_name} requires an attached client"
            ))),
        }
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
                        session.last_attached_at(),
                        session.activity_at(),
                        session.created_at(),
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
        for (session_name, session_id, live_process, last_attached_at, activity_at, created_at) in
            &sessions
        {
            if self.attached_count(session_name).await == 0 {
                preferred.push((
                    session_name.clone(),
                    *session_id,
                    *live_process,
                    *last_attached_at,
                    *activity_at,
                    *created_at,
                ));
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

        let (session_name, ..) = candidates
            .into_iter()
            .max_by(
                |(left_name, left_id, _, left_attached, left_activity, left_created),
                 (right_name, right_id, _, right_attached, right_activity, right_created)| {
                    left_attached
                        .unwrap_or(i64::MIN)
                        .cmp(&right_attached.unwrap_or(i64::MIN))
                        .then(
                            left_activity
                                .cmp(right_activity)
                                .then(left_created.cmp(right_created))
                                .then(left_id.cmp(right_id))
                                .then(right_name.as_str().cmp(left_name.as_str())),
                        )
                },
            )
            .ok_or_else(|| RmuxError::Server("no sessions".to_owned()))?;

        Ok(session_name)
    }

    async fn resize_session_for_attach_client(
        &self,
        session_name: &rmux_proto::SessionName,
        client_size: Option<TerminalSize>,
    ) -> Result<(), RmuxError> {
        self.resize_session_geometry_for_attach_client(
            session_name,
            client_size.map(TerminalGeometry::from_size),
        )
        .await
    }

    async fn resize_session_geometry_for_attach_client(
        &self,
        session_name: &rmux_proto::SessionName,
        client_geometry: Option<TerminalGeometry>,
    ) -> Result<(), RmuxError> {
        let Some(client_geometry) =
            client_geometry.filter(|geometry| geometry.size.cols > 0 && geometry.size.rows > 0)
        else {
            return Ok(());
        };
        let client_size = client_geometry.size;

        let mut state = self.state.lock().await;
        state.set_attached_terminal_pixels(session_name, client_geometry.pixels);
        state.mutate_session_and_resize_terminals(session_name, |session| {
            session.touch_attached();
            session.resize_terminal(client_size);
            Ok(())
        })
    }

    pub(in crate::handler) async fn handle_list_clients(
        &self,
        requester_pid: u32,
        request: ListClientsRequest,
    ) -> Response {
        let requester_uid = self.requester_uid(requester_pid).await;
        let mut clients = self.list_clients_snapshot().await;
        if let Some(target_session) = request.target_session.as_ref() {
            clients.retain(|client| client.session_name.as_ref() == Some(target_session));
        }
        sort_list_clients(
            &mut clients,
            request.sort_order.as_deref(),
            request.reversed,
        );

        let lines = clients
            .iter()
            .filter_map(|client| {
                let context = RuntimeFormatContext::new(FormatContext::new())
                    .with_named_value("client_name", client.name.clone())
                    .with_named_value("client_pid", client.pid.to_string())
                    .with_named_value("client_tty", client.tty.clone())
                    .with_named_value(
                        "session_name",
                        client
                            .session_name
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    )
                    .with_named_value(
                        "client_session",
                        client
                            .session_name
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    )
                    .with_named_value("client_width", client.width.to_string())
                    .with_named_value("client_height", client.height.to_string())
                    .with_named_value("client_termfeatures", client.termfeatures.clone())
                    .with_named_value("client_termname", client.termname.clone())
                    .with_named_value("client_termtype", client.termtype.clone())
                    .with_named_value("client_key_table", client.key_table_name())
                    .with_named_value("client_prefix", client.prefix_value())
                    .with_named_value("client_uid", format_client_uid(client.uid))
                    .with_named_value("client_user", format_client_user(client.uid, &client.user))
                    .with_named_value("client_utf8", if client.utf8 { "1" } else { "0" })
                    .with_named_value(
                        "client_control_mode",
                        if client.control { "1" } else { "0" },
                    )
                    .with_named_value("client_flags", client.flags.clone())
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
        if active.control_tx.send(AttachControl::Suspend).is_err() {
            active_attach.by_pid.remove(&attach_pid);
        }

        Response::SuspendClient(SuspendClientResponse {
            target_client: attach_pid.to_string(),
        })
    }
}
