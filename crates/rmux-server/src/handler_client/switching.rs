use std::sync::atomic::Ordering;
use std::time::Instant;

use rmux_core::{TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget};
use rmux_proto::request::{SwitchClientExt2Request, SwitchClientExt3Request};
use rmux_proto::{
    ErrorResponse, OptionName, PaneId, PaneTarget, Response, RmuxError, SessionId, SessionName,
    SwitchClientResponse, Target, TerminalGeometry, WindowId, WindowTarget,
};

use crate::handler_support::{ambiguous_attached_client, attached_client_required};
#[cfg(test)]
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, HandlerState};

use super::super::{
    active_session_target,
    attach_support::AttachedSwitchCommitRequest,
    attached_client_matches_target, client_environment_snapshot,
    control_support::{current_control_queue_identity, ManagedClient},
    normalize_target_client, parse_session_sort_order, switch_client_target_find_type,
    switch_target_selector_count, with_visible_pane_bases, RequestHandler, SessionSortOrder,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SwitchSessionIdentity {
    pub(super) session_name: SessionName,
    pub(super) session_id: SessionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::handler) enum SwitchTargetSelection {
    Window {
        target: WindowTarget,
        window_id: WindowId,
    },
    Pane {
        target: PaneTarget,
        window_id: WindowId,
        pane_id: PaneId,
        zoom: bool,
    },
}

impl SwitchTargetSelection {
    pub(in crate::handler) fn session_name(&self) -> &SessionName {
        match self {
            Self::Window { target, .. } => target.session_name(),
            Self::Pane { target, .. } => target.session_name(),
        }
    }

    pub(in crate::handler) fn window_target(&self) -> WindowTarget {
        let window_index = match self {
            Self::Window { target, .. } => target.window_index(),
            Self::Pane { target, .. } => target.window_index(),
        };
        WindowTarget::with_window(self.session_name().clone(), window_index)
    }

    pub(in crate::handler) fn validate_for_session_identity(
        &self,
        state: &HandlerState,
        expected_session_name: &SessionName,
        expected_session_id: SessionId,
    ) -> Result<(), RmuxError> {
        if self.session_name() != expected_session_name {
            return Err(RmuxError::Server(
                "switch target selection changed sessions before commit".to_owned(),
            ));
        }
        let session = state
            .sessions
            .session(expected_session_name)
            .filter(|session| session.id() == expected_session_id)
            .ok_or_else(|| session_not_found(expected_session_name))?;
        let mut preview = session.clone();
        self.apply_to_session(&mut preview)
    }

    pub(in crate::handler) fn apply_to_state(
        &self,
        state: &mut HandlerState,
    ) -> Result<(), RmuxError> {
        let session_name = self.session_name().clone();
        let session = state
            .sessions
            .session_mut(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        self.apply_to_session(session)
    }

    pub(in crate::handler) fn apply_to_session(
        &self,
        session: &mut rmux_core::Session,
    ) -> Result<(), RmuxError> {
        match self {
            Self::Window { target, window_id } => {
                let window = session.window_at(target.window_index()).ok_or_else(|| {
                    RmuxError::invalid_target(
                        target.to_string(),
                        "window index does not exist in session",
                    )
                })?;
                if window.id() != *window_id {
                    return Err(RmuxError::invalid_target(
                        target.to_string(),
                        "window identity changed before switch commit",
                    ));
                }
                session.select_window(target.window_index())
            }
            Self::Pane {
                target,
                window_id,
                pane_id,
                zoom,
            } => {
                let (was_zoomed, zoom_pane) = {
                    let window = session.window_at(target.window_index()).ok_or_else(|| {
                        RmuxError::invalid_target(
                            target.to_string(),
                            "window index does not exist in session",
                        )
                    })?;
                    if window.id() != *window_id {
                        return Err(RmuxError::invalid_target(
                            target.to_string(),
                            "window identity changed before switch commit",
                        ));
                    }
                    let pane = window.pane(target.pane_index()).ok_or_else(|| {
                        RmuxError::invalid_target(
                            target.to_string(),
                            "pane index does not exist in session",
                        )
                    })?;
                    if pane.id() != *pane_id {
                        return Err(RmuxError::invalid_target(
                            target.to_string(),
                            "pane identity changed before switch commit",
                        ));
                    }
                    (window.is_zoomed(), window.active_pane_index())
                };
                if was_zoomed && *zoom {
                    session.toggle_zoom_in_window(target.window_index(), zoom_pane)?;
                }
                session.select_window(target.window_index())?;
                session.select_pane_in_window(target.window_index(), target.pane_index())?;
                if was_zoomed && *zoom {
                    session.toggle_zoom_in_window(target.window_index(), target.pane_index())?;
                }
                Ok(())
            }
        }?;
        let selected_size = session.window().size();
        session.resize_active_window_terminal(selected_size);
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedSwitchTarget {
    session: SwitchSessionIdentity,
    selection: Option<SwitchTargetSelection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SwitchManagedClientIdentity {
    Attach { pid: u32, attach_id: u64 },
    Control { pid: u32, control_id: u64 },
}

impl SwitchManagedClientIdentity {
    const fn client(self) -> ManagedClient {
        match self {
            Self::Attach { pid, attach_id } => ManagedClient::Attach { pid, attach_id },
            Self::Control { pid, control_id } => ManagedClient::Control(
                super::super::control_support::ControlClientIdentity::new(pid, control_id),
            ),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
struct SwitchTargetIdentityPause {
    reached: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

#[cfg(test)]
static SWITCH_TARGET_IDENTITY_PAUSE: std::sync::Mutex<
    Vec<(SessionName, std::sync::Arc<SwitchTargetIdentityPause>)>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn install_switch_target_identity_pause(
    session_name: SessionName,
) -> std::sync::Arc<SwitchTargetIdentityPause> {
    let pause = std::sync::Arc::new(SwitchTargetIdentityPause::default());
    let mut pauses = SWITCH_TARGET_IDENTITY_PAUSE
        .lock()
        .expect("switch target identity pause lock");
    pauses.retain(|(paused_session, _)| paused_session != &session_name);
    pauses.push((session_name, pause.clone()));
    pause
}

#[cfg(test)]
pub(super) async fn pause_after_switch_target_identity_capture(session_name: &SessionName) {
    let pause = SWITCH_TARGET_IDENTITY_PAUSE
        .lock()
        .expect("switch target identity pause lock")
        .iter()
        .filter(|(paused_session, _)| paused_session == session_name)
        .map(|(_, pause)| pause.clone())
        .next();
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
    let mut installed = SWITCH_TARGET_IDENTITY_PAUSE
        .lock()
        .expect("switch target identity pause lock");
    installed.retain(|(paused_session, current)| {
        paused_session != session_name || !std::sync::Arc::ptr_eq(current, &pause)
    });
}

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
            .resolve_switch_managed_client_identity(requester_pid, request.target_client.as_deref())
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
        if matches!(client, SwitchManagedClientIdentity::Control { .. }) {
            if request.key_table.is_some() {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -T is not available for control clients".to_owned(),
                    ),
                });
            }
            if request.toggle_read_only {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -r is not available for control clients".to_owned(),
                    ),
                });
            }
        }

        if let SwitchManagedClientIdentity::Attach {
            pid: attach_pid,
            attach_id,
        } = client
        {
            // tmux clears repeat state and key table for non-repeat invocations. A new
            // -T table is installed below after stale repeat state has been flushed.
            if request.key_table.is_none() {
                if let Err(error) = self
                    .set_attached_key_table_for_client_identity(attach_pid, attach_id, None, None)
                    .await
                {
                    return Response::Error(ErrorResponse { error });
                }
            }
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| active.id == attach_id)
            else {
                return Response::Error(ErrorResponse {
                    error: attached_client_required("switch-client"),
                });
            };
            active.repeat_active = false;
            active.repeat_deadline = None;
            active.last_key = None;
        }

        let current_session = match self.current_managed_session_identity(client).await {
            Ok(identity) => Some(identity),
            Err(error) if switch_target_selector_count(&request) == 0 => {
                return Response::Error(ErrorResponse { error });
            }
            Err(_) => None,
        };
        let mut session_name = current_session
            .as_ref()
            .map(|identity| identity.session_name.clone());

        let switch_target = if let Some(target) = request.target.as_deref() {
            match self
                .apply_switch_target_for_client_identity(
                    target,
                    current_session.as_ref(),
                    TargetFindFlags::NONE,
                    request.zoom,
                    client,
                )
                .await
            {
                Ok(session_name) => Some(session_name),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else if request.last_session {
            match client {
                SwitchManagedClientIdentity::Attach {
                    pid: attach_pid,
                    attach_id,
                } => {
                    let active_attach = self.active_attach.lock().await;
                    let Some(active) = active_attach
                        .by_pid
                        .get(&attach_pid)
                        .filter(|active| active.id == attach_id)
                    else {
                        return Response::Error(ErrorResponse {
                            error: attached_client_required("switch-client"),
                        });
                    };
                    active.last_session.clone().zip(active.last_session_id).map(
                        |(session_name, session_id)| ResolvedSwitchTarget {
                            session: SwitchSessionIdentity {
                                session_name,
                                session_id,
                            },
                            selection: None,
                        },
                    )
                }
                SwitchManagedClientIdentity::Control {
                    pid: control_pid,
                    control_id,
                } => {
                    let state = self.state.lock().await;
                    let active_control = self.active_control.lock().await;
                    let Some(active) = active_control
                        .by_pid
                        .get(&control_pid)
                        .filter(|active| active.id == control_id)
                    else {
                        return Response::Error(ErrorResponse {
                            error: attached_client_required("switch-client"),
                        });
                    };
                    active
                        .last_session
                        .clone()
                        .zip(active.last_session_id)
                        .filter(|(session_name, session_id)| {
                            state
                                .sessions
                                .session(session_name)
                                .is_some_and(|session| session.id() == *session_id)
                        })
                        .map(|(session_name, session_id)| ResolvedSwitchTarget {
                            session: SwitchSessionIdentity {
                                session_name,
                                session_id,
                            },
                            selection: None,
                        })
                }
            }
        } else if request.next_session || request.previous_session {
            match self
                .adjacent_session_name(
                    current_session.as_ref(),
                    request.next_session,
                    request.sort_order.as_deref(),
                )
                .await
            {
                Ok(session_name) => session_name.map(|session| ResolvedSwitchTarget {
                    session,
                    selection: None,
                }),
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };

        if let Some(target_session) = switch_target {
            #[cfg(test)]
            pause_after_switch_target_identity_capture(&target_session.session.session_name).await;
            let target_session_name = target_session.session.session_name.clone();
            let response = self
                .switch_managed_client_to_session_identity(
                    requester_pid,
                    client,
                    target_session.session,
                    target_session.selection,
                    request.skip_environment_update,
                )
                .await;
            let Response::SwitchClient(_) = &response else {
                return response;
            };
            session_name = Some(target_session_name);
        }

        let Some(session_name) = session_name else {
            return Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            });
        };

        if let Some(key_table) = request.key_table {
            let SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } = client
            else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -T is not available for control clients".to_owned(),
                    ),
                });
            };
            if let Err(error) = self
                .apply_attached_key_table(attach_pid, attach_id, &session_name, key_table)
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        if request.toggle_read_only {
            let SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } = client
            else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "switch-client -r is not available for control clients".to_owned(),
                    ),
                });
            };
            let mut active_attach = self.active_attach.lock().await;
            if let Err(error) = active_attach.toggle_read_only_for_identity(attach_pid, attach_id) {
                return Response::Error(ErrorResponse { error });
            }
        }

        Response::SwitchClient(SwitchClientResponse { session_name })
    }

    async fn current_managed_session_identity(
        &self,
        client: SwitchManagedClientIdentity,
    ) -> Result<SwitchSessionIdentity, RmuxError> {
        match client {
            SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                let state = self.state.lock().await;
                let active_attach = self.active_attach.lock().await;
                let active = active_attach
                    .by_pid
                    .get(&attach_pid)
                    .filter(|active| active.id == attach_id)
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                if state
                    .sessions
                    .session(&active.session_name)
                    .is_none_or(|session| session.id() != active.session_id)
                {
                    return Err(session_not_found(&active.session_name));
                }
                Ok(SwitchSessionIdentity {
                    session_name: active.session_name.clone(),
                    session_id: active.session_id,
                })
            }
            SwitchManagedClientIdentity::Control {
                pid: control_pid,
                control_id,
            } => {
                let state = self.state.lock().await;
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&control_pid)
                    .filter(|active| {
                        active.id == control_id && !active.closing.load(Ordering::SeqCst)
                    })
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                let session_name = active
                    .session_name
                    .clone()
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                let session_id = active
                    .session_id
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                if state
                    .sessions
                    .session(&session_name)
                    .is_none_or(|session| session.id() != session_id)
                {
                    return Err(session_not_found(&session_name));
                }
                Ok(SwitchSessionIdentity {
                    session_name,
                    session_id,
                })
            }
        }
    }

    async fn resolve_switch_managed_client_identity(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
    ) -> Result<SwitchManagedClientIdentity, RmuxError> {
        let target_client = target_client.map(normalize_target_client);
        if target_client.is_none() || target_client == Some("=") {
            if let Some(identity) = current_control_queue_identity(requester_pid) {
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&requester_pid)
                    .filter(|active| {
                        active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
                    })
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                return Ok(SwitchManagedClientIdentity::Control {
                    pid: requester_pid,
                    control_id: active.id,
                });
            }
            {
                let active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                    return Ok(SwitchManagedClientIdentity::Attach {
                        pid: requester_pid,
                        attach_id: active.id,
                    });
                }
            }
            {
                let active_control = self.active_control.lock().await;
                if let Some(active) = active_control.by_pid.get(&requester_pid) {
                    if active.closing.load(Ordering::SeqCst) {
                        return Err(attached_client_required("switch-client"));
                    }
                    return Ok(SwitchManagedClientIdentity::Control {
                        pid: requester_pid,
                        control_id: active.id,
                    });
                }
            }

            let attach_candidates = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .iter()
                    .map(|(&pid, active)| SwitchManagedClientIdentity::Attach {
                        pid,
                        attach_id: active.id,
                    })
                    .collect::<Vec<_>>()
            };
            let control_candidates = {
                let active_control = self.active_control.lock().await;
                active_control
                    .by_pid
                    .iter()
                    .map(|(&pid, active)| SwitchManagedClientIdentity::Control {
                        pid,
                        control_id: active.id,
                    })
                    .collect::<Vec<_>>()
            };
            let candidate = match attach_candidates.len() + control_candidates.len() {
                0 => Err(attached_client_required("switch-client")),
                1 => Ok(attach_candidates
                    .into_iter()
                    .chain(control_candidates)
                    .next()
                    .expect("one switch-client candidate exists")),
                _ => Err(ambiguous_attached_client("switch-client")),
            }?;
            self.validate_switch_managed_client_identity(candidate)
                .await?;
            return Ok(candidate);
        }

        let target_client = target_client.expect("target client was normalized above");
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            self.validate_control_queue_session_identity(requester_pid, identity.control_id())
                .await?;
        }
        {
            let active_attach = self.active_attach.lock().await;
            let matched = if let Ok(pid) = target_client.parse::<u32>() {
                active_attach.by_pid.get(&pid).map(|active| (pid, active))
            } else {
                active_attach.by_pid.iter().find_map(|(&pid, active)| {
                    attached_client_matches_target(pid, target_client).then_some((pid, active))
                })
            };
            if let Some((pid, active)) = matched {
                return Ok(SwitchManagedClientIdentity::Attach {
                    pid,
                    attach_id: active.id,
                });
            }
        }
        if let Ok(pid) = target_client.parse::<u32>() {
            let active_control = self.active_control.lock().await;
            if let Some(active) = active_control.by_pid.get(&pid) {
                if active.closing.load(Ordering::SeqCst) {
                    return Err(attached_client_required("switch-client"));
                }
                return Ok(SwitchManagedClientIdentity::Control {
                    pid,
                    control_id: active.id,
                });
            }
        }

        Err(RmuxError::Server(format!(
            "can't find client: {target_client}"
        )))
    }

    async fn validate_switch_managed_client_identity(
        &self,
        client: SwitchManagedClientIdentity,
    ) -> Result<(), RmuxError> {
        let current = match client {
            SwitchManagedClientIdentity::Attach { pid, attach_id } => {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&pid)
                    .is_some_and(|active| active.id == attach_id)
            }
            SwitchManagedClientIdentity::Control { pid, control_id } => {
                let active_control = self.active_control.lock().await;
                active_control.by_pid.get(&pid).is_some_and(|active| {
                    active.id == control_id && !active.closing.load(Ordering::SeqCst)
                })
            }
        };
        current
            .then_some(())
            .ok_or_else(|| attached_client_required("switch-client"))
    }

    async fn with_switch_client_state<R>(
        &self,
        client: Option<SwitchManagedClientIdentity>,
        action: impl FnOnce(&mut HandlerState) -> Result<R, RmuxError>,
    ) -> Result<R, RmuxError> {
        let mut state = self.state.lock().await;
        match client {
            None => action(&mut state),
            Some(SwitchManagedClientIdentity::Attach { pid, attach_id }) => {
                let active_attach = self.active_attach.lock().await;
                if active_attach
                    .by_pid
                    .get(&pid)
                    .is_none_or(|active| active.id != attach_id)
                {
                    return Err(attached_client_required("switch-client"));
                }
                let result = action(&mut state);
                drop(active_attach);
                result
            }
            Some(SwitchManagedClientIdentity::Control { pid, control_id }) => {
                let active_control = self.active_control.lock().await;
                if active_control.by_pid.get(&pid).is_none_or(|active| {
                    active.id != control_id || active.closing.load(Ordering::SeqCst)
                }) {
                    return Err(attached_client_required("switch-client"));
                }
                let result = action(&mut state);
                drop(active_control);
                result
            }
        }
    }

    async fn validate_switch_destination(
        &self,
        client: SwitchManagedClientIdentity,
        session_name: &SessionName,
        session_id: SessionId,
        target_selection: Option<&SwitchTargetSelection>,
    ) -> Result<(), RmuxError> {
        self.with_switch_client_state(Some(client), |state| {
            if state
                .sessions
                .session(session_name)
                .is_none_or(|session| session.id() != session_id)
            {
                return Err(session_not_found(session_name));
            }
            if let Some(selection) = target_selection {
                selection.validate_for_session_identity(state, session_name, session_id)?;
            }
            Ok(())
        })
        .await
    }

    pub(super) async fn switch_managed_client_to_session(
        &self,
        requester_pid: u32,
        client: SwitchManagedClientIdentity,
        session_name: rmux_proto::SessionName,
        skip_environment_update: bool,
    ) -> Response {
        let target_session = match self.switch_session_identity(session_name).await {
            Ok(identity) => identity,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        self.switch_managed_client_to_session_identity(
            requester_pid,
            client,
            target_session,
            None,
            skip_environment_update,
        )
        .await
    }

    async fn switch_managed_client_to_session_identity(
        &self,
        requester_pid: u32,
        client: SwitchManagedClientIdentity,
        target_session: SwitchSessionIdentity,
        target_selection: Option<SwitchTargetSelection>,
        skip_environment_update: bool,
    ) -> Response {
        let SwitchSessionIdentity {
            session_name,
            session_id,
        } = target_session;
        if let Err(error) = self
            .validate_switch_destination(
                client,
                &session_name,
                session_id,
                target_selection.as_ref(),
            )
            .await
        {
            return Response::Error(ErrorResponse { error });
        }
        let client_environment = (!skip_environment_update)
            .then(|| client_environment_snapshot(requester_pid))
            .flatten();
        let attached_count = self
            .attached_count_after_switch(&session_name, client.client())
            .await;
        if let Err(error) = self.validate_switch_managed_client_identity(client).await {
            return Response::Error(ErrorResponse { error });
        }

        match client {
            SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                let Some((
                    terminal_context,
                    client_size,
                    client_pixels,
                    render_stream,
                    client_flags,
                )) = self
                    .terminal_context_and_size_for_attached_client_identity(attach_pid, attach_id)
                    .await
                else {
                    return Response::Error(ErrorResponse {
                        error: attached_client_required("switch-client"),
                    });
                };
                match self
                    .commit_attached_session_switch(
                        attach_pid,
                        attach_id,
                        AttachedSwitchCommitRequest {
                            session_name: session_name.clone(),
                            session_id,
                            target_selection,
                            terminal_context,
                            client_geometry: TerminalGeometry {
                                size: client_size,
                                pixels: client_pixels,
                            },
                            client_flags,
                            render_stream,
                            attached_count,
                            client_environment,
                        },
                    )
                    .await
                {
                    Ok(_previous_session_name) => {
                        self.emit_client_session_changed(
                            attach_pid,
                            session_name.clone(),
                            session_id,
                        )
                        .await;
                        Response::SwitchClient(SwitchClientResponse { session_name })
                    }
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            }
            SwitchManagedClientIdentity::Control {
                pid: control_pid,
                control_id,
            } => {
                match self
                    .set_control_session_for_client_identity(
                        control_pid,
                        control_id,
                        session_name.clone(),
                        session_id,
                        target_selection,
                        client_environment.as_ref(),
                    )
                    .await
                {
                    Ok(_previous_session_name) => {
                        self.emit_client_session_changed(
                            control_pid,
                            session_name.clone(),
                            session_id,
                        )
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
        attach_id: u64,
        session_name: &rmux_proto::SessionName,
        key_table: String,
    ) -> Result<(), RmuxError> {
        let key_table_set_at = Instant::now();
        self.set_attached_key_table_for_client_identity(
            attach_pid,
            attach_id,
            Some(key_table.clone()),
            Some(key_table_set_at),
        )
        .await?;
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| active.id == attach_id && &active.session_name == session_name)
        else {
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
        current_session: Option<&SwitchSessionIdentity>,
        flags: TargetFindFlags,
        zoom: bool,
    ) -> Result<SwitchSessionIdentity, RmuxError> {
        self.apply_switch_target_inner(target, current_session, flags, zoom, None, true)
            .await
            .map(|resolved| resolved.session)
    }

    async fn apply_switch_target_for_client_identity(
        &self,
        target: &str,
        current_session: Option<&SwitchSessionIdentity>,
        flags: TargetFindFlags,
        zoom: bool,
        client: SwitchManagedClientIdentity,
    ) -> Result<ResolvedSwitchTarget, RmuxError> {
        self.apply_switch_target_inner(target, current_session, flags, zoom, Some(client), false)
            .await
    }

    async fn apply_switch_target_inner(
        &self,
        target: &str,
        current_session: Option<&SwitchSessionIdentity>,
        flags: TargetFindFlags,
        zoom: bool,
        client: Option<SwitchManagedClientIdentity>,
        apply_selection: bool,
    ) -> Result<ResolvedSwitchTarget, RmuxError> {
        let find_type = switch_client_target_find_type(target);
        self.with_switch_client_state(client, |state| {
            let current_target = match current_session {
                Some(identity) => {
                    if state
                        .sessions
                        .session(&identity.session_name)
                        .is_none_or(|session| session.id() != identity.session_id)
                    {
                        return Err(session_not_found(&identity.session_name));
                    }
                    active_session_target(&state.sessions, &identity.session_name)
                }
                None => None,
            };
            let context = with_visible_pane_bases(
                TargetFindContext::new(current_target),
                &state.sessions,
                &state.options,
            );
            let resolved = state
                .sessions
                .resolve_unresolved_target(
                    &UnresolvedTarget::new(target.to_owned()),
                    find_type,
                    flags,
                    &context,
                )
                .map_err(|error| {
                    if find_type == TargetFindType::Session {
                        normalize_switch_session_lookup_error(target, error)
                    } else {
                        error
                    }
                })?;

            if find_type == TargetFindType::Session && !matches!(resolved, Target::Session(_)) {
                return Err(RmuxError::Server(format!(
                    "resolve-target produced {} where a session target was required",
                    switch_target_response_name(&resolved)
                )));
            }

            let (session_name, selection) = match resolved {
                Target::Session(session_name) => (session_name, None),
                Target::Window(target) => {
                    let window_id = state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .ok_or_else(|| {
                            RmuxError::invalid_target(
                                target.to_string(),
                                "window disappeared while resolving switch target",
                            )
                        })?
                        .id();
                    (
                        target.session_name().clone(),
                        Some(SwitchTargetSelection::Window { target, window_id }),
                    )
                }
                Target::Pane(target) => {
                    let (window_id, pane_id) = state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .and_then(|window| {
                            window
                                .pane(target.pane_index())
                                .map(|pane| (window.id(), pane.id()))
                        })
                        .ok_or_else(|| {
                            RmuxError::invalid_target(
                                target.to_string(),
                                "pane disappeared while resolving switch target",
                            )
                        })?;
                    (
                        target.session_name().clone(),
                        Some(SwitchTargetSelection::Pane {
                            target,
                            window_id,
                            pane_id,
                            zoom,
                        }),
                    )
                }
            };
            if apply_selection {
                if let Some(selection) = selection.as_ref() {
                    selection.apply_to_state(state)?;
                }
            }
            let session_id = state
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?
                .id();
            Ok(ResolvedSwitchTarget {
                session: SwitchSessionIdentity {
                    session_name,
                    session_id,
                },
                selection,
            })
        })
        .await
    }

    async fn adjacent_session_name(
        &self,
        current_session: Option<&SwitchSessionIdentity>,
        forward: bool,
        sort_order: Option<&str>,
    ) -> Result<Option<SwitchSessionIdentity>, RmuxError> {
        let session_names = {
            let state = self.state.lock().await;
            if let Some(identity) = current_session {
                if state
                    .sessions
                    .session(&identity.session_name)
                    .is_none_or(|session| session.id() != identity.session_id)
                {
                    return Err(session_not_found(&identity.session_name));
                }
            }
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
                .map(
                    |(session_name, _, _, _, session_id)| SwitchSessionIdentity {
                        session_name,
                        session_id,
                    },
                )
                .collect::<Vec<_>>()
        };
        if session_names.is_empty() {
            return Err(RmuxError::Server("no sessions".to_owned()));
        }

        let index = current_session
            .and_then(|current| {
                session_names
                    .iter()
                    .position(|candidate| candidate.session_name == current.session_name)
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

    pub(super) async fn switch_session_identity(
        &self,
        session_name: SessionName,
    ) -> Result<SwitchSessionIdentity, RmuxError> {
        let state = self.state.lock().await;
        let session_id = state
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?
            .id();
        Ok(SwitchSessionIdentity {
            session_name,
            session_id,
        })
    }
}

fn normalize_switch_session_lookup_error(target: &str, error: RmuxError) -> RmuxError {
    if matches!(
        &error,
        RmuxError::InvalidTarget { reason, .. } if reason.starts_with("can't find session: ")
    ) {
        let lookup = target.strip_prefix('=').unwrap_or(target);
        if let Ok(session_name) = SessionName::new(lookup.to_owned()) {
            return session_not_found(&session_name);
        }
    }
    error
}

fn switch_target_response_name(target: &Target) -> &'static str {
    match target {
        Target::Session(_) => "session",
        Target::Window(_) => "window",
        Target::Pane(_) => "pane",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client_flags::ClientFlags;
    use crate::control::{ControlModeUpgrade, ControlServerEvent, CONTROL_SERVER_EVENT_CAPACITY};
    use rmux_proto::{
        AttachSessionExtRequest, ControlMode, KillSessionRequest, KillWindowRequest,
        NewSessionRequest, NewWindowRequest, Request, Response, ScopeSelector, SetOptionMode,
        SetOptionRequest, TerminalSize,
    };
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    struct SwitchEnvironmentChild(std::process::Child);

    impl Drop for SwitchEnvironmentChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    const SWITCH_ENVIRONMENT_HELPER: &str = "RMUX_TEST_SWITCH_ENVIRONMENT_HELPER";

    #[test]
    fn switch_environment_probe_helper() {
        if std::env::var_os(SWITCH_ENVIRONMENT_HELPER).is_some() {
            std::thread::sleep(std::time::Duration::from_secs(120));
        }
    }

    async fn spawn_switch_environment_child(display: &str) -> SwitchEnvironmentChild {
        let executable = std::env::current_exe().expect("current test executable");
        let mut command = std::process::Command::new(executable);
        command.args([
            "--exact",
            "handler::client_support::switching::tests::switch_environment_probe_helper",
            "--test-threads=1",
        ]);
        command.env(SWITCH_ENVIRONMENT_HELPER, "1");
        command.env("DISPLAY", display);
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = SwitchEnvironmentChild(
            command
                .spawn()
                .expect("spawn switch environment helper process"),
        );
        let pid = child.0.id();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if rmux_os::process::environment(pid)
                    .as_ref()
                    .and_then(|environment| environment.get("DISPLAY"))
                    .is_some_and(|value| value == display)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("switch environment helper installs its environment");
        child
    }

    async fn create_session(handler: &RequestHandler, name: SessionName) {
        let response = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    }

    async fn create_window_with_name(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_name: &str,
    ) -> u32 {
        let mut state = handler.state.lock().await;
        let session = state
            .sessions
            .session_mut(session_name)
            .expect("session exists");
        let (window_index, _) = session
            .create_window(TerminalSize { cols: 80, rows: 24 })
            .expect("window create succeeds");
        session
            .rename_window(window_index, window_name.to_owned())
            .expect("window rename succeeds");
        window_index
    }

    async fn create_detached_runtime_window(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_name: &str,
        target_window_index: Option<u32>,
    ) -> u32 {
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: Some(window_name.to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        let Response::NewWindow(response) = response else {
            panic!("runtime window creation failed: {response:?}");
        };
        response.target.window_index()
    }

    async fn resize_test_window(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_index: u32,
        size: TerminalSize,
    ) {
        let mut state = handler.state.lock().await;
        state
            .mutate_session_and_resize_window_terminal(session_name, window_index, |session| {
                if session.active_window_index() == window_index {
                    session.resize_active_window_terminal(size);
                    Ok(())
                } else {
                    session.resize_window(window_index, size)
                }
            })
            .expect("test window resize succeeds");
    }

    async fn set_test_window_size_policy(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_index: u32,
        value: &str,
    ) {
        let response = handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(WindowTarget::with_window(
                    session_name.clone(),
                    window_index,
                )),
                option: OptionName::WindowSize,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await;
        assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    }

    async fn set_test_attached_client_size(
        handler: &RequestHandler,
        attach_pid: u32,
        size: TerminalSize,
    ) {
        let mut active_attach = handler.active_attach.lock().await;
        let size_sequence = active_attach.next_size_sequence;
        active_attach.next_size_sequence = size_sequence.saturating_add(1);
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .expect("test attached client exists");
        active.client_size = size;
        active.size_sequence = size_sequence;
        drop(active_attach);
        handler.bump_active_attach_epoch();
    }

    async fn test_pane_terminal_size(
        handler: &RequestHandler,
        session_name: &SessionName,
        window_index: u32,
    ) -> TerminalSize {
        let master = {
            let mut state = handler.state.lock().await;
            state
                .clone_pane_master_if_alive(session_name, window_index, 0)
                .expect("test pane terminal is alive")
        };
        let size = master.size().expect("test pane terminal size is available");
        TerminalSize {
            cols: size.cols,
            rows: size.rows,
        }
    }

    #[tokio::test]
    async fn switch_selection_rejects_reused_pane_index() {
        let handler = RequestHandler::new();
        let beta = session_name("switch-pane-index-identity");
        create_session(&handler, beta.clone()).await;
        let mut state = handler.state.lock().await;
        let session = state.sessions.session_mut(&beta).expect("session exists");
        let pane_index = session.split_pane(0).expect("second pane splits");
        let window = session.window_at(0).expect("window exists");
        let window_id = window.id();
        let captured_pane_id = window.pane(pane_index).expect("pane exists").id();
        let selection = SwitchTargetSelection::Pane {
            target: PaneTarget::with_window(beta.clone(), 0, pane_index),
            window_id,
            pane_id: captured_pane_id,
            zoom: false,
        };

        session
            .kill_pane(pane_index)
            .expect("captured pane is removed");
        let replacement_index = session.split_pane(0).expect("replacement pane splits");
        assert_eq!(replacement_index, pane_index);
        let replacement_pane_id = session
            .window_at(0)
            .and_then(|window| window.pane(replacement_index))
            .expect("replacement pane exists")
            .id();
        assert_ne!(replacement_pane_id, captured_pane_id);

        let error = selection
            .apply_to_session(session)
            .expect_err("stable pane identity must reject index reuse");
        assert!(
            matches!(error, RmuxError::InvalidTarget { .. }),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_resolved_session_name_is_recreated() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-target-identity-alpha");
        let beta = session_name("switch-target-identity-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;

        let attach_pid = 91_341;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        let original_attach_id = handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .expect("attach exists")
            .id;
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_beta = beta.clone();
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_beta.to_string()),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: beta.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_session(&handler, beta.clone()).await;
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(beta.to_string()),
            })
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attach survives");
        assert_eq!(active.id, original_attach_id);
        assert_eq!(active.session_name, alpha);
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_window_index_is_reused_before_commit() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-window-identity-alpha");
        let beta = session_name("switch-window-identity-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let target_index = create_window_with_name(&handler, &beta, "captured").await;
        let target_display = "switch-target-before";
        let client_display = "switch-client-after";
        {
            let mut state = handler.state.lock().await;
            state.environment.set(
                ScopeSelector::Session(beta.clone()),
                "DISPLAY".to_owned(),
                target_display.to_owned(),
            );
        }
        let captured_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(target_index))
            .expect("captured target window exists")
            .id();

        let requester = spawn_switch_environment_child(client_display).await;
        let attach_pid = requester.0.id();
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        let client_size = TerminalSize {
            cols: 111,
            rows: 37,
        };
        handler
            .active_attach
            .lock()
            .await
            .by_pid
            .get_mut(&attach_pid)
            .expect("attach exists")
            .client_size = client_size;
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{target_index}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
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
        });

        pause.reached.notified().await;
        let replacement_index = {
            let mut state = handler.state.lock().await;
            let session = state
                .sessions
                .session_mut(&beta)
                .expect("target session survives");
            session
                .remove_window(target_index)
                .expect("captured window is removed");
            let (replacement_index, _) = session
                .create_window(TerminalSize { cols: 80, rows: 24 })
                .expect("replacement window is created");
            session
                .rename_window(replacement_index, "replacement".to_owned())
                .expect("replacement window is named");
            replacement_index
        };
        assert_eq!(replacement_index, target_index);
        let replacement_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(target_index))
            .expect("replacement window exists")
            .id();
        assert_ne!(replacement_window_id, captured_window_id);
        pause.release.notify_one();

        let response = switch.await.expect("switch task joins");
        assert!(
            matches!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget { .. },
                })
            ),
            "{response:?}"
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        {
            let state = handler.state.lock().await;
            assert_eq!(
                state.environment.session_value(&beta, "DISPLAY"),
                Some(target_display),
                "a stale window target must not update the target environment"
            );
            let alpha_session = state.sessions.session(&alpha).expect("alpha survives");
            assert_eq!(
                alpha_session.window().size(),
                TerminalSize { cols: 80, rows: 24 },
                "the original attached session geometry remains unchanged"
            );
            let beta_session = state.sessions.session(&beta).expect("beta survives");
            assert_eq!(
                beta_session.window().size(),
                TerminalSize { cols: 80, rows: 24 },
                "a stale target must not resize the target session"
            );
            assert_eq!(
                beta_session
                    .window_at(target_index)
                    .expect("replacement window survives")
                    .size(),
                TerminalSize { cols: 80, rows: 24 },
                "the replacement window geometry remains unchanged"
            );
        }
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attach survives");
        assert_eq!(active.session_name, alpha);
        assert_eq!(active.client_size, client_size);
    }

    #[tokio::test]
    async fn switch_control_rejects_reused_window_before_environment_update() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-control-target-alpha");
        let beta = session_name("switch-control-target-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let target_index = create_window_with_name(&handler, &beta, "captured").await;
        let captured_window_id = {
            let mut state = handler.state.lock().await;
            state.environment.set(
                ScopeSelector::Session(beta.clone()),
                "DISPLAY".to_owned(),
                "switch-control-target-before".to_owned(),
            );
            state
                .sessions
                .session(&beta)
                .and_then(|session| session.window_at(target_index))
                .expect("captured control target exists")
                .id()
        };

        let requester = spawn_switch_environment_child("switch-control-client-after").await;
        let control_pid = requester.0.id();
        let (control_tx, mut control_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let control_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                control_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("initial control session is set");
        assert!(matches!(
            control_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                if session_name == &alpha
        ));
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{target_index}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    control_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
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
        });

        pause.reached.notified().await;
        let replacement_window_id = {
            let mut state = handler.state.lock().await;
            let session = state
                .sessions
                .session_mut(&beta)
                .expect("control target session survives");
            session
                .remove_window(target_index)
                .expect("captured control target is removed");
            let (replacement_index, _) = session
                .create_window(TerminalSize { cols: 80, rows: 24 })
                .expect("replacement control target is created");
            assert_eq!(replacement_index, target_index);
            session
                .window_at(replacement_index)
                .expect("replacement control target exists")
                .id()
        };
        assert_ne!(replacement_window_id, captured_window_id);
        pause.release.notify_one();

        let response = switch.await.expect("control switch task joins");
        assert!(
            matches!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget { .. },
                })
            ),
            "{response:?}"
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let state = handler.state.lock().await;
        assert_eq!(
            state.environment.session_value(&beta, "DISPLAY"),
            Some("switch-control-target-before"),
            "a stale control target must not update the target environment"
        );
        drop(state);
        let active_control = handler.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&control_pid)
            .expect("original control client survives");
        assert_eq!(active.id, control_id);
        assert_eq!(active.session_name.as_ref(), Some(&alpha));
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_attach_pid_is_reregistered() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-attach-generation-alpha");
        let beta = session_name("switch-attach-generation-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_window = create_window_with_name(&handler, &beta, "target").await;

        let attach_pid = 91_343;
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        let old_id = handler
            .register_attach(attach_pid, alpha.clone(), old_tx)
            .await;
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{beta_window}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: Some("copy-mode".to_owned()),
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: true,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
        let replacement_id = handler
            .register_attach(attach_pid, alpha.clone(), replacement_tx)
            .await;
        assert_ne!(replacement_id, old_id);
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            })
        );
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        while let Ok(control) = old_rx.try_recv() {
            assert!(!matches!(control, AttachControl::Switch(_)));
        }
        let active_attach = handler.active_attach.lock().await;
        let replacement = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("replacement attach survives");
        assert_eq!(replacement.id, replacement_id);
        assert_eq!(replacement.session_name, alpha);
        assert_eq!(replacement.key_table_name, None);
        assert!(!replacement.flags.contains(ClientFlags::READONLY));
        drop(active_attach);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("beta session survives")
                .active_window_index(),
            0,
            "a failed client identity commit must not change the target session selection"
        );
    }

    #[tokio::test]
    async fn switch_client_commits_window_selection_with_attach_identity() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-window-commit-alpha");
        let beta = session_name("switch-window-commit-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let created_window = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: beta.clone(),
                name: Some("target".to_owned()),
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        let Response::NewWindow(created_window) = created_window else {
            panic!("target window creation failed: {created_window:?}");
        };
        let beta_window = created_window.target.window_index();

        let attach_pid = 91_348;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler.register_attach(attach_pid, alpha, control_tx).await;

        let response = handler
            .handle_switch_client_ext3(
                attach_pid,
                SwitchClientExt3Request {
                    target_client: None,
                    target: Some(format!("{beta}:{beta_window}")),
                    key_table: None,
                    last_session: false,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: true,
                    zoom: false,
                },
            )
            .await;

        assert_eq!(
            response,
            Response::SwitchClient(SwitchClientResponse {
                session_name: beta.clone(),
            })
        );
        assert!(matches!(
            control_rx.try_recv(),
            Ok(AttachControl::Switch(target)) if target.session_name == beta
        ));
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("beta session survives")
                .active_window_index(),
            beta_window
        );
        drop(state);
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .by_pid
                .get(&attach_pid)
                .map(|active| active.session_name.clone()),
            Some(beta)
        );
    }

    #[tokio::test]
    async fn switch_client_resizes_selected_inactive_window_with_its_policy() {
        const OLD_ACTIVE_SIZE: TerminalSize = TerminalSize { cols: 70, rows: 20 };
        const TARGET_INITIAL_SIZE: TerminalSize = TerminalSize { cols: 90, rows: 30 };
        const TARGET_CLIENT_SIZE: TerminalSize = TerminalSize {
            cols: 100,
            rows: 32,
        };
        const SWITCHING_CLIENT_SIZE: TerminalSize = TerminalSize {
            cols: 120,
            rows: 40,
        };

        let handler = RequestHandler::new();
        let alpha = session_name("switch-target-size-alpha");
        let beta = session_name("switch-target-size-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_window =
            create_detached_runtime_window(&handler, &beta, "sized-target", Some(1)).await;
        handler.wait_for_initial_panes_for_test().await;
        resize_test_window(&handler, &beta, 0, OLD_ACTIVE_SIZE).await;
        resize_test_window(&handler, &beta, beta_window, TARGET_INITIAL_SIZE).await;
        set_test_window_size_policy(&handler, &beta, 0, "largest").await;
        set_test_window_size_policy(&handler, &beta, beta_window, "smallest").await;

        let target_client_pid = 91_349;
        let (target_client_tx, _target_client_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(target_client_pid, beta.clone(), target_client_tx)
            .await;
        set_test_attached_client_size(&handler, target_client_pid, TARGET_CLIENT_SIZE).await;

        let switching_client_pid = 91_350;
        let (switching_client_tx, mut switching_client_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(switching_client_pid, alpha, switching_client_tx)
            .await;
        set_test_attached_client_size(&handler, switching_client_pid, SWITCHING_CLIENT_SIZE).await;
        let old_active_pty_before = test_pane_terminal_size(&handler, &beta, 0).await;

        let response = handler
            .handle_switch_client_ext3(
                switching_client_pid,
                SwitchClientExt3Request {
                    target_client: None,
                    target: Some(format!("{beta}:{beta_window}")),
                    key_table: None,
                    last_session: false,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: true,
                    zoom: false,
                },
            )
            .await;

        assert_eq!(
            response,
            Response::SwitchClient(SwitchClientResponse {
                session_name: beta.clone(),
            })
        );
        let switched_target = match switching_client_rx.try_recv() {
            Ok(AttachControl::Switch(target)) => target,
            other => panic!("expected one switch target, got {other:?}"),
        };
        assert_eq!(
            switched_target.active_pane_geometry.cols(),
            TARGET_CLIENT_SIZE.cols,
            "the first switch frame must use the target window's reconciled width"
        );
        {
            let state = handler.state.lock().await;
            let session = state
                .sessions
                .session(&beta)
                .expect("target session survives");
            assert_eq!(session.active_window_index(), beta_window);
            assert_eq!(
                session.window_at(0).expect("old active survives").size(),
                OLD_ACTIVE_SIZE,
                "switching to an inactive target must not resize the old active window"
            );
            assert_eq!(
                session
                    .window_at(beta_window)
                    .expect("selected target survives")
                    .size(),
                TARGET_CLIENT_SIZE,
                "the target's smallest policy must choose the existing target client over the larger incoming client"
            );
            assert_eq!(
                session.terminal_size(),
                TARGET_CLIENT_SIZE,
                "the committed session terminal size must follow the newly active target"
            );
        }
        assert_eq!(
            test_pane_terminal_size(&handler, &beta, 0).await,
            old_active_pty_before,
            "the old active PTY must remain untouched"
        );
        let target_pty_size = test_pane_terminal_size(&handler, &beta, beta_window).await;
        assert_eq!(target_pty_size.cols, TARGET_CLIENT_SIZE.cols);
        assert!(
            target_pty_size.rows == TARGET_CLIENT_SIZE.rows
                || target_pty_size.rows == TARGET_CLIENT_SIZE.rows.saturating_sub(1),
            "the selected target PTY follows its reconciled geometry: {target_pty_size:?}"
        );
    }

    #[tokio::test]
    async fn switch_client_targeted_resize_rejects_window_index_aba_after_size_selection() {
        const OLD_ACTIVE_SIZE: TerminalSize = TerminalSize { cols: 73, rows: 21 };
        const REPLACEMENT_SIZE: TerminalSize = TerminalSize { cols: 66, rows: 18 };
        const SWITCHING_CLIENT_SIZE: TerminalSize = TerminalSize {
            cols: 121,
            rows: 41,
        };

        let handler = RequestHandler::new();
        let alpha = session_name("switch-size-aba-alpha");
        let beta = session_name("switch-size-aba-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let target_index =
            create_detached_runtime_window(&handler, &beta, "captured", Some(1)).await;
        handler.wait_for_initial_panes_for_test().await;
        resize_test_window(&handler, &beta, 0, OLD_ACTIVE_SIZE).await;

        let captured_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(target_index))
            .expect("captured target exists")
            .id();
        let attach_pid = 91_351;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        let attach_id = handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;
        set_test_attached_client_size(&handler, attach_pid, SWITCHING_CLIENT_SIZE).await;
        let pause = handler.install_attached_size_selection_pause();

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{target_index}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    attach_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let killed = handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(beta.clone(), target_index),
                kill_all_others: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillWindow(_)), "{killed:?}");
        let replacement_index =
            create_detached_runtime_window(&handler, &beta, "replacement", Some(target_index))
                .await;
        assert_eq!(replacement_index, target_index);
        handler.wait_for_initial_panes_for_test().await;
        resize_test_window(&handler, &beta, replacement_index, REPLACEMENT_SIZE).await;
        set_test_window_size_policy(&handler, &beta, replacement_index, "manual").await;
        let replacement_window_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .and_then(|session| session.window_at(replacement_index))
            .expect("replacement target exists")
            .id();
        assert_ne!(replacement_window_id, captured_window_id);
        let old_active_pty_before_release = test_pane_terminal_size(&handler, &beta, 0).await;
        let replacement_pty_before_release =
            test_pane_terminal_size(&handler, &beta, replacement_index).await;
        pause.release.notify_one();

        assert!(matches!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: RmuxError::InvalidTarget { .. },
            })
        ));
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        {
            let state = handler.state.lock().await;
            let session = state
                .sessions
                .session(&beta)
                .expect("target session survives");
            assert_eq!(session.active_window_index(), 0);
            assert_eq!(session.window().size(), OLD_ACTIVE_SIZE);
            assert_eq!(
                session
                    .window_at(replacement_index)
                    .expect("replacement survives")
                    .size(),
                REPLACEMENT_SIZE,
                "a replacement at the captured index must not inherit the stale resize"
            );
            assert_eq!(session.terminal_size(), OLD_ACTIVE_SIZE);
        }
        let active_attach = handler.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("original attached client survives");
        assert_eq!(active.id, attach_id);
        assert_eq!(active.session_name, alpha);
        drop(active_attach);
        assert_eq!(
            test_pane_terminal_size(&handler, &beta, 0).await,
            old_active_pty_before_release
        );
        assert_eq!(
            test_pane_terminal_size(&handler, &beta, replacement_index).await,
            replacement_pty_before_release
        );
    }

    #[tokio::test]
    async fn attach_session_fails_closed_when_attach_pid_is_reregistered() {
        let handler = RequestHandler::new();
        let alpha = session_name("attach-generation-alpha");
        let beta = session_name("attach-generation-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;

        let attach_pid = 91_345;
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        let old_id = handler
            .register_attach(attach_pid, alpha.clone(), old_tx)
            .await;
        let pause = install_switch_target_identity_pause(beta.clone());

        let attach_handler = handler.clone();
        let attach_beta = beta.clone();
        let attach = tokio::spawn(async move {
            attach_handler
                .dispatch(
                    attach_pid,
                    Request::AttachSessionExt(AttachSessionExtRequest {
                        target: Some(attach_beta),
                        detach_other_clients: false,
                        kill_other_clients: false,
                        read_only: true,
                        skip_environment_update: true,
                        flags: None,
                    }),
                )
                .await
        });

        pause.reached.notified().await;
        let (replacement_tx, mut replacement_rx) = mpsc::unbounded_channel();
        let replacement_id = handler
            .register_attach(attach_pid, alpha.clone(), replacement_tx)
            .await;
        assert_ne!(replacement_id, old_id);
        pause.release.notify_one();

        assert_eq!(
            attach.await.expect("attach task joins").response,
            Response::Error(ErrorResponse {
                error: attached_client_required("attach-session"),
            })
        );
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        let replacement = active_attach
            .by_pid
            .get(&attach_pid)
            .expect("replacement attach survives");
        assert_eq!(replacement.id, replacement_id);
        assert_eq!(replacement.session_name, alpha);
        assert!(!replacement.flags.contains(ClientFlags::READONLY));
    }

    #[tokio::test]
    async fn switch_client_fails_closed_when_control_pid_is_reregistered() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-control-generation-alpha");
        let beta = session_name("switch-control-generation-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_window = create_window_with_name(&handler, &beta, "target").await;

        let control_pid = 91_344;
        let (old_tx, mut old_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let old_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                old_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("initial control session set succeeds");
        assert!(matches!(
            old_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                if session_name == &alpha
        ));
        let pause = install_switch_target_identity_pause(beta.clone());

        let switch_handler = handler.clone();
        let switch_target = format!("{beta}:{beta_window}");
        let switch = tokio::spawn(async move {
            switch_handler
                .handle_switch_client_ext3(
                    control_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(switch_target),
                        key_table: None,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only: false,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await
        });

        pause.reached.notified().await;
        let (replacement_tx, mut replacement_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let replacement_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                replacement_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        assert_ne!(replacement_id, old_id);
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("replacement control session set succeeds");
        assert!(matches!(
            replacement_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                if session_name == &alpha
        ));
        pause.release.notify_one();

        assert_eq!(
            switch.await.expect("switch task joins"),
            Response::Error(ErrorResponse {
                error: attached_client_required("switch-client"),
            })
        );
        assert!(matches!(
            replacement_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(matches!(old_rx.try_recv(), Ok(ControlServerEvent::Exit(_))));
        let active_control = handler.active_control.lock().await;
        let replacement = active_control
            .by_pid
            .get(&control_pid)
            .expect("replacement control survives");
        assert_eq!(replacement.id, replacement_id);
        assert_eq!(replacement.session_name.as_ref(), Some(&alpha));
        assert_eq!(replacement.last_session, None);
        drop(active_control);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&beta)
                .expect("beta session survives")
                .active_window_index(),
            0,
            "a failed control identity commit must not change the target session selection"
        );
    }

    #[tokio::test]
    async fn switch_control_rejects_attach_only_flags_before_switching_session() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-control-flags-alpha");
        let beta = session_name("switch-control-flags-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;

        let control_pid = 91_345;
        let (event_tx, mut event_rx) = mpsc::channel(CONTROL_SERVER_EVENT_CAPACITY);
        let control_id = handler
            .register_control_with_closing(
                control_pid,
                ControlModeUpgrade {
                    mode: ControlMode::Plain,
                    terminal_context: crate::outer_terminal::OuterTerminalContext::default(),
                    initial_command_count: 0,
                },
                event_tx,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        handler
            .set_control_session(control_pid, Some(alpha.clone()))
            .await
            .expect("initial control session set succeeds");
        assert!(matches!(
            event_rx.try_recv(),
            Ok(ControlServerEvent::SessionChanged(Some(ref session_name)))
                if session_name == &alpha
        ));

        for (key_table, toggle_read_only, expected_message) in [
            (
                Some("copy-mode".to_owned()),
                false,
                "switch-client -T is not available for control clients",
            ),
            (
                None,
                true,
                "switch-client -r is not available for control clients",
            ),
        ] {
            let response = handler
                .handle_switch_client_ext3(
                    control_pid,
                    SwitchClientExt3Request {
                        target_client: None,
                        target: Some(beta.to_string()),
                        key_table,
                        last_session: false,
                        next_session: false,
                        previous_session: false,
                        toggle_read_only,
                        sort_order: None,
                        skip_environment_update: true,
                        zoom: false,
                    },
                )
                .await;
            assert_eq!(
                response,
                Response::Error(ErrorResponse {
                    error: RmuxError::Server(expected_message.to_owned()),
                })
            );
            assert!(matches!(
                event_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ));
            let active_control = handler.active_control.lock().await;
            let active = active_control
                .by_pid
                .get(&control_pid)
                .expect("control remains registered");
            assert_eq!(active.id, control_id);
            assert_eq!(active.session_name.as_ref(), Some(&alpha));
        }
    }

    #[tokio::test]
    async fn switch_client_last_session_keeps_the_captured_session_identity() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-last-identity-alpha");
        let beta = session_name("switch-last-identity-beta");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, beta.clone()).await;
        let beta_id = handler
            .state
            .lock()
            .await
            .sessions
            .session(&beta)
            .expect("beta exists")
            .id();

        let attach_pid = 91_342;
        let (control_tx, mut control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, alpha.clone(), control_tx)
            .await;

        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: beta.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)), "{killed:?}");
        create_session(&handler, beta.clone()).await;
        {
            let mut active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("attach exists");
            active.last_session = Some(beta.clone());
            active.last_session_id = Some(beta_id);
        }

        let response = handler
            .handle_switch_client_ext3(
                attach_pid,
                SwitchClientExt3Request {
                    target_client: None,
                    target: None,
                    key_table: None,
                    last_session: true,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: true,
                    zoom: false,
                },
            )
            .await;

        assert_eq!(
            response,
            Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(beta.to_string()),
            })
        );
        assert!(matches!(
            control_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let active_attach = handler.active_attach.lock().await;
        assert_eq!(
            active_attach
                .by_pid
                .get(&attach_pid)
                .map(|active| active.session_name.clone()),
            Some(alpha)
        );
    }

    #[tokio::test]
    async fn apply_switch_target_resolves_window_id_target() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-window-id-alpha");
        create_session(&handler, alpha.clone()).await;
        let response = handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                environment: None,
                command: None,
                start_directory: None,
                target_window_index: None,
                insert_at_target: false,
                process_command: None,
            })))
            .await;
        assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
        let window_id = {
            let state = handler.state.lock().await;
            state
                .sessions
                .session(&alpha)
                .and_then(|session| session.window_at(1))
                .map(|window| window.id().to_string())
                .expect("second window id exists")
        };
        let current = handler
            .switch_session_identity(alpha.clone())
            .await
            .expect("current session identity exists");

        let switched = handler
            .apply_switch_target(&window_id, Some(&current), TargetFindFlags::NONE, false)
            .await
            .expect("window id target resolves");
        assert_eq!(switched.session_name, alpha);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("alpha session exists")
                .active_window_index(),
            1
        );
    }

    #[tokio::test]
    async fn apply_switch_target_resolves_bare_session_name_before_window_name() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-bare-window-alpha");
        let editor = session_name("editor");
        create_session(&handler, alpha.clone()).await;
        create_session(&handler, editor.clone()).await;
        let editor_window = create_window_with_name(&handler, &alpha, "editor").await;
        let current = handler
            .switch_session_identity(alpha.clone())
            .await
            .expect("current session identity exists");

        let switched = handler
            .apply_switch_target("editor", Some(&current), TargetFindFlags::NONE, false)
            .await
            .expect("bare session name resolves before colliding window name");

        assert_eq!(switched.session_name, editor);
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("alpha session exists")
                .active_window_index(),
            0,
            "colliding alpha window {editor_window} must not be selected"
        );
    }

    #[tokio::test]
    async fn apply_switch_target_rejects_bare_numeric_window_without_session_match() {
        let handler = RequestHandler::new();
        let alpha = session_name("switch-bare-numeric-alpha");
        create_session(&handler, alpha.clone()).await;
        create_window_with_name(&handler, &alpha, "one").await;
        create_window_with_name(&handler, &alpha, "two").await;
        let current = handler
            .switch_session_identity(alpha.clone())
            .await
            .expect("current session identity exists");

        let error = handler
            .apply_switch_target("2", Some(&current), TargetFindFlags::NONE, false)
            .await
            .expect_err("bare numeric window index is not a session target");

        assert!(matches!(error, RmuxError::SessionNotFound(session) if session == "2"));
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&alpha)
                .expect("alpha session exists")
                .active_window_index(),
            0
        );
    }

    #[tokio::test]
    async fn switch_client_dot_target_keeps_current_attached_session() {
        let handler = RequestHandler::new();
        let requester_pid = std::process::id();
        let work = session_name("switch-dot-work");
        let idle = session_name("switch-dot-idle");
        create_session(&handler, work.clone()).await;
        create_session(&handler, idle).await;
        let (control_tx, _control_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(requester_pid, work.clone(), control_tx)
            .await;

        let response = handler
            .handle(Request::SwitchClientExt3(Box::new(
                SwitchClientExt3Request {
                    target_client: None,
                    target: Some(".".to_owned()),
                    key_table: None,
                    last_session: false,
                    next_session: false,
                    previous_session: false,
                    toggle_read_only: false,
                    sort_order: None,
                    skip_environment_update: false,
                    zoom: false,
                },
            )))
            .await;

        assert_eq!(
            response,
            Response::SwitchClient(SwitchClientResponse {
                session_name: work.clone()
            })
        );
        assert_eq!(
            handler
                .attached_session_name_for_command(requester_pid, "switch-client")
                .await
                .expect("attached client remains registered"),
            work
        );
    }
}
