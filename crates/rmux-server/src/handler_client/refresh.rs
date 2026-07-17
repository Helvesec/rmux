use rmux_core::LifecycleEvent;
use rmux_proto::request::RefreshClientRequest;
use rmux_proto::{
    ErrorResponse, RefreshClientResponse, Response, RmuxError, SessionId, TerminalSize,
    WindowTarget,
};

use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;

use super::super::{
    client_runtime_support::clipboard_query_sequence,
    control_support::{ControlClientIdentity, ManagedClient},
    RequestHandler,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_refresh_client(
        &self,
        requester_pid: u32,
        request: RefreshClientRequest,
    ) -> Response {
        if let Err(error) = validate_refresh_supported_request(&request) {
            return Response::Error(ErrorResponse { error });
        }

        let client = match self
            .resolve_target_managed_client(
                requester_pid,
                request.target_client.as_deref(),
                "refresh-client",
            )
            .await
        {
            Ok(client) => client,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        match client {
            ManagedClient::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                self.handle_refresh_attached_client(attach_pid, attach_id, request)
                    .await
            }
            ManagedClient::Control(identity) => {
                self.handle_refresh_control_client(identity, request).await
            }
        }
    }

    async fn handle_refresh_attached_client(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        request: RefreshClientRequest,
    ) -> Response {
        let mut needs_full_refresh = !request.status_only;
        let clipboard_query = request.clipboard_query;
        let (session_name, session_id, size_eligibility_changed) = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
                active.id == expected_attach_id
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            }) else {
                return Response::Error(ErrorResponse {
                    error: attached_client_required("refresh-client"),
                });
            };

            let ignored_size_before = active.flags.contains(super::ClientFlags::IGNORESIZE);
            let raw_flag = request.flags.as_deref().or(request.flags_alias.as_deref());
            if let Some(raw) = raw_flag {
                let mut merged_flags = active.flags;
                for token in raw.split(',').filter(|t| !t.is_empty()) {
                    if let Err(error) = merged_flags.apply_named(token) {
                        return Response::Error(ErrorResponse { error });
                    }
                }
                if !active.can_write {
                    merged_flags = merged_flags.with_read_only();
                }
                active.flags = merged_flags;
            }
            let size_eligibility_changed =
                ignored_size_before != active.flags.contains(super::ClientFlags::IGNORESIZE);
            if size_eligibility_changed {
                self.bump_active_attach_epoch();
            }

            (
                active.session_name.clone(),
                active.session_id,
                size_eligibility_changed,
            )
        };

        if size_eligibility_changed {
            if let Err(error) = self
                .reconcile_refresh_size_eligibility_transition(
                    attach_pid,
                    expected_attach_id,
                    session_id,
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        if request.status_only {
            if let Err(error) = self
                .refresh_attached_client_status_for_identity(
                    attach_pid,
                    expected_attach_id,
                    &session_name,
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
            needs_full_refresh = false;
        }
        if clipboard_query {
            if let Err(error) = self
                .send_attach_control_for_client_identity(
                    attach_pid,
                    expected_attach_id,
                    AttachControl::Write(clipboard_query_sequence()),
                    "refresh-client",
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }
        if needs_full_refresh {
            if let Err(error) = self
                .refresh_attached_client_for_identity(
                    attach_pid,
                    expected_attach_id,
                    &session_name,
                    "refresh-client",
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        Response::RefreshClient(RefreshClientResponse {
            target_client: attach_pid.to_string(),
        })
    }

    async fn reconcile_refresh_size_eligibility_transition(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        expected_session_id: SessionId,
    ) -> Result<(), RmuxError> {
        let identity_is_current = {
            let active_attach = self.active_attach.lock().await;
            active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                active.id == expected_attach_id
                    && active.session_id == expected_session_id
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        };
        if !identity_is_current {
            return Err(attached_client_required("refresh-client"));
        }
        self.reconcile_attached_session_identity_size_and_emit(expected_session_id)
            .await
    }

    async fn handle_refresh_control_client(
        &self,
        identity: ControlClientIdentity,
        request: RefreshClientRequest,
    ) -> Response {
        let control_pid = identity.requester_pid();
        if request.has_attach_only_effects() {
            return Response::Error(ErrorResponse {
                error: attached_client_required("refresh-client"),
            });
        }

        let control_size = match request.control_size.as_deref() {
            Some(value) => match parse_control_size(value) {
                Some(size) => Some(size),
                None => {
                    return Response::Error(ErrorResponse {
                        error: RmuxError::Server(format!("invalid refresh-client size '{value}'")),
                    });
                }
            },
            None => None,
        };

        let (session_name, session_id) = {
            let active_control = self.active_control.lock().await;
            let Some(active) = active_control.by_pid.get(&control_pid).filter(|active| {
                active.id == identity.control_id()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            }) else {
                return Response::Error(ErrorResponse {
                    error: attached_client_required("refresh-client"),
                });
            };
            (active.session_name.clone(), active.session_id)
        };

        if let (Some(session_name), Some(session_id), Some(size)) =
            (session_name.as_ref(), session_id, control_size)
        {
            #[cfg(windows)]
            self.wait_for_windows_deferred_all_pane_pids().await;
            let target = {
                let mut state = self.state.lock().await;
                let active_control = self.active_control.lock().await;
                let exact_client_still_attached = active_control
                    .by_pid
                    .get(&control_pid)
                    .is_some_and(|active| {
                        active.id == identity.control_id()
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                            && active.session_name.as_ref() == Some(session_name)
                            && active.session_id == Some(session_id)
                    });
                if !exact_client_still_attached {
                    return Response::Error(ErrorResponse {
                        error: attached_client_required("refresh-client"),
                    });
                }
                match state.mutate_session_and_resize_active_window_terminal(
                    session_name,
                    |session| {
                        session.touch_attached();
                        session.resize_active_window_terminal(size);
                        Ok(WindowTarget::with_window(
                            session_name.clone(),
                            session.active_window_index(),
                        ))
                    },
                ) {
                    Ok(target) => target,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                }
            };
            self.emit(LifecycleEvent::WindowLayoutChanged { target })
                .await;
        } else if control_size.is_none() {
            if let Err(error) = self.refresh_control_client_for_identity(identity).await {
                return Response::Error(ErrorResponse { error });
            }
        }

        Response::RefreshClient(RefreshClientResponse {
            target_client: control_pid.to_string(),
        })
    }
}

trait RefreshClientControlScope {
    fn has_attach_only_effects(&self) -> bool;
}

impl RefreshClientControlScope for RefreshClientRequest {
    fn has_attach_only_effects(&self) -> bool {
        self.status_only
            || self.clipboard_query
            || self.flags.is_some()
            || self.flags_alias.is_some()
    }
}

fn validate_refresh_supported_request(request: &RefreshClientRequest) -> Result<(), RmuxError> {
    let mut unsupported = Vec::new();
    if request.clear_pan {
        unsupported.push("-c");
    }
    if request.pan_down {
        unsupported.push("-D");
    }
    if request.pan_left {
        unsupported.push("-L");
    }
    if request.pan_right {
        unsupported.push("-R");
    }
    if request.pan_up {
        unsupported.push("-U");
    }
    if request.adjustment.is_some() {
        unsupported.push("adjustment");
    }
    if !request.subscriptions.is_empty() {
        unsupported.push("-A");
    }
    if !request.subscriptions_format.is_empty() {
        unsupported.push("-B");
    }
    if request.colour_report.is_some() {
        unsupported.push("-r");
    }
    if unsupported.is_empty() {
        return Ok(());
    }
    Err(RmuxError::Server(format!(
        "refresh-client {} is not supported",
        unsupported.join("/")
    )))
}

fn parse_control_size(value: &str) -> Option<TerminalSize> {
    let (cols, rows) = value.split_once('x')?;
    let cols = cols.parse::<u16>().ok()?;
    let rows = rows.parse::<u16>().ok()?;
    (cols > 0 && rows > 0).then_some(TerminalSize { cols, rows })
}
