use rmux_proto::{OptionName, RmuxError};
use tokio::sync::oneshot;
use tracing::warn;

use super::super::control_support::ManagedClient;
use super::super::scripting_support::{
    command_parser_from_state, spawn_background_async, ParsedPromptHistoryCommand,
    PromptHistoryAction, QueueCommandAction,
};
use super::super::RequestHandler;
use super::events::process_prompt_event;
use super::substitution::substitute_prompt_template;
use super::{
    prompt_accept_should_dismiss_mode_tree, ClientPromptState, CommandPromptPlan,
    ConfirmBeforePlan, FinishedPrompt, FinishedPromptKind, PromptCompletion, PromptDispatch,
    PromptFinalizeKind, PromptInputEvent, PromptQueueResult, PromptStartOutcome, PromptType,
};
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::renderer::RenderedPrompt;

impl RequestHandler {
    pub(in crate::handler) async fn start_command_prompt(
        &self,
        plan: CommandPromptPlan,
    ) -> Result<PromptStartOutcome, RmuxError> {
        let managed = match self
            .resolve_target_managed_client(
                plan.requester_pid,
                plan.target_client.as_deref(),
                "command-prompt",
            )
            .await
        {
            Ok(managed) => managed,
            Err(RmuxError::Server(message))
                if message == "command-prompt requires an attached client" =>
            {
                return Err(RmuxError::Message("no current client".to_owned()));
            }
            Err(RmuxError::Server(message)) if message.starts_with("can't find client: ") => {
                return Err(RmuxError::Message(message));
            }
            Err(error) => return Err(error),
        };
        let attach_pid = match managed {
            ManagedClient::Attach(attach_pid) => attach_pid,
            ManagedClient::Control(_) => {
                return Ok(PromptStartOutcome::Immediate);
            }
        };

        let (prompt, outcome) = if plan.background {
            (
                ClientPromptState::new_command(plan, PromptCompletion::Background),
                PromptStartOutcome::Immediate,
            )
        } else {
            let (tx, rx) = oneshot::channel();
            (
                ClientPromptState::new_command(plan, PromptCompletion::Foreground(tx)),
                PromptStartOutcome::Waiting(rx),
            )
        };
        let initial_dispatch = prompt.initial_incremental_dispatch();
        let installed = self.install_prompt(attach_pid, prompt).await?;
        if !installed {
            return Ok(PromptStartOutcome::Immediate);
        }
        if let Some(dispatch) = initial_dispatch {
            self.dispatch_prompt_commands(dispatch).await;
        }
        Ok(outcome)
    }

    pub(in crate::handler) async fn start_confirm_before(
        &self,
        plan: ConfirmBeforePlan,
    ) -> Result<PromptStartOutcome, RmuxError> {
        let managed = match self
            .resolve_target_managed_client(
                plan.requester_pid,
                plan.target_client.as_deref(),
                "confirm-before",
            )
            .await
        {
            Ok(managed) => managed,
            Err(RmuxError::Server(message))
                if message == "confirm-before requires an attached client" =>
            {
                return Err(RmuxError::Message("no current client".to_owned()));
            }
            Err(RmuxError::Server(message)) if message.starts_with("can't find client: ") => {
                return Err(RmuxError::Message(message));
            }
            Err(error) => return Err(error),
        };
        let attach_pid = match managed {
            ManagedClient::Attach(attach_pid) => attach_pid,
            ManagedClient::Control(_) => {
                return Ok(PromptStartOutcome::Immediate);
            }
        };

        let (prompt, outcome) = if plan.background {
            (
                ClientPromptState::new_confirm(plan, PromptCompletion::Background),
                PromptStartOutcome::Immediate,
            )
        } else {
            let (tx, rx) = oneshot::channel();
            (
                ClientPromptState::new_confirm(plan, PromptCompletion::Foreground(tx)),
                PromptStartOutcome::Waiting(rx),
            )
        };
        if !self.install_prompt(attach_pid, prompt).await? {
            return Ok(PromptStartOutcome::Immediate);
        }
        Ok(outcome)
    }

    async fn install_prompt(
        &self,
        attach_pid: u32,
        prompt: ClientPromptState,
    ) -> Result<bool, RmuxError> {
        let (session_name, control_tx, render_generation, overlay_generation) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if active.prompt.is_some() {
                return Ok(false);
            }
            active.prompt = Some(prompt);
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            (
                active.session_name.clone(),
                active.control_tx.clone(),
                active.render_generation,
                active.overlay_generation,
            )
        };

        let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::new(
            Vec::new(),
            render_generation,
            overlay_generation,
        )));
        self.refresh_attached_client(attach_pid, &session_name)
            .await;
        Ok(true)
    }

    #[allow(dead_code)]
    pub(in crate::handler) async fn attached_prompt_render(
        &self,
        attach_pid: u32,
    ) -> Option<RenderedPrompt> {
        let active_attach = self.active_attach.lock().await;
        active_attach.by_pid.get(&attach_pid).and_then(|active| {
            active
                .prompt
                .as_ref()
                .map(ClientPromptState::rendered_prompt)
        })
    }

    pub(in crate::handler) async fn prompt_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.prompt.is_some())
    }

    pub(in crate::handler) async fn handle_prompt_event_deferred_refresh(
        &self,
        attach_pid: u32,
        event: PromptInputEvent,
        deferred_refresh: &mut bool,
    ) -> Result<(), RmuxError> {
        let session_name = self.attached_session_name(attach_pid).await?;
        let separators = {
            let state = self.state.lock().await;
            state
                .options
                .resolve(Some(&session_name), OptionName::WordSeparators)
                .unwrap_or_default()
                .to_owned()
        };
        let history_limit = {
            let state = self.state.lock().await;
            state
                .options
                .resolve(None, OptionName::PromptHistoryLimit)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(100)
        };

        let (action, finished) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(prompt) = active.prompt.as_mut() else {
                return Ok(());
            };
            let mut history = self.prompt_history.lock().await;
            let action =
                process_prompt_event(prompt, event, &mut history, &separators, history_limit);
            let finished = action.finalize.as_ref().map(|kind| {
                active
                    .prompt
                    .take()
                    .expect("prompt exists")
                    .into_finished(kind.clone())
            });
            (action, finished)
        };

        if action.refresh && finished.is_none() {
            *deferred_refresh = true;
        }
        if let Some(dispatch) = action.dispatch {
            self.dispatch_prompt_commands(dispatch).await;
        }
        if let Some(finished) = finished {
            self.finish_prompt(finished, attach_pid).await;
        }

        Ok(())
    }

    pub(in crate::handler) async fn flush_attached_prompt_refresh(
        &self,
        attach_pid: u32,
    ) -> Result<(), RmuxError> {
        if !self.prompt_active(attach_pid).await {
            return Ok(());
        }
        let session_name = self.attached_session_name(attach_pid).await?;
        self.refresh_attached_client(attach_pid, &session_name)
            .await;
        Ok(())
    }

    pub(in crate::handler) async fn clear_prompt_for_attach(&self, attach_pid: u32) {
        let finished = {
            let mut active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get_mut(&attach_pid)
                .and_then(|active| active.prompt.take())
                .map(|prompt| prompt.into_finished(PromptFinalizeKind::Cancel))
        };
        if let Some(finished) = finished {
            self.finish_prompt(finished, attach_pid).await;
        }
    }

    async fn finish_prompt(&self, finished: FinishedPrompt, attach_pid: u32) {
        if let Ok(session_name) = self.attached_session_name(attach_pid).await {
            if prompt_accept_should_dismiss_mode_tree(&finished) {
                self.dismiss_mode_tree_for_session(&session_name).await;
            }
            if matches!(finished.kind, FinishedPromptKind::Cancel) {
                self.refresh_attached_client(attach_pid, &session_name)
                    .await;
            } else {
                self.refresh_attached_client_base_only(attach_pid, &session_name)
                    .await;
            }
        }

        match finished.kind {
            FinishedPromptKind::Cancel => {
                if let PromptCompletion::Foreground(sender) = finished.completion {
                    let _ = sender.send(PromptQueueResult::noop());
                }
            }
            FinishedPromptKind::Command {
                template,
                format_values,
                responses,
            } => {
                let parsed = self
                    .parse_prompt_commands(&template, &format_values, &responses)
                    .await;
                match finished.completion {
                    PromptCompletion::Foreground(sender) => {
                        let _ = sender.send(match parsed {
                            Ok(parsed) => PromptQueueResult {
                                inserted: Some((parsed, finished.context)),
                                error: None,
                                responses: Some(responses),
                            },
                            Err(error) => PromptQueueResult {
                                inserted: None,
                                error: Some(error),
                                responses: Some(responses),
                            },
                        });
                    }
                    PromptCompletion::Background => match parsed {
                        Ok(parsed) => {
                            let handler = self.clone();
                            let requester_pid = finished.requester_pid;
                            let context = finished.context;
                            let _ =
                                spawn_background_async("rmux-prompt-finish", move || async move {
                                    let _ = handler
                                        .execute_parsed_commands(requester_pid, parsed, context)
                                        .await;
                                });
                        }
                        Err(error) => {
                            warn!("background prompt command failed to parse: {error}");
                        }
                    },
                }
            }
        }
    }

    /// Runs `show-prompt-history`, returning the rendered tmux-compatible history body.
    pub(super) async fn show_prompt_history(
        &self,
        selected: Option<PromptType>,
    ) -> Result<String, RmuxError> {
        let history = self.prompt_history.lock().await;
        Ok(history.render(selected))
    }

    /// Runs `clear-prompt-history`, dropping entries for the selected type or all types.
    pub(super) async fn clear_prompt_history(
        &self,
        selected: Option<PromptType>,
    ) -> Result<(), RmuxError> {
        let mut history = self.prompt_history.lock().await;
        history.clear(selected);
        Ok(())
    }

    /// Routes a parsed prompt-history queue command to the right store operation.
    pub(in crate::handler) async fn execute_queued_prompt_history(
        &self,
        command: ParsedPromptHistoryCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        match command.action {
            PromptHistoryAction::Show => {
                let body = self.show_prompt_history(command.prompt_type).await?;
                Ok(QueueCommandAction::Normal {
                    output: Some(rmux_proto::CommandOutput::from_stdout(body.into_bytes())),
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                })
            }
            PromptHistoryAction::Clear => {
                self.clear_prompt_history(command.prompt_type).await?;
                Ok(QueueCommandAction::Normal {
                    output: None,
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                })
            }
        }
    }

    async fn dispatch_prompt_commands(&self, dispatch: PromptDispatch) {
        let parsed = self
            .parse_prompt_commands(
                &dispatch.template,
                &dispatch.format_values,
                &dispatch.responses,
            )
            .await;
        match parsed {
            Ok(parsed) => {
                let handler = self.clone();
                let _ = spawn_background_async("rmux-prompt-dispatch", move || async move {
                    let _ = handler
                        .execute_parsed_commands(dispatch.requester_pid, parsed, dispatch.context)
                        .await;
                });
            }
            Err(error) => warn!("prompt command failed to parse: {error}"),
        }
    }

    async fn parse_prompt_commands(
        &self,
        template: &str,
        format_values: &[(String, String)],
        responses: &[String],
    ) -> Result<rmux_core::command_parser::ParsedCommands, RmuxError> {
        let substituted = substitute_prompt_template(template, responses);
        let state = self.state.lock().await;
        let mut parser = command_parser_from_state(&state);
        for (name, value) in format_values {
            parser = parser.with_format_value(name, value.clone());
        }
        parser
            .parse_one_group(&substituted)
            .map_err(|error| RmuxError::Server(error.to_string()))
    }
}
