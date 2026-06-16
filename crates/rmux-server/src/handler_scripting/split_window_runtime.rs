use rmux_core::command_parser::ParsedCommand;
use rmux_proto::{DisplayMessageRequest, Response, RmuxError, Target};

use super::pane_parse::ParsedSplitWindowCommand;
use super::queue::{queue_action_from_response, QueueCommandAction};
use super::RequestHandler;

impl RequestHandler {
    pub(super) async fn execute_queued_split_window(
        &self,
        requester_pid: u32,
        command_for_hooks: &ParsedCommand,
        command: ParsedSplitWindowCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
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
