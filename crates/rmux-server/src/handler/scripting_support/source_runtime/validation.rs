use rmux_core::{
    command_parser::{lookup_command, ParsedCommand, ParsedCommands},
    parse_binding_command_tokens_with_parser, ManagedSessionStore,
};
use rmux_proto::{Request, RmuxError, Target};

use super::super::command_args::CommandListArgument;
use super::super::parser_context::command_parser_from_state;
use super::super::queue::{QueueExecutionContext, QueueInvocation};
use super::super::request_parse::parse_queue_invocation;
use super::super::source_files::ParsedSourceFileCommand;
use super::super::targets::{
    command_has_independent_target, queue_target_find_context, QueueTargetFindContextInput,
};
use super::RequestHandler;
use crate::handler::control_support::current_control_queue_identity;
use crate::handler::target_support::{requester_environment_context, RequesterEnvironmentContext};

impl RequestHandler {
    #[async_recursion::async_recursion]
    pub(in crate::handler::scripting_support) async fn validate_sourced_command_syntax(
        &self,
        requester_pid: u32,
        commands: &ParsedCommands,
        context: &QueueExecutionContext,
        recursive: bool,
        load_nested_sources: bool,
        fail_fast: bool,
    ) -> Result<(), RmuxError> {
        let attached_session = self.current_session_candidate(requester_pid).await;
        let socket_path = self.socket_path();
        let mut validation_context = context.clone();
        let mut validation_sessions = self
            .state
            .lock()
            .await
            .sessions
            .is_empty()
            .then(ManagedSessionStore::new);
        let requester_environment = requester_environment_context(requester_pid, &socket_path);

        let mut errors = Vec::new();
        let settings = SourceValidationSettings {
            load_nested_sources,
            fail_fast,
        };
        for command in commands.commands() {
            let result = {
                let state = self.state.lock().await;
                let marked_target = state.marked_pane_target();
                let sessions = validation_sessions.as_ref().unwrap_or(&state.sessions);
                let command_requester_environment = if validation_context.current_target.is_none()
                    && !command_has_independent_target(command, marked_target.as_ref())
                {
                    requester_environment
                } else {
                    RequesterEnvironmentContext::default()
                };
                let find_context = queue_target_find_context(QueueTargetFindContextInput {
                    sessions,
                    options: &state.options,
                    requester_environment: command_requester_environment,
                    attached_session: attached_session.as_ref(),
                    requester_owns_attached_session: current_control_queue_identity(requester_pid)
                        .is_some(),
                    current_target: validation_context.current_target.as_ref(),
                    current_target_follows_client: validation_context
                        .current_target_follows_client(),
                    missing_current_target_fallback: validation_context
                        .missing_current_target_fallback(),
                    mouse_target: validation_context.mouse_target.as_ref(),
                    marked_target: marked_target.as_ref(),
                });
                find_context.and_then(|find_context| {
                    parse_queue_invocation(
                        command.clone(),
                        validation_context.caller_cwd.as_deref(),
                        sessions,
                        &state.options,
                        &find_context,
                        validation_context.canfail_fallback_target(),
                        validation_context.run_shell_canfail_fallback_target(),
                    )
                })
            };
            if let Some(sessions) = validation_sessions.as_mut() {
                advance_source_validation_session_state(&result, sessions, &mut validation_context);
            }

            match result {
                Ok(QueueInvocation::SourceFile(command)) if recursive && load_nested_sources => {
                    self.validate_nested_source_file_syntax(
                        requester_pid,
                        command,
                        &validation_context,
                        &mut errors,
                    )
                    .await;
                }
                Ok(QueueInvocation::IfShell(if_shell)) if recursive => {
                    let nested_context = match if_shell.target.clone() {
                        Some(target) => {
                            validation_context.clone().with_current_target(Some(target))
                        }
                        None => validation_context.clone(),
                    };
                    self.push_command_list_validation_error(
                        requester_pid,
                        &if_shell.then_commands,
                        &nested_context,
                        command,
                        &mut errors,
                        settings,
                    )
                    .await;
                    if let Some(else_commands) = if_shell.else_commands.as_ref() {
                        self.push_command_list_validation_error(
                            requester_pid,
                            else_commands,
                            &nested_context,
                            command,
                            &mut errors,
                            settings,
                        )
                        .await;
                    }
                }
                Ok(QueueInvocation::CommandPrompt(prompt)) if recursive => {
                    if let Some(template) = prompt.template.as_ref() {
                        self.push_command_list_validation_error(
                            requester_pid,
                            template,
                            &validation_context,
                            command,
                            &mut errors,
                            settings,
                        )
                        .await;
                    }
                }
                Ok(QueueInvocation::ConfirmBefore(confirm)) if recursive => {
                    self.push_command_list_validation_error(
                        requester_pid,
                        &confirm.command,
                        &validation_context,
                        command,
                        &mut errors,
                        settings,
                    )
                    .await;
                }
                Ok(
                    QueueInvocation::Request(request) | QueueInvocation::QuietOnlyIfUnset(request),
                ) if recursive => {
                    self.validate_request_embedded_command_syntax(
                        requester_pid,
                        &request,
                        &validation_context,
                        command,
                        &mut errors,
                        settings,
                    )
                    .await;
                }
                Ok(QueueInvocation::RunShell(run_shell)) if recursive => {
                    self.validate_request_embedded_command_syntax(
                        requester_pid,
                        &rmux_proto::Request::RunShell(Box::new(run_shell.request)),
                        &validation_context,
                        command,
                        &mut errors,
                        settings,
                    )
                    .await;
                }
                Ok(_) => {}
                Err(error) if should_defer_source_validation_error(&error, command, recursive) => {}
                Err(error) => {
                    errors.push(super::super::source_file_context_error(
                        error,
                        command,
                        &validation_context,
                    ));
                }
            }
            if fail_fast && !errors.is_empty() {
                return Err(errors.remove(0));
            }
        }

        match super::super::aggregate_rmux_errors(errors) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn validate_nested_source_file_syntax(
        &self,
        requester_pid: u32,
        mut command: ParsedSourceFileCommand,
        context: &QueueExecutionContext,
        errors: &mut Vec<RmuxError>,
    ) {
        if command.parse_only {
            return;
        }
        command.current_file = context.current_file.clone();
        if command.target_missing_canfail {
            command.target = match self.implicit_source_file_target(requester_pid).await {
                Ok(target) => target,
                Err(error) => {
                    errors.push(error);
                    return;
                }
            };
        } else if command.target.is_none() {
            if let Some(Target::Pane(target)) = context.current_target() {
                command.target = Some(target.clone());
            }
        }
        let depth = context.source_file_depth.saturating_add(1);
        let explicit_target = command.target.is_some() || command.target_missing_canfail;
        match self
            .load_nested_source_file_command_with_context(&command, depth, explicit_target, context)
            .await
        {
            Ok(mut loaded) => {
                if let Some(error) = loaded.take_error() {
                    errors.push(error);
                }
                let mut nested_context =
                    context.for_sourced_commands(depth, context.current_file.clone());
                if command.target_missing_canfail {
                    nested_context = nested_context
                        .with_implicit_current_target(command.target.clone().map(Target::Pane));
                } else if let Some(target) = command.target.clone().map(Target::Pane) {
                    nested_context = nested_context.with_current_target(Some(target));
                }
                if let Some(error) = self
                    .validate_loaded_source_file(requester_pid, &loaded, &nested_context, depth)
                    .await
                {
                    errors.push(error);
                }
            }
            Err(error) => errors.push(error),
        }
    }

    async fn push_command_list_validation_error(
        &self,
        requester_pid: u32,
        argument: &CommandListArgument,
        context: &QueueExecutionContext,
        parent_command: &ParsedCommand,
        errors: &mut Vec<RmuxError>,
        settings: SourceValidationSettings,
    ) {
        if let Some(error) = self
            .validate_command_list_argument_syntax(requester_pid, argument, context, settings)
            .await
        {
            errors.push(error.with_parent_context(parent_command, context));
        }
    }

    async fn validate_command_list_argument_syntax(
        &self,
        requester_pid: u32,
        argument: &CommandListArgument,
        context: &QueueExecutionContext,
        settings: SourceValidationSettings,
    ) -> Option<NestedValidationError> {
        let commands = match argument {
            CommandListArgument::Parsed(commands) => commands.clone(),
            CommandListArgument::String(command) => {
                match self.parse_command_string_one_group(command).await {
                    Ok(commands) => commands,
                    Err(error) => return Some(NestedValidationError::needs_parent(error)),
                }
            }
        };
        self.validate_sourced_command_syntax(
            requester_pid,
            &commands,
            context,
            true,
            settings.load_nested_sources,
            settings.fail_fast,
        )
        .await
        .err()
        .map(NestedValidationError::already_contextualized)
    }

    async fn validate_command_string_syntax(
        &self,
        requester_pid: u32,
        command: &str,
        context: &QueueExecutionContext,
        settings: SourceValidationSettings,
    ) -> Option<NestedValidationError> {
        let commands = match self.parse_command_string_one_group(command).await {
            Ok(commands) => commands,
            Err(error) => return Some(NestedValidationError::needs_parent(error)),
        };
        self.validate_sourced_command_syntax(
            requester_pid,
            &commands,
            context,
            true,
            settings.load_nested_sources,
            settings.fail_fast,
        )
        .await
        .err()
        .map(NestedValidationError::already_contextualized)
    }

    async fn validate_binding_command_tokens_syntax(
        &self,
        requester_pid: u32,
        tokens: &[String],
        context: &QueueExecutionContext,
        settings: SourceValidationSettings,
    ) -> Option<NestedValidationError> {
        let parser = {
            let state = self.state.lock().await;
            command_parser_from_state(&state)
        };
        let commands = match parse_binding_command_tokens_with_parser(&parser, tokens) {
            Ok(commands) => commands,
            Err(error) => {
                return Some(NestedValidationError::needs_parent(RmuxError::Server(
                    error.message().to_owned(),
                )));
            }
        };
        self.validate_sourced_command_syntax(
            requester_pid,
            &commands,
            context,
            true,
            settings.load_nested_sources,
            settings.fail_fast,
        )
        .await
        .err()
        .map(NestedValidationError::already_contextualized)
    }

    async fn validate_request_embedded_command_syntax(
        &self,
        requester_pid: u32,
        request: &Request,
        context: &QueueExecutionContext,
        parent_command: &ParsedCommand,
        errors: &mut Vec<RmuxError>,
        settings: SourceValidationSettings,
    ) {
        if let Request::BindKey(request) = request {
            if let Some(tokens) = request.command.as_ref() {
                if let Some(error) = self
                    .validate_binding_command_tokens_syntax(
                        requester_pid,
                        tokens,
                        context,
                        settings,
                    )
                    .await
                {
                    errors.push(error.with_parent_context(parent_command, context));
                }
            }
            return;
        }

        let command = match request {
            Request::SetHook(request) => Some(request.command.clone()),
            Request::SetHookMutation(request) => request.command.clone(),
            _ => None,
        };
        if let Some(command) = command {
            if let Some(error) = self
                .validate_command_string_syntax(requester_pid, &command, context, settings)
                .await
            {
                errors.push(error.with_parent_context(parent_command, context));
            }
        }
    }
}

fn advance_source_validation_session_state(
    result: &Result<QueueInvocation, RmuxError>,
    sessions: &mut ManagedSessionStore,
    context: &mut QueueExecutionContext,
) {
    match result {
        Ok(QueueInvocation::Request(Request::NewSession(request))) => {
            let size = request
                .size
                .unwrap_or(super::super::super::DEFAULT_SESSION_SIZE);
            let _ = sessions.create_session(request.session_name.clone(), size);
        }
        Ok(QueueInvocation::Request(Request::NewSessionExt(request)))
            if request.group_target.is_none() =>
        {
            let Some(session_name) = request.session_name.clone() else {
                return;
            };
            let size = request
                .size
                .unwrap_or(super::super::super::DEFAULT_SESSION_SIZE);
            let _ = sessions.create_session(session_name, size);
        }
        Ok(QueueInvocation::Request(Request::RenameSession(request)))
            if sessions
                .rename_session(&request.target, request.new_name.clone())
                .is_ok() =>
        {
            context.rebase_for_source_validation_session_rename(&request.target, &request.new_name);
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
struct SourceValidationSettings {
    load_nested_sources: bool,
    fail_fast: bool,
}

struct NestedValidationError {
    error: RmuxError,
    needs_parent_context: bool,
}

impl NestedValidationError {
    fn needs_parent(error: RmuxError) -> Self {
        Self {
            error,
            needs_parent_context: true,
        }
    }

    fn already_contextualized(error: RmuxError) -> Self {
        Self {
            error,
            needs_parent_context: false,
        }
    }

    fn with_parent_context(
        self,
        parent_command: &ParsedCommand,
        context: &QueueExecutionContext,
    ) -> RmuxError {
        if self.needs_parent_context {
            super::super::source_file_context_error(self.error, parent_command, context)
        } else {
            self.error
        }
    }
}

fn should_defer_source_validation_error(
    error: &RmuxError,
    command: &ParsedCommand,
    recursive: bool,
) -> bool {
    let is_direct_display_message_error = !recursive
        && matches!(error, RmuxError::Message(_))
        && lookup_command(command.name()).is_ok_and(|entry| entry.name == "display-message");
    match error {
        RmuxError::InvalidTarget { .. } | RmuxError::SessionNotFound(_) => true,
        RmuxError::Message(message) => {
            is_direct_display_message_error || is_runtime_target_or_client_lookup_error(message)
        }
        RmuxError::Server(message) => {
            is_runtime_target_or_client_lookup_error(message)
                || (!recursive && is_source_runtime_option_lookup_error(message))
        }
        _ => false,
    }
}

fn is_runtime_target_or_client_lookup_error(message: &str) -> bool {
    message.contains("can't find session")
        || message.contains("can't find window")
        || message.contains("can't find pane")
        || message.contains("can't find client")
        || message.contains("can't find target")
        || message.contains("ambiguous target")
        || message.contains("no current client")
        || message.contains("no current target")
        || message.contains("no current session")
}

fn is_source_runtime_option_lookup_error(message: &str) -> bool {
    message.starts_with("unknown option: ")
        || message.starts_with("invalid option: ")
        || message.starts_with("ambiguous option: ")
}
