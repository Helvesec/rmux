use std::collections::HashSet;

use rmux_core::{command_parser::ControlClientSize, LifecycleEvent};

mod control_size;
use rmux_proto::request::RefreshClientRequest;
use rmux_proto::{
    ErrorResponse, RefreshClientResponse, Response, RmuxError, SessionId, SessionName, WindowTarget,
};

use crate::control::{ControlPaneOutputAction, ControlPaneOutputActionKind};
use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;
use crate::pane_terminals::HandlerState;

use super::super::{
    client_runtime_support::clipboard_query_sequence,
    control_support::{ControlClientIdentity, ManagedClient},
    QueuedLifecycleEvent, RequestHandler,
};

impl RequestHandler {
    pub(in crate::handler) async fn handle_refresh_client(
        &self,
        requester_pid: u32,
        request: RefreshClientRequest,
    ) -> Response {
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

        match super::panning::pan_action(&request) {
            Ok(Some(action)) => return self.pan_client(client, action).await,
            Err(error) => return Response::Error(ErrorResponse { error }),
            Ok(None) => {}
        }
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
        if !request.subscriptions_format.is_empty() {
            return Response::Error(ErrorResponse {
                error: attached_client_required("refresh-client"),
            });
        }
        if !request.subscriptions.is_empty() {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("refresh-client -A is not supported".to_owned()),
            });
        }
        if request
            .control_size
            .as_deref()
            .is_some_and(|size| size.starts_with('@'))
        {
            return Response::Error(ErrorResponse {
                error: attached_client_required("refresh-client"),
            });
        }
        let mut needs_full_refresh = !request.status_only;
        let clipboard_query = request.clipboard_query;
        let (session_name, session_id, size_eligibility_changed) = {
            let state = self.state.lock().await;
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
            if !clipboard_query {
                if let Some(report) = request
                    .colour_report
                    .as_deref()
                    .and_then(crate::control_colours::ColourReport::parse)
                {
                    state.apply_control_colour_report(report);
                }
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

        // Explicit refresh invalidates this registration's status jobs even
        // before their TTL expires. Automatic redraws keep using the cache.
        self.state.lock().await.status_jobs().refresh_owner(
            &crate::status_jobs::StatusJobOwner::Status(Some((false, expected_attach_id))),
        );

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

        if let Err(error) = validate_control_client_flags(&request) {
            return Response::Error(ErrorResponse { error });
        }
        let control_size = match request
            .control_size
            .as_deref()
            .map(ControlClientSize::parse)
            .transpose()
        {
            Ok(size) => size,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        let flags_changed = request.flags.is_some() || request.flags_alias.is_some();
        if flags_changed {
            if let Err(error) = self
                .apply_control_size_and_flags(
                    identity,
                    control_size,
                    [request.flags_alias.as_deref(), request.flags.as_deref()]
                        .into_iter()
                        .flatten(),
                )
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        if let Some(report) = request
            .colour_report
            .as_deref()
            .and_then(crate::control_colours::ColourReport::parse)
        {
            if let Err(error) = self
                .apply_control_colour_for_identity(identity, report)
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }

        let actions = self
            .parse_control_pane_output_actions(&request.subscriptions)
            .await;
        if let Err(error) = self
            .apply_control_pane_output_actions_for_identity(identity, actions)
            .await
        {
            return Response::Error(ErrorResponse { error });
        }

        if !flags_changed {
            if let Some(size) = control_size {
                if let Err(error) = self.apply_control_size(identity, size).await {
                    return Response::Error(ErrorResponse { error });
                }
            }
        }
        if flags_changed || control_size.is_none() {
            if let Err(error) = self.refresh_control_client_for_identity(identity).await {
                return Response::Error(ErrorResponse { error });
            }
        }
        if let Err(error) = self
            .apply_control_format_subscriptions_for_identity(identity, request.subscriptions_format)
            .await
        {
            return Response::Error(ErrorResponse { error });
        }

        Response::RefreshClient(RefreshClientResponse {
            target_client: control_pid.to_string(),
        })
    }

    async fn parse_control_pane_output_actions(
        &self,
        values: &[String],
    ) -> Vec<ControlPaneOutputAction> {
        let state = self.state.lock().await;
        values
            .iter()
            .filter_map(|value| {
                let selector = value.strip_prefix('%')?;
                let (pane_id, action) = selector.split_once(':')?;
                let pane_id = pane_id.parse::<u32>().ok()?;
                state
                    .has_pane_output(pane_id)
                    .then_some(ControlPaneOutputAction::new(
                        pane_id,
                        match action {
                            "on" => ControlPaneOutputActionKind::On,
                            "off" => ControlPaneOutputActionKind::Off,
                            "pause" => ControlPaneOutputActionKind::Pause,
                            "continue" => ControlPaneOutputActionKind::Continue,
                            _ => return None,
                        },
                    ))
            })
            .collect()
    }
}

fn prepare_control_size_layout_echo_events(
    state: &mut HandlerState,
    session_name: &SessionName,
    session_id: SessionId,
) -> Vec<QueuedLifecycleEvent> {
    let Some(session) = state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == session_id)
    else {
        let applied_resizes = state.take_applied_window_resizes();
        let mut prepared = Vec::with_capacity(applied_resizes.len().saturating_mul(2));
        for resize in applied_resizes {
            let (target, layout_change_prepared, _) = resize.into_parts();
            prepare_applied_window_resize_pair(
                state,
                &target,
                layout_change_prepared,
                &mut prepared,
            );
        }
        return prepared;
    };
    let mut echoed_ids = HashSet::new();
    let layout_targets = session
        .windows()
        .filter(|(_, window)| echoed_ids.insert(window.id()))
        .map(|(index, _)| index)
        .map(|window_index| WindowTarget::with_window(session_name.clone(), window_index))
        .collect::<Vec<_>>();
    let echo_by_id = layout_targets
        .iter()
        .filter_map(|target| {
            session
                .window_at(target.window_index())
                .map(|window| (window.id(), target.clone()))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let applied_resizes = state
        .take_applied_window_resizes()
        .into_iter()
        .map(|resize| {
            let echo = resize
                .window_id()
                .and_then(|id| echo_by_id.get(&id))
                .cloned();
            let (target, prepared, _) = resize.into_parts();
            (echo.unwrap_or(target), prepared)
        })
        .collect::<Vec<_>>();
    let layout_target_set = layout_targets.iter().cloned().collect::<HashSet<_>>();
    let applied_resize_set = applied_resizes
        .iter()
        .map(|(target, _)| target.clone())
        .collect::<HashSet<_>>();
    let insert_at = applied_resizes
        .iter()
        .position(|(target, _)| layout_target_set.contains(target))
        .unwrap_or(applied_resizes.len());
    let mut prepared = Vec::with_capacity(
        layout_targets
            .len()
            .saturating_add(applied_resizes.len().saturating_mul(2)),
    );

    for (index, (target, layout_change_prepared)) in applied_resizes.iter().enumerate() {
        if index == insert_at {
            prepare_control_size_layout_batch(
                state,
                &layout_targets,
                &applied_resize_set,
                &mut prepared,
            );
        }
        if !layout_target_set.contains(target) {
            prepare_applied_window_resize_pair(
                state,
                target,
                *layout_change_prepared,
                &mut prepared,
            );
        }
    }
    if insert_at == applied_resizes.len() {
        prepare_control_size_layout_batch(
            state,
            &layout_targets,
            &applied_resize_set,
            &mut prepared,
        );
    }
    prepared
}

fn prepare_control_size_layout_batch(
    state: &mut HandlerState,
    layout_targets: &[WindowTarget],
    applied_resizes: &HashSet<WindowTarget>,
    prepared: &mut Vec<QueuedLifecycleEvent>,
) {
    for target in layout_targets {
        prepared.push(super::super::prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowLayoutChanged {
                target: target.clone(),
            },
        ));
        if applied_resizes.contains(target) {
            prepared.push(super::super::prepare_lifecycle_event(
                state,
                &LifecycleEvent::WindowResized {
                    target: target.clone(),
                },
            ));
        }
    }
}

fn prepare_applied_window_resize_pair(
    state: &mut HandlerState,
    target: &WindowTarget,
    layout_change_prepared: bool,
    prepared: &mut Vec<QueuedLifecycleEvent>,
) {
    if !layout_change_prepared {
        prepared.push(super::super::prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowLayoutChanged {
                target: target.clone(),
            },
        ));
    }
    prepared.push(super::super::prepare_lifecycle_event(
        state,
        &LifecycleEvent::WindowResized {
            target: target.clone(),
        },
    ));
}

trait RefreshClientControlScope {
    fn has_attach_only_effects(&self) -> bool;
}

impl RefreshClientControlScope for RefreshClientRequest {
    fn has_attach_only_effects(&self) -> bool {
        self.status_only || self.clipboard_query
    }
}

mod colours;

// Preserve the control flags implemented since the original ignore-size patch.
// Validate both spellings before either can mutate the registered client.
fn validate_control_client_flags(request: &RefreshClientRequest) -> Result<(), RmuxError> {
    for raw in [request.flags_alias.as_deref(), request.flags.as_deref()]
        .into_iter()
        .flatten()
    {
        for token in raw.split(',').filter(|token| !token.is_empty()) {
            let token = token.strip_prefix('!').unwrap_or(token);
            match token {
                "ignore-size" | "ignoresize" | "no-output" | "wait-exit" | "pause-after"
                | "read-only" | "readonly" => {}
                token
                    if token
                        .strip_prefix("pause-after=")
                        .and_then(|value| value.parse::<u32>().ok())
                        .is_some() => {}
                _ => {
                    return Err(RmuxError::Server(
                        "refresh-client client flags are not available for control clients"
                            .to_owned(),
                    ));
                }
            }
        }
    }
    Ok(())
}
