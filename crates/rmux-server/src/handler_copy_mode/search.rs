use std::time::Duration;

use tokio::sync::oneshot;

use rmux_proto::{PaneTarget, RmuxError};

use super::super::prompt_support::{
    CommandPromptPlan, PromptField, PromptQueueResult, PromptStartOutcome, PromptType,
};
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use crate::copy_mode::{CopyModeCommandContext, CopyModeCommandOutcome};
use crate::pane_transcript::SharedPaneTranscript;

const COPY_MODE_PROMPT_TEMPLATE: &str = "display-message -p -- '%%'";
const COPY_MODE_SEARCH_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CopyModeSearchCommandResult {
    pub(super) outcome: CopyModeCommandOutcome,
    pub(super) stop_repeats: bool,
}

impl CopyModeSearchCommandResult {
    fn completed(outcome: CopyModeCommandOutcome) -> Self {
        Self {
            outcome,
            stop_repeats: false,
        }
    }

    fn timed_out() -> Self {
        Self {
            outcome: CopyModeCommandOutcome::nothing(),
            stop_repeats: true,
        }
    }
}

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

    pub(super) async fn execute_copy_mode_search_command(
        &self,
        target_transcript: &SharedPaneTranscript,
        command: &str,
        args: &[String],
        context: &CopyModeCommandContext,
    ) -> Result<CopyModeSearchCommandResult, RmuxError> {
        let original_mode = {
            let transcript = target_transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            let Some(mode) = transcript.copy_mode_state() else {
                return Err(RmuxError::Server("pane is not in copy mode".to_owned()));
            };
            mode.clone()
        };

        let mut computed_mode = original_mode.clone();
        let command = command.to_owned();
        let args = args.to_vec();
        let context = context.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let outcome = computed_mode.execute_command(&command, &args, &context)?;
            Ok::<_, RmuxError>((computed_mode, outcome))
        });

        match tokio::time::timeout(COPY_MODE_SEARCH_TIMEOUT, worker).await {
            Ok(Ok(Ok((computed_mode, outcome)))) => {
                let mut transcript = target_transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned");
                if transcript.copy_mode_state() == Some(&original_mode) {
                    transcript.set_copy_mode_state(Some(computed_mode));
                }
                Ok(CopyModeSearchCommandResult::completed(outcome))
            }
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(error)) => Err(RmuxError::Server(format!(
                "copy-mode search worker failed: {error}"
            ))),
            Err(_) => {
                let mut transcript = target_transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned");
                if let Some(mode) = transcript.copy_mode_state_mut() {
                    if mode == &original_mode {
                        mode.mark_search_timed_out();
                    }
                }
                Ok(CopyModeSearchCommandResult::timed_out())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CopyModeSearchCommandResult;

    #[test]
    fn timed_out_search_result_stops_repeats_without_canceling_copy_mode() {
        let result = CopyModeSearchCommandResult::timed_out();

        assert!(result.stop_repeats);
        assert!(!result.outcome.cancel);
        assert!(result.outcome.transfer.is_none());
    }
}
