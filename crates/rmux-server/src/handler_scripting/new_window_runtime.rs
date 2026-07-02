use rmux_core::{LifecycleEvent, SessionStore};
use rmux_proto::request::Request;
use rmux_proto::{
    ErrorResponse, HookName, NewWindowRequest, PaneTarget, Response, RmuxError, ScopeSelector,
    Target,
};

use super::format_context_for_target_with_server_values;
use super::queue::{queue_action_from_response, QueueCommandAction, QueueExecutionContext};
use super::queue_parse::ParsedNewWindowCommand;
use super::render_start_directory_template;
use super::targets::NewWindowTargetIndex;
use crate::format_runtime::render_runtime_template;
use crate::handler::{client_environment_snapshot, client_spawn_environment, RequestHandler};
use crate::hook_runtime::{capture_inline_hooks, PendingInlineHookFormat};
use crate::pane_terminals::{NewWindowOptions, WindowSpawnOptions};

impl RequestHandler {
    pub(super) async fn execute_queued_new_window(
        &self,
        requester_pid: u32,
        command: ParsedNewWindowCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let ParsedNewWindowCommand {
            target,
            target_window_index,
            insert_at_target,
            name,
            detached,
            start_directory,
            environment,
            command,
        } = command;

        let target_window_index = {
            let state = self.state.lock().await;
            resolve_queued_new_window_target_index(&state.sessions, &target, target_window_index)?
        };
        let can_write = self.requester_can_write(requester_pid).await;
        let request_for_hooks = crate::server_access::apply_access_policy(
            Request::NewWindow(Box::new(NewWindowRequest {
                target: target.clone(),
                name: name.clone(),
                detached,
                environment: environment.clone(),
                command: command.clone(),
                process_command: None,
                start_directory: start_directory.clone(),
                target_window_index,
                insert_at_target,
            })),
            can_write,
        )?;

        let socket_path = self.socket_path();
        let attached_count = self.attached_count(&target).await;
        let rendered_command = self
            .render_queued_new_window_command(
                command.as_deref(),
                &target,
                context,
                attached_count,
                &socket_path,
            )
            .await?;
        let process_command =
            crate::legacy_command::from_legacy_command(rendered_command.as_deref());
        let client_environment = client_environment_snapshot(requester_pid);
        let spawn_environment = client_spawn_environment(client_environment.as_ref());
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let (response, inline_hooks) = capture_inline_hooks(async {
            let response = {
                let mut state = self.state.lock().await;
                let start_directory = match render_start_directory_template(
                    &state,
                    &Target::Session(target.clone()),
                    attached_count,
                    start_directory.clone(),
                ) {
                    Ok(start_directory) => start_directory,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
                match state.create_window_at_requested_index(
                    &target,
                    target_window_index,
                    insert_at_target,
                    NewWindowOptions {
                        name,
                        detached,
                        spawn: WindowSpawnOptions {
                            start_directory: start_directory.as_deref(),
                            command: process_command.as_ref(),
                            socket_path: &socket_path,
                            spawn_environment: spawn_environment.as_ref(),
                            environment_overrides: environment.as_deref(),
                            pane_alert_callback: Some(self.pane_alert_callback()),
                            pane_exit_callback: Some(self.pane_exit_callback()),
                        },
                    },
                ) {
                    Ok(response) => Response::NewWindow(response),
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            };

            if matches!(response, Response::NewWindow(_)) {
                self.sync_session_silence_timers(&target).await;
                if let Response::NewWindow(success) = &response {
                    self.queue_inline_hook(
                        HookName::AfterNewWindow,
                        ScopeSelector::Session(target.clone()),
                        Some(Target::Pane(PaneTarget::with_window(
                            success.target.session_name().clone(),
                            success.target.window_index(),
                            0,
                        ))),
                        PendingInlineHookFormat::AfterCommand,
                    );
                    self.emit(LifecycleEvent::WindowLinked {
                        session_name: target.clone(),
                        target: Some(success.target.clone()),
                    })
                    .await;
                }
                self.refresh_attached_session(&target).await;
            }

            response
        })
        .await;

        let inline_hook_names = inline_hooks
            .iter()
            .map(|pending| pending.hook)
            .collect::<Vec<_>>();
        self.run_inline_hooks(requester_pid, inline_hooks, None)
            .await;
        self.run_request_hooks(
            requester_pid,
            &request_for_hooks,
            &response,
            None,
            &inline_hook_names,
        )
        .await;

        queue_action_from_response(response)
    }
    async fn render_queued_new_window_command(
        &self,
        command: Option<&[String]>,
        target: &rmux_proto::SessionName,
        context: &QueueExecutionContext,
        attached_count: usize,
        socket_path: &std::path::Path,
    ) -> Result<Option<Vec<String>>, RmuxError> {
        let Some(command) = command else {
            return Ok(None);
        };
        if !command.iter().any(|argument| argument.contains("#{")) {
            return Ok(Some(command.to_vec()));
        }

        let format_target = context
            .current_target()
            .cloned()
            .unwrap_or_else(|| Target::Session(target.clone()));
        let state = self.state.lock().await;
        let mut runtime = format_context_for_target_with_server_values(
            &state,
            &format_target,
            attached_count,
            socket_path,
        )?;
        if let Some(client_name) = context.client_name.as_ref() {
            runtime = runtime.with_named_value("client_name", client_name.clone());
        }

        Ok(Some(
            command
                .iter()
                .map(|argument| render_runtime_template(argument, &runtime, false))
                .collect(),
        ))
    }
}

fn resolve_queued_new_window_target_index(
    sessions: &SessionStore,
    target: &rmux_proto::SessionName,
    target_window_index: Option<NewWindowTargetIndex>,
) -> Result<Option<u32>, RmuxError> {
    let Some(target_window_index) = target_window_index else {
        return Ok(None);
    };

    match target_window_index {
        NewWindowTargetIndex::Absolute(index) => Ok(Some(index)),
        NewWindowTargetIndex::Relative(offset) => {
            let active = sessions
                .session(target)
                .ok_or_else(|| RmuxError::SessionNotFound(target.to_string()))?
                .active_window_index();
            if offset >= 0 {
                Ok(Some(active.checked_add(offset as u32).ok_or_else(
                    || RmuxError::Server("window index space exhausted for new-window".to_owned()),
                )?))
            } else {
                Ok(Some(active.checked_sub(offset.unsigned_abs()).ok_or_else(
                    || RmuxError::invalid_target(target.to_string(), "window offset out of range"),
                )?))
            }
        }
    }
}
