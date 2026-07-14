use std::path::Path;

use rmux_core::command_parser::ParsedCommand;
use rmux_proto::{
    DisplayMessageRequest, Request, Response, RmuxError, SplitWindowExtRequest, Target,
};

use super::format_context_for_target_with_server_values;
use super::pane_parse::ParsedSplitWindowCommand;
use super::queue::{queue_action_from_response, QueueCommandAction, QueueExecutionContext};
use super::RequestHandler;
use crate::format_runtime::render_runtime_template;

impl RequestHandler {
    pub(super) async fn execute_queued_split_window(
        &self,
        requester_pid: u32,
        command_for_hooks: &ParsedCommand,
        mut command: ParsedSplitWindowCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        command.request =
            split_window_with_caller_cwd(command.request, context.caller_cwd.as_deref());
        command.request = self
            .render_queued_split_window_command(command.request, context)
            .await?;
        let can_write = self.requester_can_write(requester_pid).await;
        let request =
            crate::server_access::apply_access_policy(command.request.clone(), can_write)?;
        let request_for_hooks = request.clone();
        let (outcome, inline_hooks) =
            Box::pin(self.dispatch_captured(requester_pid, u64::from(requester_pid), request))
                .await;
        let inline_hook_names = inline_hooks
            .iter()
            .map(|pending| pending.hook)
            .collect::<Vec<_>>();
        self.run_inline_hooks(requester_pid, inline_hooks, Some(command_for_hooks))
            .await;
        self.run_request_hooks(
            requester_pid,
            &request_for_hooks,
            &outcome.response,
            Some(command_for_hooks),
            &inline_hook_names,
        )
        .await;
        self.queued_split_window_action(requester_pid, command, outcome.response)
            .await
    }

    async fn render_queued_split_window_command(
        &self,
        request: Request,
        context: &QueueExecutionContext,
    ) -> Result<Request, RmuxError> {
        let mut request = match request {
            Request::SplitWindowExt(request) => request,
            request => return Ok(request),
        };
        let Some(command) = request.command.clone() else {
            return Ok(Request::SplitWindowExt(request));
        };
        if !command.iter().any(|argument| argument.contains("#{")) {
            return Ok(Request::SplitWindowExt(request));
        }

        let format_target = match &request.target {
            rmux_proto::SplitWindowTarget::Session(session_name) => {
                Target::Session(session_name.clone())
            }
            rmux_proto::SplitWindowTarget::Pane(target) => Target::Pane(target.clone()),
        };
        let attached_count = self.attached_count(format_target.session_name()).await;
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let mut runtime = format_context_for_target_with_server_values(
            &state,
            &format_target,
            attached_count,
            &socket_path,
        )?;
        if let Some(client_name) = context.client_name.as_ref() {
            runtime = runtime.with_named_value("client_name", client_name.clone());
        }

        request.command = Some(
            command
                .into_iter()
                .map(|argument| render_runtime_template(&argument, &runtime, false))
                .collect(),
        );
        Ok(Request::SplitWindowExt(request))
    }

    async fn queued_split_window_action(
        &self,
        requester_pid: u32,
        command: ParsedSplitWindowCommand,
        response: Response,
    ) -> Result<QueueCommandAction, RmuxError> {
        let pane = match &response {
            Response::SplitWindow(response) if command.print_target => response.pane.clone(),
            _ => return queue_action_from_response(response),
        };
        let printed = self
            .handle_display_message(
                requester_pid,
                DisplayMessageRequest {
                    target: Some(Target::Pane(pane)),
                    print: true,
                    message: Some(command.format),
                    empty_target_context: false,
                },
            )
            .await;
        queue_action_from_response(printed)
    }
}

fn split_window_with_caller_cwd(request: Request, caller_cwd: Option<&Path>) -> Request {
    let Some(caller_cwd) = caller_cwd else {
        return request;
    };

    match request {
        Request::SplitWindow(request) => Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: request.target,
            direction: request.direction,
            before: request.before,
            environment: request.environment,
            command: None,
            process_command: None,
            start_directory: Some(caller_cwd.to_path_buf()),
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })),
        Request::SplitWindowExt(mut request) => {
            if request.start_directory.is_none() {
                request.start_directory = Some(caller_cwd.to_path_buf());
            }
            Request::SplitWindowExt(request)
        }
        request => request,
    }
}
