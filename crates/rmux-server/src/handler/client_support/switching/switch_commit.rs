use super::*;
use crate::control::ControlFlagChange;
use crate::handler::control_support::ControlClientIdentity;

impl RequestHandler {
    pub(in crate::handler::client_support) async fn switch_managed_client_to_session_with_control_flags(
        &self,
        _requester_pid: u32,
        client: SwitchManagedClientIdentity,
        target_session: SwitchSessionIdentity,
        target_selection: Option<SwitchTargetSelection>,
        skip_environment_update: bool,
        control_flag_change: ControlFlagChange,
    ) -> Response {
        record_switch_client_target_identity(client);
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
        // Destination validation checked this resolved client's generation above.
        // An external switch requester must not supply the target's environment.
        let client_environment = (!skip_environment_update)
            .then(|| match client {
                SwitchManagedClientIdentity::Attach { pid, .. }
                | SwitchManagedClientIdentity::Control { pid, .. } => {
                    client_environment_snapshot(pid)
                }
            })
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
                            expected_current_session_id: None,
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
                    Ok(outcome) => {
                        record_switch_client_committed_target(
                            client,
                            outcome.committed_target.clone(),
                        );
                        self.finish_attached_session_switch(
                            outcome,
                            session_name.clone(),
                            session_id,
                        )
                        .await;
                        Response::SwitchClient(SwitchClientResponse { session_name })
                    }
                    Err(failure) => {
                        let error = self.finish_attached_switch_failure(failure).await;
                        Response::Error(ErrorResponse { error })
                    }
                }
            }
            SwitchManagedClientIdentity::Control {
                pid: control_pid,
                control_id,
            } => {
                // The commit publishes `client-session-changed` itself, before
                // the geometry reconciles it triggers, because tmux 3.7b
                // reports the client move before the `%layout-change` lines it
                // causes.
                match self
                    .switch_control_session_for_client_identity(
                        ControlClientIdentity::new(control_pid, control_id),
                        session_name.clone(),
                        session_id,
                        target_selection,
                        client_environment.as_ref(),
                        control_flag_change,
                    )
                    .await
                {
                    Ok(_previous_session_name) => {
                        Response::SwitchClient(SwitchClientResponse { session_name })
                    }
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            }
        }
    }
}
