use rmux_core::{
    command_parser::{CommandParser, ParsedCommand},
    formats::{FormatContext, FormatVariables},
};
use rmux_proto::{RmuxError, SessionId, SessionName};

use super::super::prompt_support::{
    CommandPromptPlan, ConfirmBeforePlan, PromptField, PromptQueueResult, PromptStartOutcome,
};
use super::super::{
    attach_support::ActiveAttachIdentity, with_expected_attach_and_session_identity, RequestHandler,
};
use super::command_args::CommandListArgument;
use super::format_context::{
    apply_client_loop_context, format_context_for_target, ParseTimeFormats,
};
use super::parser_context::command_parser_from_state;
use super::prompt_parse::{ParsedCommandPromptCommand, ParsedConfirmBeforeCommand};
use super::queue::{prompt_queue_action_from_result, QueueCommandAction, QueueExecutionContext};
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

        self.finish_attached_prompt_binding(requester_pid, None, receiver);
        Ok(())
    }

    pub(super) async fn start_attached_command_prompt_binding_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: SessionName,
        session_id: SessionId,
        requester_pid: u32,
        command: ParsedCommandPromptCommand,
        context: &QueueExecutionContext,
    ) -> Result<(), RmuxError> {
        let plan = self
            .build_command_prompt_plan(requester_pid, command, context)
            .await?;
        let PromptStartOutcome::Waiting(receiver) = with_expected_attach_and_session_identity(
            identity,
            session_name.clone(),
            session_id,
            Box::pin(self.start_command_prompt(plan)),
        )
        .await?
        else {
            return Ok(());
        };
        self.finish_attached_prompt_binding(
            requester_pid,
            Some((identity, session_name, session_id)),
            receiver,
        );
        Ok(())
    }

    async fn build_command_prompt_plan(
        &self,
        requester_pid: u32,
        command: ParsedCommandPromptCommand,
        context: &QueueExecutionContext,
    ) -> Result<CommandPromptPlan, RmuxError> {
        let origin = self.capture_requester_origin(requester_pid).await;
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
        let owned_client_loop_snapshots = if context.client_loop_snapshots().is_none() {
            Some(self.client_loop_snapshots().await)
        } else {
            None
        };
        let client_loop_snapshots = context
            .client_loop_snapshots()
            .or(owned_client_loop_snapshots.as_deref());
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
            let runtime = apply_client_loop_context(
                runtime,
                client_loop_snapshots,
                context.client_name.as_deref(),
            );
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

            // The template is parsed once the prompt completes; a response
            // replaces only a `%` reference, so a template without one parses
            // as written and cannot read the parse-time values.
            let format_values = ParseTimeFormats::of(&runtime)
                .for_source(&template)
                .to_vec();
            (template, fields, format_values)
        };

        Ok(CommandPromptPlan {
            origin,
            target_client: command.target_client.clone(),
            context: if command.background {
                context.clone().without_mouse_origin()
            } else {
                context.clone()
            },
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

        self.finish_attached_prompt_binding(requester_pid, None, receiver);
        Ok(())
    }

    pub(super) async fn start_attached_confirm_before_binding_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: SessionName,
        session_id: SessionId,
        requester_pid: u32,
        command: ParsedConfirmBeforeCommand,
        context: &QueueExecutionContext,
    ) -> Result<(), RmuxError> {
        let plan = self
            .build_confirm_before_plan(requester_pid, command, context)
            .await?;
        let PromptStartOutcome::Waiting(receiver) = with_expected_attach_and_session_identity(
            identity,
            session_name.clone(),
            session_id,
            Box::pin(self.start_confirm_before(plan)),
        )
        .await?
        else {
            return Ok(());
        };
        self.finish_attached_prompt_binding(
            requester_pid,
            Some((identity, session_name, session_id)),
            receiver,
        );
        Ok(())
    }

    fn finish_attached_prompt_binding(
        &self,
        requester_pid: u32,
        identity: Option<(ActiveAttachIdentity, SessionName, SessionId)>,
        receiver: tokio::sync::oneshot::Receiver<PromptQueueResult>,
    ) {
        let handler = self.clone();
        let task = finish_attached_prompt_binding_task(handler, requester_pid, identity, receiver);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(task);
        } else {
            let _ = self.spawn_background_task("rmux-attached-prompt-finish", move || task);
        }
    }

    async fn build_confirm_before_plan(
        &self,
        requester_pid: u32,
        command: ParsedConfirmBeforeCommand,
        context: &QueueExecutionContext,
    ) -> Result<ConfirmBeforePlan, RmuxError> {
        let origin = self.capture_requester_origin(requester_pid).await;
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
        let owned_client_loop_snapshots = if context.client_loop_snapshots().is_none() {
            Some(self.client_loop_snapshots().await)
        } else {
            None
        };
        let client_loop_snapshots = context
            .client_loop_snapshots()
            .or(owned_client_loop_snapshots.as_deref());
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
            let runtime = apply_client_loop_context(
                runtime,
                client_loop_snapshots,
                context.client_name.as_deref(),
            );
            let parse_time_formats = ParseTimeFormats::of(&runtime);
            let parsed = match &command.command {
                CommandListArgument::Parsed(commands) => commands.clone(),
                CommandListArgument::String(value) => parse_time_formats
                    .seed_parser(command_parser_from_state(&state), value)
                    .parse_one_group(value)
                    .map_err(super::command_parse_error_to_rmux)?,
            };
            let template = parsed.to_tmux_reparse_string();
            // Confirming parses the template again with no response to
            // substitute, so the values follow what that parse can read.
            let format_values = parse_time_formats.for_source(&template).to_vec();
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
            origin,
            target_client: command.target_client.clone(),
            context: if command.background {
                context.clone().without_mouse_origin()
            } else {
                context.clone()
            },
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
    identity: Option<(ActiveAttachIdentity, SessionName, SessionId)>,
    receiver: tokio::sync::oneshot::Receiver<PromptQueueResult>,
) {
    let result = receiver.await.unwrap_or_else(|_| PromptQueueResult::noop());
    let _access = result
        .origin
        .as_ref()
        .map(|origin| handler.begin_requester_origin_access(origin));
    if let Some((commands, context)) = result.inserted {
        match identity {
            Some((identity, session_name, session_id)) => {
                if !handler.current_live_attach_input(identity).await {
                    return;
                }
                let _ = with_expected_attach_and_session_identity(
                    identity,
                    session_name,
                    session_id,
                    handler.execute_parsed_commands(requester_pid, commands, context),
                )
                .await;
            }
            None => {
                let _ = handler
                    .execute_parsed_commands(requester_pid, commands, context)
                    .await;
            }
        }
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

#[cfg(test)]
mod tests {
    // Proves that command-prompt and confirm-before capture the parse-time values
    // only through `ParseTimeFormats`: a prompt whose later parse cannot read
    // them must not capture them.
    use rmux_proto::{NewSessionRequest, Request, SessionName, Target, TerminalSize};

    use super::super::super::prompt_support::PromptType;
    use super::super::command_args::CommandListArgument;
    use super::super::format_context::parse_time_captures_on_this_thread;
    use super::super::prompt_parse::{ParsedCommandPromptCommand, ParsedConfirmBeforeCommand};
    use super::super::queue::QueueExecutionContext;
    use super::RequestHandler;

    const ALPHA_CONDITIONAL: &str =
        "%if #{==:#{session_name},alpha}\nlist-sessions\n%else\nlist-windows\n%endif";

    async fn handler_on_alpha() -> (RequestHandler, QueueExecutionContext) {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session name");
        crate::session_fixture::assert_new_session(
            &handler
                .handle(Request::NewSession(NewSessionRequest {
                    session_name: session_name.clone(),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }))
                .await,
        );
        let context = QueueExecutionContext::without_caller_cwd()
            .with_current_target(Some(Target::Session(session_name)));
        (handler, context)
    }

    fn holds_alpha(values: &[(String, String)]) -> bool {
        values
            .iter()
            .any(|(name, value)| name == "session_name" && value == "alpha")
    }

    /// Builds a command-prompt plan for `template` and returns its stored
    /// template, its parse-time values and the captures it made on this thread.
    async fn command_prompt_plan(template: &str) -> (String, Vec<(String, String)>, usize) {
        let (handler, context) = handler_on_alpha().await;
        let command = ParsedCommandPromptCommand {
            background: false,
            format_template: false,
            literal: false,
            prompts: Some("go".to_owned()),
            inputs: None,
            prompt_type: PromptType::Command,
            target_client: None,
            flags: 0,
            template: Some(CommandListArgument::String(template.to_owned())),
        };
        let captures_before = parse_time_captures_on_this_thread();
        let plan = handler
            .build_command_prompt_plan(std::process::id(), command, &context)
            .await
            .expect("command-prompt plan builds");
        (
            plan.template,
            plan.format_values,
            parse_time_captures_on_this_thread() - captures_before,
        )
    }

    /// Builds a confirm-before plan for `source` and returns its template, its
    /// parse-time values and the captures it made on this thread.
    async fn confirm_before_plan(source: &str) -> (String, Vec<(String, String)>, usize) {
        let (handler, context) = handler_on_alpha().await;
        let command = ParsedConfirmBeforeCommand {
            background: false,
            prompt: None,
            confirm_key: 'y',
            default_yes: false,
            target_client: None,
            command: CommandListArgument::String(source.to_owned()),
        };
        let captures_before = parse_time_captures_on_this_thread();
        let plan = handler
            .build_confirm_before_plan(std::process::id(), command, &context)
            .await
            .expect("confirm-before plan builds");
        (
            plan.template,
            plan.format_values,
            parse_time_captures_on_this_thread() - captures_before,
        )
    }

    #[tokio::test]
    async fn command_prompt_captures_only_for_a_template_with_percent() {
        let (template, values, captures) = command_prompt_plan("%1").await;
        assert_eq!(template, "%1");
        assert!(holds_alpha(&values), "{values:?}");
        assert_eq!(captures, 1);

        // A template without % is parsed as written once the prompt completes.
        let (template, values, captures) =
            command_prompt_plan("display-message -p \"#{session_name}\"").await;
        assert_eq!(template, "display-message -p \"#{session_name}\"");
        assert_eq!((values, captures), (Vec::new(), 0));
    }

    #[tokio::test]
    async fn confirm_before_captures_once_and_only_for_a_source_with_percent() {
        // The directive reads the target's values while the plan parses it; the
        // template it yields has no % left, so confirming cannot read them.
        let (template, values, captures) = confirm_before_plan(ALPHA_CONDITIONAL).await;
        assert_eq!(template, "list-sessions");
        assert_eq!((values, captures), (Vec::new(), 1));

        // A template that keeps a % stores the values the plan captured once.
        let (template, values, captures) = confirm_before_plan("display-message '50%'").await;
        assert!(template.contains('%'), "{template:?}");
        assert!(holds_alpha(&values), "{values:?}");
        assert_eq!(captures, 1);

        let (template, values, captures) = confirm_before_plan("display-message ok").await;
        assert_eq!(template, "display-message ok");
        assert_eq!((values, captures), (Vec::new(), 0));
    }
}
