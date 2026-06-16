use std::time::Instant;

use rmux_core::LifecycleEvent;
use rmux_proto::request::{SwitchClientExt2Request, SwitchClientExt3Request};
use rmux_proto::{ErrorResponse, OptionName, Response, RmuxError, SwitchClientResponse, Target};

use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;
use crate::pane_terminals::session_not_found;

#[cfg(feature = "web")]
use super::super::attach_support::attach_render_target_for_session;
use super::super::{
    attach_support::attach_target_for_session, client_environment_snapshot,
    control_support::ManagedClient, parse_session_sort_order, switch_target_selector_count,
    update_environment_from_client, RequestHandler, SessionSortOrder,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_switch_client(
        &self,
        requester_pid: u32,
        request: rmux_proto::SwitchClientRequest,
    ) -> Response {
        self.handle_switch_client_ext3(
            requester_pid,
            SwitchClientExt3Request {
                target_client: None,
                target: Some(request.target.to_string()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext(
        &self,
        requester_pid: u32,
        request: rmux_proto::SwitchClientExtRequest,
    ) -> Response {
        self.handle_switch_client_ext3(
            requester_pid,
            SwitchClientExt3Request {
                target_client: None,
                target: request.target.map(|target| target.to_string()),
                key_table: request.key_table,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext2(
        &self,
        requester_pid: u32,
        request: SwitchClientExt2Request,
    ) -> Response {
        self.handle_switch_client_ext3(
            requester_pid,
            SwitchClientExt3Request {
                target_client: None,
                target: request.target.map(|target| target.to_string()),
                key_table: request.key_table,
                last_session: request.last_session,
                next_session: request.next_session,
                previous_session: request.previous_session,
                toggle_read_only: request.toggle_read_only,
                sort_order: request.sort_order,
                skip_environment_update: request.skip_environment_update,
                zoom: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_switch_client_ext3(
        &self,
        requester_pid: u32,
        request: SwitchClientExt3Request,
    ) -> Response {
        let client = match self
            .resolve_target_managed_client(
                requester_pid,
                request.target_client.as_deref(),
                "switch-client",
            )
            .await
        {
            Ok(client) => client,
            Err(error)
                if request.target_client.is_none()
                    && matches!(
                        &error,
                        RmuxError::Server(message)
                            if message == "switch-client requires an attached client"
                    ) =>
            {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Message("no current client".to_owned()),
                });
            }
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        if switch_target_selector_count(&request) > 1 {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "switch-client accepts only one of -t, -l, -n, or -p".to_owned(),
                ),
            });
        }
        if switch_target_selector_count(&request) == 0
            && request.key_table.is_none()
            && !request.toggle_read_only
        {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "switch-client requires -t target, -T key-table, -l, -n, -p, or -r".to_owned(),
                ),
            });
        }

        if let ManagedClient::Attach(attach_pid) = client {
            // tmux clears repeat state and key table for non-repeat invocations. A new
            // -T table is installed below after stale repeat state has been flushed.
            if request.key_table.is_none() {
                let _ = self.set_attached_key_table(attach_pid, None, None).await;
            }
            let mut active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach.by_pid.get_mut(&attach_pid) {
                active.repeat_active = false;
                active.repeat_deadline = None;
                active.last_key = None;
            }
        }

        let mut session_name = match self.current_managed_session_name(client).await {
            Ok(session_name) => Some(session_name),
            Err(error) if switch_target_selector_count(&request) == 0 => {
                return Response::Error(ErrorResponse { error });
            }
            Err(_) => None,
        };

        let switch_target = if let Some(target) = request.target.as_deref() {
            match self.apply_switch_target(target, request.zoom).await {
                Ok(session_name) => Some(session_name),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else if request.last_session {
            match client {
                ManagedClient::Attach(attach_pid) => {
                    let active_attach = self.active_attach.lock().await;
                    match active_attach.last_session_for_client(attach_pid) {
                        Ok(session_name) => session_name,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    }
                }
                ManagedClient::Control(control_pid) => self.control_last_session(control_pid).await,
            }
        } else if request.next_session || request.previous_session {
            match self
                .adjacent_session_name(
                    session_name.as_ref(),
                    request.next_session,
                    request.sort_order.as_deref(),
                )
                .await
            {
                Ok(session_name) => session_name,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };

        if let Some(target_session) = switch_target {
            let response = self
                .switch_managed_client_to_session(
                    requester_pid,
                    client,
                    target_session.clone(),
                    request.skip_environment_update,
                )
                .await;
            let Response::SwitchClient(_) = &response else {
                return response;
            };
            session_name = Some(target_session);
        }

        let Some(session_name) = session_name else {
            return Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            });
        };

        if let Some(key_table) = request.key_table {
            let ManagedClient::Attach(attach_pid) = client else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -T is not available for control clients".to_owned(),
                    ),
                });
            };
            if let Err(error) = self
                .apply_attached_key_table(attach_pid, &session_name, key_table)
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        if request.toggle_read_only {
            let ManagedClient::Attach(attach_pid) = client else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -r is not available for control clients".to_owned(),
                    ),
                });
            };
            let mut active_attach = self.active_attach.lock().await;
            if let Err(error) = active_attach.toggle_read_only(attach_pid) {
                return Response::Error(ErrorResponse { error });
            }
        }

        Response::SwitchClient(SwitchClientResponse { session_name })
    }

    async fn current_managed_session_name(
        &self,
        client: ManagedClient,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        match client {
            ManagedClient::Attach(attach_pid) => {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .map(|active| active.session_name.clone())
                    .ok_or_else(|| attached_client_required("switch-client"))
            }
            ManagedClient::Control(control_pid) => self
                .control_session_name(control_pid)
                .await
                .ok_or_else(|| attached_client_required("switch-client")),
        }
    }

    pub(super) async fn switch_managed_client_to_session(
        &self,
        requester_pid: u32,
        client: ManagedClient,
        session_name: rmux_proto::SessionName,
        skip_environment_update: bool,
    ) -> Response {
        if !skip_environment_update {
            if let Some(client_environment) = client_environment_snapshot(requester_pid) {
                let mut state = self.state.lock().await;
                update_environment_from_client(&mut state, &session_name, &client_environment);
            }
        }
        let attached_count = self
            .attached_count_after_switch(&session_name, client)
            .await;

        match client {
            ManagedClient::Attach(attach_pid) => {
                let Some((terminal_context, client_size, client_pixels, render_stream)) = self
                    .terminal_context_and_size_for_attached_client(attach_pid)
                    .await
                else {
                    return Response::Error(ErrorResponse {
                        error: attached_client_required("switch-client"),
                    });
                };
                if let Err(error) = self
                    .resize_session_geometry_for_attach_client(
                        &session_name,
                        Some(rmux_proto::TerminalGeometry {
                            size: client_size,
                            pixels: client_pixels,
                        }),
                    )
                    .await
                {
                    return Response::Error(ErrorResponse { error });
                }
                let target = {
                    let state = self.state.lock().await;
                    #[cfg(feature = "web")]
                    let result = if render_stream {
                        attach_render_target_for_session(
                            &state,
                            &session_name,
                            attached_count,
                            &terminal_context,
                            &self.socket_path(),
                        )
                    } else {
                        attach_target_for_session(
                            &state,
                            &session_name,
                            attached_count,
                            &terminal_context,
                            &self.socket_path(),
                        )
                    };
                    #[cfg(not(feature = "web"))]
                    let result = {
                        let _ = render_stream;
                        attach_target_for_session(
                            &state,
                            &session_name,
                            attached_count,
                            &terminal_context,
                            &self.socket_path(),
                        )
                    };
                    match result {
                        Ok(target) => target,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    }
                };

                match self
                    .send_attach_control(
                        attach_pid,
                        AttachControl::switch(target),
                        "switch-client",
                        Some(session_name.clone()),
                    )
                    .await
                {
                    Ok(_previous_session_name) => {
                        let mut state = self.state.lock().await;
                        if let Some(session) = state.sessions.session_mut(&session_name) {
                            session.touch_attached();
                        }
                        drop(state);
                        self.emit(LifecycleEvent::ClientSessionChanged {
                            session_name: session_name.clone(),
                            client_name: Some(attach_pid.to_string()),
                        })
                        .await;
                        Response::SwitchClient(SwitchClientResponse { session_name })
                    }
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            }
            ManagedClient::Control(control_pid) => {
                {
                    let mut state = self.state.lock().await;
                    let Some(session) = state.sessions.session_mut(&session_name) else {
                        return Response::Error(ErrorResponse {
                            error: RmuxError::SessionNotFound(session_name.to_string()),
                        });
                    };
                    session.touch_attached();
                }

                match self
                    .set_control_session(control_pid, Some(session_name.clone()))
                    .await
                {
                    Ok(_previous_session_name) => {
                        self.emit(LifecycleEvent::ClientSessionChanged {
                            session_name: session_name.clone(),
                            client_name: Some(control_pid.to_string()),
                        })
                        .await;
                        Response::SwitchClient(SwitchClientResponse { session_name })
                    }
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            }
        }
    }

    async fn apply_attached_key_table(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
        key_table: String,
    ) -> Result<(), RmuxError> {
        let key_table_set_at = Instant::now();
        self.set_attached_key_table(attach_pid, Some(key_table.clone()), Some(key_table_set_at))
            .await?;
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(attached_client_required("switch-client"));
        };
        active.repeat_active = false;
        active.repeat_deadline = None;
        active.last_key = None;
        drop(active_attach);

        if key_table == "prefix" {
            let prefix_timeout_ms = {
                let state = self.state.lock().await;
                state
                    .options
                    .resolve(Some(session_name), OptionName::PrefixTimeout)
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            if prefix_timeout_ms != 0 {
                self.schedule_attached_prefix_timeout(
                    attach_pid,
                    key_table_set_at,
                    prefix_timeout_ms,
                );
            }
        }

        Ok(())
    }

    pub(super) async fn apply_switch_target(
        &self,
        target: &str,
        zoom: bool,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        match Target::parse(target)? {
            Target::Session(session_name) => {
                let state = self.state.lock().await;
                if state.sessions.session(&session_name).is_none() {
                    return Err(session_not_found(&session_name));
                }
                Ok(session_name)
            }
            Target::Window(target) => {
                let mut state = self.state.lock().await;
                let session = state
                    .sessions
                    .session_mut(target.session_name())
                    .ok_or_else(|| session_not_found(target.session_name()))?;
                session.select_window(target.window_index())?;
                Ok(target.session_name().clone())
            }
            Target::Pane(target) => {
                let mut state = self.state.lock().await;
                let session = state
                    .sessions
                    .session_mut(target.session_name())
                    .ok_or_else(|| session_not_found(target.session_name()))?;
                let (was_zoomed, zoom_pane) = {
                    let window = session.window_at(target.window_index()).ok_or_else(|| {
                        RmuxError::invalid_target(
                            target.to_string(),
                            "window index does not exist in session",
                        )
                    })?;
                    (window.is_zoomed(), window.active_pane_index())
                };
                if was_zoomed && zoom {
                    session.toggle_zoom_in_window(target.window_index(), zoom_pane)?;
                }
                session.select_window(target.window_index())?;
                session.select_pane_in_window(target.window_index(), target.pane_index())?;
                if was_zoomed && zoom {
                    session.toggle_zoom_in_window(target.window_index(), target.pane_index())?;
                }
                Ok(target.session_name().clone())
            }
        }
    }

    async fn adjacent_session_name(
        &self,
        current_session: Option<&rmux_proto::SessionName>,
        forward: bool,
        sort_order: Option<&str>,
    ) -> Result<Option<rmux_proto::SessionName>, RmuxError> {
        let session_names = {
            let state = self.state.lock().await;
            let mut sessions = state
                .sessions
                .iter()
                .map(|(session_name, session)| {
                    (
                        session_name.clone(),
                        session.created_at(),
                        session.activity_at(),
                        session.window().size().cols,
                        session.id(),
                    )
                })
                .collect::<Vec<_>>();
            sessions.sort_by(|left, right| {
                let ordering = match parse_session_sort_order(sort_order) {
                    Some(SessionSortOrder::Activity) => left.2.cmp(&right.2),
                    Some(SessionSortOrder::Creation) => left.1.cmp(&right.1),
                    Some(SessionSortOrder::Index) => left.4.cmp(&right.4),
                    Some(SessionSortOrder::Size) => left.3.cmp(&right.3),
                    Some(
                        SessionSortOrder::Name
                        | SessionSortOrder::Modifier
                        | SessionSortOrder::Order,
                    )
                    | None => left.0.as_str().cmp(right.0.as_str()),
                };
                if ordering.is_eq() {
                    left.4.cmp(&right.4)
                } else {
                    ordering
                }
            });
            sessions
                .into_iter()
                .map(|(session_name, ..)| session_name)
                .collect::<Vec<_>>()
        };
        if session_names.is_empty() {
            return Err(RmuxError::Server("no sessions".to_owned()));
        }

        let index = current_session
            .and_then(|current| {
                session_names
                    .iter()
                    .position(|candidate| candidate == current)
            })
            .unwrap_or(0);
        let next_index = if forward {
            (index + 1) % session_names.len()
        } else if index == 0 {
            session_names.len().saturating_sub(1)
        } else {
            index - 1
        };
        Ok(session_names.get(next_index).cloned())
    }
}
