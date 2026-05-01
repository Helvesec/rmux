use tokio::sync::oneshot;

use rmux_proto::{PaneTarget, RmuxError};

use super::super::prompt_support::{
    CommandPromptPlan, PromptField, PromptQueueResult, PromptStartOutcome, PromptType,
};
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;

const COPY_MODE_PROMPT_TEMPLATE: &str = "display-message -p -- '%%'";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttachedCopyModeSearchDirection {
    Forward,
    Backward,
}

impl RequestHandler {
    pub(super) async fn start_copy_mode_search_prompt(
        &self,
        attach_pid: u32,
        target: PaneTarget,
        direction: AttachedCopyModeSearchDirection,
    ) -> Result<(), RmuxError> {
        let plan = CommandPromptPlan {
            requester_pid: attach_pid,
            target_client: None,
            context: QueueExecutionContext::without_caller_cwd(),
            fields: vec![PromptField {
                prompt: match direction {
                    AttachedCopyModeSearchDirection::Forward => "(search down) ".to_owned(),
                    AttachedCopyModeSearchDirection::Backward => "(search up) ".to_owned(),
                },
                input: String::new(),
            }],
            template: COPY_MODE_PROMPT_TEMPLATE.to_owned(),
            flags: 0,
            prompt_type: PromptType::Search,
            background: false,
            format_values: Vec::new(),
        };

        if let PromptStartOutcome::Waiting(rx) = self.start_command_prompt(plan).await? {
            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .await_copy_mode_search_prompt(attach_pid, target, direction, rx)
                    .await;
            });
        }
        Ok(())
    }

    async fn await_copy_mode_search_prompt(
        &self,
        attach_pid: u32,
        target: PaneTarget,
        direction: AttachedCopyModeSearchDirection,
        rx: oneshot::Receiver<PromptQueueResult>,
    ) {
        let Ok(result) = rx.await else {
            return;
        };
        let Some(responses) = result.responses else {
            return;
        };
        let Some(query) = responses.first().filter(|query| !query.is_empty()) else {
            return;
        };
        let command = match direction {
            AttachedCopyModeSearchDirection::Forward => "search-forward",
            AttachedCopyModeSearchDirection::Backward => "search-backward",
        };
        let args = vec!["--".to_owned(), query.clone()];
        let _ = self
            .execute_copy_mode_command(attach_pid, target, command, &args, 1)
            .await;
    }
}
