use rmux_core::{
    command_parser::{CommandParser, ParsedCommand},
    formats::{FormatContext, FormatVariables},
};
use rmux_proto::RmuxError;

use super::super::prompt_support::{
    CommandPromptPlan, ConfirmBeforePlan, PromptField, PromptQueueResult, PromptStartOutcome,
};
use super::super::RequestHandler;
use super::command_args::CommandListArgument;
use super::format_context::{collect_parse_time_values, format_context_for_target};
use super::parser_context::command_parser_from_state;
use super::prompt_parse::{ParsedCommandPromptCommand, ParsedConfirmBeforeCommand};
use super::queue::{prompt_queue_action_from_result, QueueCommandAction, QueueExecutionContext};
use super::runtime::spawn_background_async;
use super::targets::active_session_target;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

impl RequestHandler {
    pub(super) async fn execute_queued_command_prompt(
        &self,
        requester_pid: u32,
        command: ParsedCommandPromptCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let plan = self
            .build_command_prompt_plan(requester_pid, command, context)
            .await?;
        let result = match self.start_command_prompt(plan).await? {
            PromptStartOutcome::Immediate => {
                return Ok(QueueCommandAction::Normal {
                    output: None,
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                });
            }
            PromptStartOutcome::Waiting(receiver) => {
                receiver.await.unwrap_or_else(|_| PromptQueueResult::noop())
            }
        };
        Ok(prompt_queue_action_from_result(result))
    }

    pub(super) async fn start_attached_command_prompt_binding(
        &self,
        requester_pid: u32,
        command: ParsedCommandPromptCommand,
        context: &QueueExecutionContext,
    ) -> Result<(), RmuxError> {
        let plan = self
            .build_command_prompt_plan(requester_pid, command, context)
            .await?;
        let PromptStartOutcome::Waiting(receiver) = self.start_command_prompt(plan).await? else {
            return Ok(());
        };

        self.finish_attached_prompt_binding(requester_pid, receiver);
        Ok(())
    }

    async fn build_command_prompt_plan(
        &self,
        requester_pid: u32,
        command: ParsedCommandPromptCommand,
        context: &QueueExecutionContext,
    ) -> Result<CommandPromptPlan, RmuxError> {
        let session_candidate = if context.current_target.is_none() {
            self.current_session_candidate(requester_pid).await
        } else {
            None
        };
        let attached_count = match context.current_target.as_ref() {
            Some(target) => self.attached_count(target.session_name()).await,
            None => match session_candidate.as_ref() {
                Some(session_name) => self.attached_count(session_name).await,
                None => 0,
            },
        };
        let (template, fields, format_values) = {
            let state = self.state.lock().await;
            let format_target = context.current_target.clone().or_else(|| {
                session_candidate
                    .as_ref()
                    .and_then(|session_name| active_session_target(&state.sessions, session_name))
            });
            let runtime = match format_target.as_ref() {
                Some(target) => format_context_for_target(&state, target, attached_count)?,
                None => RuntimeFormatContext::new(FormatContext::new()).with_state(&state),
            };
            let template = match &command.template {
                Some(CommandListArgument::Parsed(commands)) => commands.to_tmux_reparse_string(),
                Some(CommandListArgument::String(value)) if command.format_template => {
                    render_runtime_template(value, &runtime, true)
                }
                Some(CommandListArgument::String(value)) => value.clone(),
                None => "%1".to_owned(),
            };
            let default_prompts = if command.template.is_some() {
                format!(
                    "({})",
                    command_prompt_default_label(command.template.as_ref(), &template)
                )
            } else {
                ":".to_owned()
            };
            let fields = if command.literal {
                vec![PromptField {
                    prompt: match &command.prompts {
                        Some(prompts) => render_runtime_template(prompts, &runtime, true),
                        None => default_prompts.clone(),
                    },
                    input: command
                        .inputs
                        .as_deref()
                        .map(|inputs| render_runtime_template(inputs, &runtime, true))
                        .unwrap_or_default(),
                }]
            } else {
                render_split_prompt_fields(
                    command.prompts.as_deref().unwrap_or(&default_prompts),
                    command.inputs.as_deref(),
                    command.prompts.is_some() || command.template.is_some(),
                    &runtime,
                )
            };

            (template, fields, collect_parse_time_values(&runtime))
        };

        Ok(CommandPromptPlan {
            requester_pid,
            target_client: command.target_client.clone(),
            context: context.clone(),
            fields,
            template,
            flags: command.flags,
            prompt_type: command.prompt_type,
            background: command.background,
            format_values,
        })
    }

    pub(super) async fn execute_queued_confirm_before(
        &self,
        requester_pid: u32,
        command: ParsedConfirmBeforeCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let plan = self
            .build_confirm_before_plan(requester_pid, command, context)
            .await?;
        let result = match self.start_confirm_before(plan).await? {
            PromptStartOutcome::Immediate => {
                return Ok(QueueCommandAction::Normal {
                    output: None,
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                });
            }
            PromptStartOutcome::Waiting(receiver) => {
                receiver.await.unwrap_or_else(|_| PromptQueueResult::noop())
            }
        };
        Ok(prompt_queue_action_from_result(result))
    }

    pub(super) async fn start_attached_confirm_before_binding(
        &self,
        requester_pid: u32,
        command: ParsedConfirmBeforeCommand,
        context: &QueueExecutionContext,
    ) -> Result<(), RmuxError> {
        let plan = self
            .build_confirm_before_plan(requester_pid, command, context)
            .await?;
        let PromptStartOutcome::Waiting(receiver) = self.start_confirm_before(plan).await? else {
            return Ok(());
        };

        self.finish_attached_prompt_binding(requester_pid, receiver);
        Ok(())
    }

    fn finish_attached_prompt_binding(
        &self,
        requester_pid: u32,
        receiver: tokio::sync::oneshot::Receiver<PromptQueueResult>,
    ) {
        let handler = self.clone();
        let task = finish_attached_prompt_binding_task(handler, requester_pid, receiver);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(task);
        } else {
            let _ = spawn_background_async("rmux-attached-prompt-finish", move || task);
        }
    }

    async fn build_confirm_before_plan(
        &self,
        requester_pid: u32,
        command: ParsedConfirmBeforeCommand,
        context: &QueueExecutionContext,
    ) -> Result<ConfirmBeforePlan, RmuxError> {
        let session_candidate = if context.current_target.is_none() {
            self.current_session_candidate(requester_pid).await
        } else {
            None
        };
        let attached_count = match context.current_target.as_ref() {
            Some(target) => self.attached_count(target.session_name()).await,
            None => match session_candidate.as_ref() {
                Some(session_name) => self.attached_count(session_name).await,
                None => 0,
            },
        };
        let (prompt, template, format_values) = {
            let state = self.state.lock().await;
            let format_target = context.current_target.clone().or_else(|| {
                session_candidate
                    .as_ref()
                    .and_then(|session_name| active_session_target(&state.sessions, session_name))
            });
            let runtime = match format_target.as_ref() {
                Some(target) => format_context_for_target(&state, target, attached_count)?,
                None => RuntimeFormatContext::new(FormatContext::new()).with_state(&state),
            };
            let format_values = collect_parse_time_values(&runtime);
            let parsed = match &command.command {
                CommandListArgument::Parsed(commands) => commands.clone(),
                CommandListArgument::String(value) => {
                    let mut parser = command_parser_from_state(&state);
                    for (name, value) in &format_values {
                        parser = parser.with_format_value(name, value.clone());
                    }
                    parser
                        .parse_one_group(value)
                        .map_err(super::command_parse_error_to_rmux)?
                }
            };
            let template = parsed.to_tmux_reparse_string();
            let prompt = match &command.prompt {
                Some(prompt) => format!("{} ", render_runtime_template(prompt, &runtime, true)),
                None => {
                    let name = parsed
                        .commands()
                        .first()
                        .map(ParsedCommand::name)
                        .unwrap_or("");
                    format!("Confirm '{name}'? ({}/n) ", command.confirm_key)
                }
            };
            (prompt, template, format_values)
        };

        Ok(ConfirmBeforePlan {
            requester_pid,
            target_client: command.target_client.clone(),
            context: context.clone(),
            prompt,
            template,
            confirm_key: command.confirm_key,
            default_yes: command.default_yes,
            background: command.background,
            format_values,
        })
    }
}

async fn finish_attached_prompt_binding_task(
    handler: RequestHandler,
    requester_pid: u32,
    receiver: tokio::sync::oneshot::Receiver<PromptQueueResult>,
) {
    let result = receiver.await.unwrap_or_else(|_| PromptQueueResult::noop());
    if let Some((commands, context)) = result.inserted {
        let _ = handler
            .execute_parsed_commands(requester_pid, commands, context)
            .await;
    }
}

fn command_prompt_default_label(template: Option<&CommandListArgument>, rendered: &str) -> String {
    match template {
        Some(CommandListArgument::Parsed(commands)) => commands
            .commands()
            .first()
            .map(|command| command.name().to_owned())
            .unwrap_or_else(|| rendered.to_owned()),
        Some(CommandListArgument::String(_)) => CommandParser::new()
            .parse_one_group(rendered)
            .ok()
            .and_then(|commands| {
                commands
                    .commands()
                    .first()
                    .map(|command| command.name().to_owned())
            })
            .unwrap_or_else(|| rendered.to_owned()),
        None => ":".to_owned(),
    }
}

fn render_split_prompt_fields<V>(
    prompts: &str,
    inputs: Option<&str>,
    append_space: bool,
    variables: &V,
) -> Vec<PromptField>
where
    V: FormatVariables + ?Sized,
{
    let mut input_parts = inputs.map(|value| value.split(','));
    prompts
        .split(',')
        .map(|prompt| {
            let mut prompt = render_runtime_template(prompt, variables, true);
            if append_space {
                prompt.push(' ');
            }
            PromptField {
                prompt,
                input: input_parts
                    .as_mut()
                    .and_then(|parts| parts.next())
                    .map(|input| render_runtime_template(input, variables, true))
                    .unwrap_or_default(),
            }
        })
        .collect()
}
