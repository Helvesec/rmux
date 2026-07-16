use rmux_proto::{ErrorResponse, HookName, Response, ScopeSelector, Target};

use super::super::{
    client_environment_snapshot, client_spawn_environment,
    scripting_support::{format_context_for_target, render_start_directory_template},
    RequestHandler,
};
#[cfg(windows)]
use super::format_references_pane_pid;
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::PendingInlineHookFormat;
use crate::pane_terminal_lookup::pane_id_for_target;

impl RequestHandler {
    pub(in crate::handler) async fn handle_pipe_pane(
        &self,
        _requester_pid: u32,
        request: rmux_proto::PipePaneRequest,
    ) -> Response {
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let attached_count = self.attached_count(&session_name).await;
        let write_to_pipe = if !request.stdin && !request.stdout {
            true
        } else {
            request.stdout
        };
        let response = {
            let mut state = self.state.lock().await;
            let command = match request.command.as_deref() {
                Some(command) => {
                    let runtime = match format_context_for_target(
                        &state,
                        &Target::Pane(target.clone()),
                        attached_count,
                    ) {
                        Ok(runtime) => runtime,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    };
                    Some(render_runtime_template(command, &runtime, true))
                }
                None => None,
            };

            match state.pipe_pane(
                target.clone(),
                command,
                request.stdin,
                write_to_pipe,
                request.once,
            ) {
                Ok(response) => Response::PipePane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::PipePane(_)) {
            self.queue_inline_hook(
                HookName::AfterPipePane,
                ScopeSelector::Pane(target.clone()),
                Some(Target::Pane(target)),
                PendingInlineHookFormat::AfterCommand,
            );
        }

        response
    }

    pub(in crate::handler) async fn handle_respawn_pane(
        &self,
        requester_pid: u32,
        mut request: rmux_proto::RespawnPaneRequest,
    ) -> Response {
        #[cfg(windows)]
        if request.start_directory.as_ref().is_some_and(|path| {
            format_references_pane_pid(Some(path.as_os_str().to_string_lossy().as_ref()))
        }) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let socket_path = self.socket_path();
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let attached_count = self.attached_count(&session_name).await;
        let (response, respawned_pane_id) = {
            let mut state = self.state.lock().await;
            let target_window = rmux_proto::WindowTarget::with_window(
                request.target.session_name().clone(),
                request.target.window_index(),
            );
            if let Err(error) =
                super::super::require_expected_window_identity(&state, &target_window)
            {
                return Response::Error(ErrorResponse { error });
            }
            request.start_directory = match render_start_directory_template(
                &state,
                &Target::Pane(target),
                attached_count,
                request.start_directory,
            ) {
                Ok(start_directory) => start_directory,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let pane_id = match pane_id_for_target(
                &state.sessions,
                request.target.session_name(),
                request.target.window_index(),
                request.target.pane_index(),
            ) {
                Ok(pane_id) => pane_id,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            match state.respawn_pane(
                request,
                &socket_path,
                spawn_environment.as_ref(),
                Some(self.pane_alert_callback()),
                Some(self.pane_exit_callback()),
                |_, _| {},
            ) {
                Ok(response) => {
                    self.record_pane_respawn_boundary(pane_id);
                    (Response::RespawnPane(response), Some(pane_id))
                }
                Err(error) => (Response::Error(ErrorResponse { error }), None),
            }
        };

        if respawned_pane_id.is_some() {
            self.refresh_attached_session(&session_name).await;
        }

        response
    }
}
