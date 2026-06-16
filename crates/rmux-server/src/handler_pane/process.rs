use rmux_core::LifecycleEvent;
use rmux_proto::{ErrorResponse, HookName, Response, ScopeSelector, Target};

use super::super::{
    client_environment_snapshot, client_spawn_environment, prepare_lifecycle_event,
    scripting_support::{format_context_for_target, render_start_directory_template},
    RequestHandler,
};
use crate::format_runtime::render_runtime_template;
use crate::hook_runtime::PendingInlineHookFormat;

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
        let session_name = request.target.session_name().clone();
        let target = request.target.clone();
        let socket_path = self.socket_path();
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        let attached_count = self.attached_count(&session_name).await;
        let response = {
            let mut state = self.state.lock().await;
            request.start_directory = match render_start_directory_template(
                &state,
                &Target::Pane(target),
                attached_count,
                request.start_directory,
            ) {
                Ok(start_directory) => start_directory,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            match state.respawn_pane(
                request,
                &socket_path,
                spawn_environment.as_ref(),
                Some(self.pane_alert_callback()),
                Some(self.pane_exit_callback()),
                |state, replaced| {
                    let queued = prepare_lifecycle_event(
                        state,
                        &LifecycleEvent::PaneExited {
                            target: replaced.target.clone(),
                            pane_id: Some(replaced.pane_id),
                            window_id: Some(replaced.window_id),
                            window_name: Some(replaced.window_name.clone()),
                        },
                    );
                    self.emit_prepared(queued);
                },
            ) {
                Ok(response) => Response::RespawnPane(response),
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::RespawnPane(_)) {
            self.refresh_attached_session(&session_name).await;
        }

        response
    }
}
