use std::path::Path;
use std::time::Duration;

use rmux_core::{
    command_parser::{
        CommandParseErrorKind, CommandParser, ParsedCommand, ParsedCommands,
        SOURCE_FILE_MAX_COMMAND_BYTES,
    },
    parse_binding_command_tokens,
};
use rmux_proto::{
    CommandOutput, ErrorResponse, PaneTarget, Request, Response, RmuxError, SourceFileRequest,
    SourceFileResponse, Target,
};

use super::super::target_support::{
    pane_id_target, requester_environment_context, requester_environment_pane_id,
};
use super::super::{ConfigLoadingGuard, RequestHandler};
use super::command_args::CommandListArgument;
use super::config_engine::{
    append_error_output, config_error_lines, nonempty_stdout, ConfigLoadOrigin, ConfigLoadRequest,
};
use super::format_context::{
    format_context_for_target_with_server_values, global_format_context,
    parser_with_parse_time_context,
};
use super::parser_context::command_parser_from_state;
use super::queue::{QueueCommandAction, QueueExecutionContext, QueueInvocation, QueueMode};
use super::request_parse::parse_queue_invocation;
use super::source_files::{
    default_config_paths, default_tmux_fallback_paths, source_inputs_for_path,
    source_parse_error_with_line_offset, LoadedSourceFile, ParsedSourceFileCommand, SourceInput,
    SourceSyntax, SourcedParsedCommands,
};
use super::targets::{
    active_session_target, queue_target_find_context, QueueTargetFindContextInput,
};
use super::tmux_compat::tmux_compat_input;
use crate::format_runtime::render_runtime_template;
use crate::{ConfigFileSelection, ConfigLoadOptions};

const SOURCE_PARSE_RECOVERY_ERROR_LIMIT: usize = 256;
const STARTUP_CONFIG_ERROR_LIMIT: usize = 256;
const CONFIG_MESSAGE_MAX_BYTES: usize = 512;
#[cfg(not(test))]
const STARTUP_READINESS_BUDGET: Duration = Duration::from_secs(2);
#[cfg(test)]
const STARTUP_READINESS_BUDGET: Duration = Duration::from_millis(100);

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn load_startup_config(&self, config_load: ConfigLoadOptions) {
        let guard = self.start_config_loading();
        self.load_startup_config_with_guard(config_load, guard)
            .await;
    }

    pub(crate) async fn load_startup_config_with_guard(
        &self,
        config_load: ConfigLoadOptions,
        guard: ConfigLoadingGuard,
    ) {
        let (paths, tmux_fallback_paths) = match config_load.selection() {
            ConfigFileSelection::Disabled => return,
            ConfigFileSelection::Default => (default_config_paths(), default_tmux_fallback_paths()),
            ConfigFileSelection::Files(files) => (
                files
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                Vec::new(),
            ),
        };

        let command = ParsedSourceFileCommand {
            paths,
            quiet: config_load.quiet(),
            parse_only: false,
            verbose: false,
            expand_paths: false,
            target: None,
            caller_cwd: config_load.cwd().map(Path::to_path_buf),
            stdin: None,
            current_file: None,
            syntax: SourceSyntax::Rmux,
        };

        let loaded = match self.load_startup_source_file_command(&command, 1).await {
            Ok(loaded) => loaded,
            Err(error) => {
                self.record_config_error(error).await;
                return;
            }
        };

        let should_load_tmux_fallback =
            !loaded.loaded_any_file() && !loaded.has_errors() && !tmux_fallback_paths.is_empty();
        let mut loaded = if should_load_tmux_fallback {
            let fallback_command = ParsedSourceFileCommand {
                paths: tmux_fallback_paths,
                quiet: true,
                syntax: SourceSyntax::TmuxCompat,
                ..command.clone()
            };
            match self
                .load_startup_source_file_command(&fallback_command, 1)
                .await
            {
                Ok(loaded) => loaded,
                // INTENTIONAL: the tmux fallback is a compatibility import for
                // users who do not have an RMUX config. If it cannot even be
                // loaded, startup must continue like tmux's absent/optional
                // fallback paths rather than failing daemon readiness.
                Err(_) => return,
            }
        } else {
            loaded
        };

        let mut errors = Vec::new();
        if let Some(error) = loaded.take_error() {
            errors.push(error);
        }
        let execution = self
            .execute_startup_source_file_with_readiness_budget(
                loaded,
                QueueExecutionContext::new(command.caller_cwd.clone()),
                guard,
            )
            .await;
        if let Some(error) = execution.error {
            errors.push(error);
        }
        if let Some(error) = super::aggregate_rmux_errors(errors) {
            self.record_config_error(error).await;
        }
    }

    async fn execute_startup_source_file_with_readiness_budget(
        &self,
        loaded: LoadedSourceFile,
        context: QueueExecutionContext,
        guard: ConfigLoadingGuard,
    ) -> SourceFileExecution {
        let execution = self.execute_loaded_source_file(std::process::id(), loaded, context, 1);
        tokio::pin!(execution);
        let mut guard = Some(guard);
        let result = tokio::select! {
            biased;
            result = &mut execution => result,
            _ = tokio::time::sleep(STARTUP_READINESS_BUDGET) => {
                drop(guard.take());
                execution.await
            }
        };
        drop(guard);
        result
    }

    pub(in crate::handler) async fn handle_source_file(
        &self,
        requester_pid: u32,
        request: SourceFileRequest,
    ) -> Response {
        let mut command = ParsedSourceFileCommand::from(request);
        let explicit_target = command.target.is_some();
        let socket_path = self.socket_path();
        let requester_environment = requester_environment_context(requester_pid, &socket_path);
        if command.target.is_none() {
            command.target = self
                .implicit_source_file_target_with_pane_id(
                    requester_pid,
                    requester_environment.pane_id,
                )
                .await;
        }
        let depth = requester_environment
            .source_depth
            .unwrap_or(0)
            .saturating_add(1);
        let invoked_from_sourced_shell = requester_environment.source_depth.is_some();
        let mut loaded = match self
            .load_explicit_source_file_command(&command, depth, explicit_target)
            .await
        {
            Ok(loaded) => loaded,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let strict_errors = command.syntax == SourceSyntax::Rmux;
        let mut errors = Vec::new();
        let load_error = loaded.take_error();

        let context = QueueExecutionContext::new(command.caller_cwd.clone());
        let context = match command.target.clone().map(Target::Pane) {
            Some(target) if explicit_target => context.with_current_target(Some(target)),
            Some(target) => context.with_implicit_current_target(Some(target)),
            None => context,
        };
        let mut stdout = std::mem::take(&mut loaded.stdout);
        let mut exit_status = None;
        if command.parse_only {
            let validation_error = self
                .validate_loaded_source_file(requester_pid, &loaded, &context, depth)
                .await;
            let error = validation_error.or(load_error);
            if command.verbose {
                let stop = error
                    .as_ref()
                    .and_then(|error| source_error_location_for_loaded(error, &loaded));
                append_verbose_loaded_commands(&mut stdout, &loaded, stop.as_ref());
            }
            if let Some(error) = error {
                errors.push(error);
            }
        } else {
            if let Some(error) = load_error {
                errors.push(error);
            }
            let SourceFileExecution {
                output,
                error,
                exit_status: execution_exit_status,
            } = self
                .execute_loaded_source_file(requester_pid, loaded, context, depth)
                .await;
            stdout.extend_from_slice(output.stdout());
            if let Some(status) = execution_exit_status {
                exit_status = Some(status);
            }
            if let Some(error) = error {
                errors.push(error);
            }
        }

        if let Some(error) = super::aggregate_rmux_errors(errors) {
            self.log_source_file_error_messages(&error, invoked_from_sourced_shell)
                .await;
            if strict_errors {
                if !stdout.is_empty() {
                    append_error_output(&mut stdout, &error);
                    return Response::SourceFile(
                        SourceFileResponse::from_output(CommandOutput::from_stdout(stdout))
                            .with_exit_status(Some(1)),
                    );
                }
                return Response::Error(ErrorResponse { error });
            }
            append_error_output(&mut stdout, &error);
        }

        let response = if stdout.is_empty() {
            SourceFileResponse::no_output()
        } else {
            SourceFileResponse::from_output(CommandOutput::from_stdout(stdout))
        };
        Response::SourceFile(response.with_exit_status(nonzero_exit_status(exit_status)))
    }

    pub(super) async fn implicit_source_file_target(
        &self,
        requester_pid: u32,
    ) -> Option<PaneTarget> {
        let socket_path = self.socket_path();
        let requester_pane_id = requester_environment_pane_id(requester_pid, &socket_path);
        self.implicit_source_file_target_with_pane_id(requester_pid, requester_pane_id)
            .await
    }

    async fn implicit_source_file_target_with_pane_id(
        &self,
        requester_pid: u32,
        requester_pane_id: Option<u32>,
    ) -> Option<PaneTarget> {
        let attached_session = self.current_session_candidate(requester_pid).await;
        let preferred_session = self.preferred_session_name().await.ok();
        let state = self.state.lock().await;
        attached_session
            .as_ref()
            .and_then(|session_name| active_session_target(&state.sessions, session_name))
            .or_else(|| {
                requester_pane_id.and_then(|pane_id| pane_id_target(&state.sessions, pane_id))
            })
            .or_else(|| {
                preferred_session
                    .as_ref()
                    .and_then(|session_name| active_session_target(&state.sessions, session_name))
            })
            .and_then(|target| match target {
                Target::Pane(target) => Some(target),
                _ => None,
            })
    }

    pub(super) async fn execute_queued_source_file(
        &self,
        requester_pid: u32,
        mut command: ParsedSourceFileCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let depth = context.source_file_depth.saturating_add(1);
        command.current_file = context.current_file.clone();
        let explicit_target = command.target.is_some();
        if command.target.is_none() {
            if let Some(Target::Pane(target)) = context.current_target() {
                command.target = Some(target.clone());
            }
        }
        let sourced_target = command.target.clone().map(Target::Pane);
        let mut loaded = self
            .load_nested_source_file_command(&command, depth, explicit_target)
            .await?;
        let mut errors = Vec::new();
        let load_error = loaded.take_error();

        let mut batches = Vec::new();
        for batch in loaded.commands {
            let mut batch_context = context.for_sourced_commands(depth, batch.current_file);
            if explicit_target {
                if let Some(target) = sourced_target.clone() {
                    batch_context = batch_context.with_current_target(Some(target));
                }
            }
            match self
                .validate_sourced_command_syntax(
                    requester_pid,
                    &batch.commands,
                    &batch_context,
                    command.parse_only,
                    !command.parse_only,
                    command.parse_only,
                )
                .await
            {
                Ok(()) => batches.push((batch.commands, batch_context)),
                Err(error) => errors.push(error),
            }
            if command.parse_only && !errors.is_empty() {
                break;
            }
        }
        if let Some(error) = load_error {
            if !command.parse_only || errors.is_empty() {
                errors.push(error);
            }
        }
        let error = super::aggregate_rmux_errors(errors);

        if command.parse_only || batches.is_empty() {
            return Ok(QueueCommandAction::Normal {
                output: nonempty_stdout(loaded.stdout),
                error,
                exit_status: None,
            });
        }

        Ok(QueueCommandAction::InsertAfter {
            batches,
            output: nonempty_stdout(loaded.stdout),
            error,
            exit_status: None,
        })
    }

    async fn execute_loaded_source_file(
        &self,
        requester_pid: u32,
        loaded: LoadedSourceFile,
        mut context: QueueExecutionContext,
        depth: usize,
    ) -> SourceFileExecution {
        let mut stdout = Vec::new();
        let mut errors = Vec::new();
        let mut exit_status = None;
        for batch in loaded.commands {
            let batch_context = context.for_sourced_commands(depth, batch.current_file);
            if let Err(error) = self
                .validate_sourced_command_syntax(
                    requester_pid,
                    &batch.commands,
                    &batch_context,
                    false,
                    true,
                    false,
                )
                .await
            {
                errors.push(error);
                continue;
            }
            let result = self
                .execute_command_queue(
                    requester_pid,
                    batch.commands,
                    batch_context,
                    QueueMode::Detached,
                )
                .await;
            stdout.extend_from_slice(&result.stdout);
            if let Some(status) = result.exit_status {
                exit_status = Some(status);
            }
            if let Some(error) = result.error {
                errors.push(error);
            }
            if !context.uses_explicit_current_target() {
                context = context.with_implicit_current_target(
                    self.implicit_source_file_target(requester_pid)
                        .await
                        .map(Target::Pane),
                );
            }
        }

        let error = super::aggregate_rmux_errors(errors);
        SourceFileExecution {
            output: CommandOutput::from_stdout(stdout),
            error,
            exit_status,
        }
    }

    async fn validate_loaded_source_file(
        &self,
        requester_pid: u32,
        loaded: &LoadedSourceFile,
        context: &QueueExecutionContext,
        depth: usize,
    ) -> Option<RmuxError> {
        for batch in &loaded.commands {
            let batch_context = context.for_sourced_commands(depth, batch.current_file.clone());
            if let Err(error) = self
                .validate_sourced_command_syntax(
                    requester_pid,
                    &batch.commands,
                    &batch_context,
                    true,
                    false,
                    true,
                )
                .await
            {
                return Some(error);
            }
        }
        None
    }

    async fn load_startup_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
    ) -> Result<LoadedSourceFile, RmuxError> {
        self.load_source_file_command(command, depth, ConfigLoadOrigin::Startup, false, true)
            .await
    }

    async fn load_explicit_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
        explicit_target: bool,
    ) -> Result<LoadedSourceFile, RmuxError> {
        self.load_source_file_command(
            command,
            depth,
            ConfigLoadOrigin::ExplicitSourceFile,
            explicit_target,
            !explicit_target,
        )
        .await
    }

    async fn load_nested_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
        explicit_target: bool,
    ) -> Result<LoadedSourceFile, RmuxError> {
        self.load_source_file_command(
            command,
            depth,
            ConfigLoadOrigin::NestedSourceFile,
            explicit_target,
            !explicit_target,
        )
        .await
    }

    async fn load_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
        origin: ConfigLoadOrigin,
        explicit_target: bool,
        implicit_target_refresh: bool,
    ) -> Result<LoadedSourceFile, RmuxError> {
        let request = ConfigLoadRequest::from_source_command(
            command,
            origin,
            explicit_target,
            implicit_target_refresh,
            depth,
        );
        super::config_engine::load(self, request).await
    }

    pub(super) async fn load_source_file_command_inner(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
    ) -> Result<LoadedSourceFile, RmuxError> {
        if depth > super::SOURCE_FILE_NESTING_LIMIT {
            return Err(RmuxError::Server("too many nested files".to_owned()));
        }

        let mut loaded = LoadedSourceFile::default();

        for path in &command.paths {
            let expanded_path = if command.expand_paths {
                self.render_source_file_path(
                    path,
                    command.target.as_ref(),
                    command.current_file.as_deref(),
                )
                .await?
            } else {
                path.clone()
            };
            let inputs = match source_inputs_for_path(
                &expanded_path,
                command.caller_cwd.as_deref(),
                command.quiet,
                command.stdin.as_deref(),
                command.read_policy(),
            ) {
                Ok(inputs) => inputs,
                Err(error) => {
                    loaded.push_error(error);
                    continue;
                }
            };
            if !inputs.is_empty() {
                loaded.record_loaded_files(inputs.len());
            }
            for input in inputs {
                let input = match command.syntax {
                    SourceSyntax::Rmux => input,
                    SourceSyntax::TmuxCompat => tmux_compat_input(&input),
                };
                if input.contents.trim().is_empty() {
                    continue;
                }
                let parsed = match self
                    .parse_source_input(&input, command.target.as_ref(), command.parse_only)
                    .await
                {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        loaded.push_parse_error(error);
                        continue;
                    }
                };
                for error in parsed.errors {
                    loaded.push_parse_error(error);
                }
                if parsed.has_fatal_parse_error {
                    continue;
                }
                for commands in parsed.commands {
                    if command.verbose && !command.parse_only {
                        append_verbose_commands(&mut loaded.stdout, &input.current_file, &commands);
                    }
                    loaded.commands.push(SourcedParsedCommands {
                        commands,
                        current_file: Some(input.current_file.clone()),
                    });
                }
            }
        }

        Ok(loaded)
    }

    async fn record_config_error(&self, error: RmuxError) {
        self.log_config_error_messages(&error).await;
        let mut errors = self.startup_config_errors.lock().await;
        if errors.len() >= STARTUP_CONFIG_ERROR_LIMIT {
            errors.remove(0);
        }
        errors.push(error);
    }

    async fn log_config_error_messages(&self, error: &RmuxError) {
        let lines = config_error_lines(error);
        if lines.is_empty() {
            return;
        }
        let mut state = self.state.lock().await;
        for line in lines {
            state.add_message(truncate_config_message(format!("config error: {line}")));
        }
    }

    async fn log_source_file_error_messages(
        &self,
        error: &RmuxError,
        invoked_from_sourced_shell: bool,
    ) {
        if !invoked_from_sourced_shell {
            self.log_config_error_messages(error).await;
            return;
        }

        let lines = config_error_lines(error);
        if lines.is_empty() {
            return;
        }
        let mut state = self.state.lock().await;
        for line in lines {
            state.add_message(truncate_config_message(line));
        }
    }

    async fn render_source_file_path(
        &self,
        path: &str,
        target: Option<&PaneTarget>,
        current_file: Option<&str>,
    ) -> Result<String, RmuxError> {
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let mut context = match target {
            Some(target) => format_context_for_target_with_server_values(
                &state,
                &Target::Pane(target.clone()),
                attached_count,
                &socket_path,
            )?,
            None => global_format_context(&state, &socket_path),
        };

        if let Some(current_file) = current_file {
            context = context.with_named_value("current_file", current_file);
        }
        Ok(render_runtime_template(path, &context, false))
    }

    async fn parse_source_input(
        &self,
        input: &SourceInput,
        target: Option<&PaneTarget>,
        stop_at_first_error: bool,
    ) -> Result<ParsedSourceInput, RmuxError> {
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let mut parser =
            command_parser_from_state(&state).with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
        let context = match target {
            Some(target) => format_context_for_target_with_server_values(
                &state,
                &Target::Pane(target.clone()),
                attached_count,
                &socket_path,
            )?
            .with_named_value("current_file", &input.current_file),
            None => global_format_context(&state, &socket_path)
                .with_named_value("current_file", &input.current_file),
        };
        parser = parser_with_parse_time_context(parser, &context);
        let mut parsed = ParsedSourceInput::default();
        if stop_at_first_error {
            parse_source_fragment_until_first_error(
                &parser,
                input,
                &input.contents,
                0,
                &mut parsed,
            );
        } else {
            parse_source_fragment_recovering(&parser, input, &input.contents, 0, &mut parsed);
        }
        Ok(parsed)
    }

    #[async_recursion::async_recursion]
    async fn validate_sourced_command_syntax(
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
        let requester_pane_id = context
            .current_target
            .is_none()
            .then(|| requester_environment_pane_id(requester_pid, &socket_path))
            .flatten();

        let mut errors = Vec::new();
        let settings = SourceValidationSettings {
            load_nested_sources,
            fail_fast,
        };
        for command in commands.commands() {
            let result = {
                let state = self.state.lock().await;
                let marked_target = state.marked_pane_target();
                let find_context = queue_target_find_context(QueueTargetFindContextInput {
                    sessions: &state.sessions,
                    options: &state.options,
                    requester_pane_id,
                    attached_session: attached_session.as_ref(),
                    current_target: context.current_target.as_ref(),
                    mouse_target: context.mouse_target.as_ref(),
                    marked_target: marked_target.as_ref(),
                });
                parse_queue_invocation(
                    command.clone(),
                    context.caller_cwd.as_deref(),
                    &state.sessions,
                    &state.options,
                    &find_context,
                    context.canfail_fallback_target(),
                )
            };

            match result {
                Ok(QueueInvocation::SourceFile(command)) if recursive && load_nested_sources => {
                    self.validate_nested_source_file_syntax(
                        requester_pid,
                        command,
                        context,
                        &mut errors,
                    )
                    .await;
                }
                Ok(QueueInvocation::IfShell(if_shell)) if recursive => {
                    let nested_context = match if_shell.target.clone() {
                        Some(target) => context.clone().with_current_target(Some(target)),
                        None => context.clone(),
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
                            context,
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
                        context,
                        command,
                        &mut errors,
                        settings,
                    )
                    .await;
                }
                Ok(QueueInvocation::Request(request)) if recursive => {
                    self.validate_request_embedded_command_syntax(
                        requester_pid,
                        &request,
                        context,
                        command,
                        &mut errors,
                        settings,
                    )
                    .await;
                }
                Ok(_) => {}
                Err(error) if should_defer_source_validation_error(&error, recursive) => {}
                Err(error) => {
                    errors.push(super::source_file_context_error(error, command, context));
                }
            }
            if fail_fast && !errors.is_empty() {
                return Err(errors.remove(0));
            }
        }

        match super::aggregate_rmux_errors(errors) {
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
        if command.target.is_none() {
            if let Some(Target::Pane(target)) = context.current_target() {
                command.target = Some(target.clone());
            }
        }
        let depth = context.source_file_depth.saturating_add(1);
        match self
            .load_nested_source_file_command(&command, depth, command.target.is_some())
            .await
        {
            Ok(mut loaded) => {
                if let Some(error) = loaded.take_error() {
                    errors.push(error);
                }
                let nested_context =
                    context.for_sourced_commands(depth, context.current_file.clone());
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
        let commands = match parse_binding_command_tokens(tokens) {
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
            super::source_file_context_error(self.error, parent_command, context)
        } else {
            self.error
        }
    }
}

fn should_defer_source_validation_error(error: &RmuxError, recursive: bool) -> bool {
    match error {
        RmuxError::InvalidTarget { .. } | RmuxError::SessionNotFound(_) => true,
        RmuxError::Server(message) | RmuxError::Message(message) => {
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

#[derive(Default)]
struct ParsedSourceInput {
    commands: Vec<ParsedCommands>,
    errors: Vec<RmuxError>,
    has_fatal_parse_error: bool,
}

struct SourceFileExecution {
    output: CommandOutput,
    error: Option<RmuxError>,
    exit_status: Option<i32>,
}

fn nonzero_exit_status(exit_status: Option<i32>) -> Option<i32> {
    exit_status.filter(|status| *status != 0)
}

fn parse_source_fragment_recovering(
    parser: &CommandParser,
    input: &SourceInput,
    contents: &str,
    line_offset: usize,
    parsed: &mut ParsedSourceInput,
) {
    let mut fragments = vec![(contents, line_offset)];
    while let Some((fragment, fragment_line_offset)) = fragments.pop() {
        if fragment.trim().is_empty() {
            continue;
        }
        if parsed.errors.len() >= SOURCE_PARSE_RECOVERY_ERROR_LIMIT {
            parsed.errors.push(RmuxError::Server(format!(
                "{}: too many config parse errors; stopped recovery after {SOURCE_PARSE_RECOVERY_ERROR_LIMIT} errors",
                input.current_file
            )));
            break;
        }

        match parser.parse_source_file(fragment) {
            Ok(mut commands) => {
                if !commands.is_empty() {
                    commands.add_line_offset(fragment_line_offset);
                    parsed.commands.push(commands);
                }
            }
            Err(error) => {
                let error_line = error.line();
                let recoverable = error.kind() == CommandParseErrorKind::Lookup;
                parsed.errors.push(source_parse_error_with_line_offset(
                    input,
                    error,
                    fragment_line_offset,
                ));
                if !recoverable {
                    parsed.has_fatal_parse_error = true;
                    continue;
                }
                let Some((prefix, suffix, suffix_line)) =
                    split_around_source_command(parser, fragment, error_line)
                        .or_else(|| split_around_source_line(fragment, error_line))
                else {
                    continue;
                };
                if !suffix.trim().is_empty() && suffix.len() < fragment.len() {
                    fragments.push((
                        suffix,
                        fragment_line_offset.saturating_add(suffix_line.saturating_sub(1)),
                    ));
                }
                if !prefix.trim().is_empty() && prefix.len() < fragment.len() {
                    fragments.push((prefix, fragment_line_offset));
                }
            }
        }
    }
}

fn parse_source_fragment_until_first_error(
    parser: &CommandParser,
    input: &SourceInput,
    contents: &str,
    line_offset: usize,
    parsed: &mut ParsedSourceInput,
) {
    if contents.trim().is_empty() || !parsed.errors.is_empty() {
        return;
    }

    match parser.parse_source_file(contents) {
        Ok(mut commands) => {
            if !commands.is_empty() {
                commands.add_line_offset(line_offset);
                parsed.commands.push(commands);
            }
        }
        Err(error) => {
            let error_line = error.line();
            let recoverable = error.kind() == CommandParseErrorKind::Lookup;
            let contextual_error = source_parse_error_with_line_offset(input, error, line_offset);
            if recoverable {
                if let Some((prefix, _, _)) =
                    split_around_source_command(parser, contents, error_line)
                        .or_else(|| split_around_source_line(contents, error_line))
                {
                    if !prefix.trim().is_empty() && prefix.len() < contents.len() {
                        parse_source_fragment_until_first_error(
                            parser,
                            input,
                            prefix,
                            line_offset,
                            parsed,
                        );
                        if !parsed.errors.is_empty() {
                            return;
                        }
                    }
                }
            }
            parsed.errors.push(contextual_error);
        }
    }
}

fn split_around_source_command<'a>(
    parser: &CommandParser,
    contents: &'a str,
    line: usize,
) -> Option<(&'a str, &'a str, usize)> {
    if line == 0 {
        return None;
    }
    let structure = parser.parse_source_file_structure(contents).ok()?;
    let commands = structure.commands();
    let (index, command) = commands.iter().enumerate().find(|(index, command)| {
        let start_line = command.line();
        let next_line = commands
            .get(index.saturating_add(1))
            .map(ParsedCommand::line)
            .unwrap_or_else(|| {
                contents
                    .lines()
                    .count()
                    .saturating_add(1)
                    .max(start_line + 1)
            });
        start_line <= line && line < next_line
    })?;
    let start = line_start_byte(contents, command.line())?;
    let end_line = commands
        .get(index.saturating_add(1))
        .map(ParsedCommand::line)
        .unwrap_or_else(|| contents.lines().count().saturating_add(1));
    let end = line_start_byte(contents, end_line).unwrap_or(contents.len());
    if start >= end || start > contents.len() || end > contents.len() {
        return None;
    }
    Some((&contents[..start], &contents[end..], end_line))
}

fn split_around_source_line(contents: &str, line: usize) -> Option<(&str, &str, usize)> {
    if line == 0 {
        return None;
    }
    let last_line = contents.lines().count().max(1);
    let mut start =
        line_start_byte(contents, line).or_else(|| line_start_byte(contents, last_line))?;
    if start == contents.len() && line > 1 {
        start = line_start_byte(contents, last_line)?;
    }
    let next = line_start_byte(contents, line.saturating_add(1)).unwrap_or(contents.len());
    if start == contents.len() && next == contents.len() {
        return None;
    }
    Some((
        &contents[..start],
        &contents[next..],
        line.saturating_add(1),
    ))
}

fn line_start_byte(contents: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }

    let mut current_line = 1usize;
    for (index, byte) in contents.bytes().enumerate() {
        if byte == b'\n' {
            current_line += 1;
            if current_line == line {
                return Some(index + 1);
            }
        }
    }
    (current_line == line).then_some(contents.len())
}

fn truncate_config_message(message: String) -> String {
    if message.len() <= CONFIG_MESSAGE_MAX_BYTES {
        return message;
    }
    let mut end = CONFIG_MESSAGE_MAX_BYTES;
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &message[..end])
}

fn append_verbose_commands(stdout: &mut Vec<u8>, current_file: &str, parsed: &ParsedCommands) {
    if parsed.is_empty() {
        return;
    }
    for command in parsed.commands() {
        append_verbose_command(stdout, current_file, command);
    }
}

fn append_verbose_loaded_commands(
    stdout: &mut Vec<u8>,
    loaded: &LoadedSourceFile,
    stop: Option<&SourceErrorLocation>,
) {
    for batch in &loaded.commands {
        let current_file = batch.current_file.as_deref().unwrap_or_default();
        for command in batch.commands.commands() {
            if stop.is_some_and(|stop| {
                stop.current_file == current_file && command.line() >= stop.line
            }) {
                return;
            }
            append_verbose_command(stdout, current_file, command);
        }
    }
}

fn append_verbose_command(stdout: &mut Vec<u8>, current_file: &str, command: &ParsedCommand) {
    stdout.extend_from_slice(current_file.as_bytes());
    stdout.push(b':');
    stdout.extend_from_slice(command.line().to_string().as_bytes());
    stdout.extend_from_slice(b": ");
    stdout.extend_from_slice(command.to_tmux_string().as_bytes());
    stdout.push(b'\n');
}

struct SourceErrorLocation {
    current_file: String,
    line: usize,
}

fn source_error_location_for_loaded(
    error: &RmuxError,
    loaded: &LoadedSourceFile,
) -> Option<SourceErrorLocation> {
    let message = error.to_string();
    for line in message.lines() {
        for batch in &loaded.commands {
            let Some(current_file) = batch.current_file.as_deref() else {
                continue;
            };
            let prefix = format!("{current_file}:");
            let Some(start) = line.find(&prefix) else {
                continue;
            };
            let rest = &line[start + prefix.len()..];
            let Some((line_number, _)) = rest.split_once(':') else {
                continue;
            };
            let Ok(line) = line_number.parse::<usize>() else {
                continue;
            };
            return Some(SourceErrorLocation {
                current_file: current_file.to_owned(),
                line,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::super::super::RequestHandler;
    use crate::test_env::EnvVarGuard;
    use crate::DaemonConfig;
    use rmux_proto::OptionName;

    #[tokio::test]
    async fn config_loading_guard_marks_handler_busy_until_dropped() {
        let handler = RequestHandler::new();

        assert!(
            !handler.config_loading_active(),
            "fresh handler should not be loading config"
        );
        let guard = handler.start_config_loading();
        assert!(
            handler.config_loading_active(),
            "guard should mark startup config loading before async work starts"
        );
        drop(guard);
        assert!(
            !handler.config_loading_active(),
            "dropping guard should clear startup config loading"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_readiness_clears_before_blocking_run_shell_finishes() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("slow-run-shell-readiness");
        let config_path = root.join("slow.conf");
        write_test_config(config_path.clone(), "run-shell 'sleep 3'\n");
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![config_path],
            false,
            Some(root.clone()),
        );

        let guard = handler.start_config_loading();
        assert!(handler.config_loading_active());

        let load_handler = handler.clone();
        let load_config = config.config_load().clone();
        let task = tokio::spawn(async move {
            load_handler
                .load_startup_config_with_guard(load_config, guard)
                .await;
        });

        let readiness = async {
            while handler.config_loading_active() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(1), readiness)
            .await
            .expect("readiness should clear before the blocking run-shell finishes");

        assert!(
            !task.is_finished(),
            "startup config should still be executing after readiness clears"
        );

        task.abort();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_is_not_used_after_rmux_config_load_error() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-load-error");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        write_test_config(rmux_user_config_path(&home), "definitely-not-a-command\n");
        write_test_config(tmux_user_config_path(&home), "set -g status off\n");

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let errors = handler.startup_config_errors.lock().await;
        let rendered = errors
            .first()
            .expect("rmux config load error should be retained")
            .to_string();
        assert!(
            rendered.contains(".rmux.conf"),
            "expected rmux config load error, got {rendered}"
        );
        assert!(
            rendered.contains("unknown command: definitely-not-a-command"),
            "expected rmux config parse error, got {rendered}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn explicit_startup_config_lookup_errors_continue_after_bad_command() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("explicit-load-error");
        let config_path = root.join("bad.conf");
        write_test_config(
            config_path.clone(),
            "definitely-not-a-command\nset -g status off\n",
        );
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![config_path],
            false,
            Some(root.clone()),
        );

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let errors = handler.startup_config_errors.lock().await;
        let rendered = errors
            .first()
            .expect("explicit config load error should be retained")
            .to_string();
        assert!(
            rendered.contains("definitely-not-a-command"),
            "expected explicit config error, got {rendered}"
        );
        drop(errors);
        let state = handler.state.lock().await;
        assert_eq!(
            state.options.global_value(OptionName::Status),
            Some("off"),
            "startup config must retain lookup errors and continue with later commands"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_dev_null_startup_config_is_silent() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("explicit-dev-null");
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![PathBuf::from("/dev/null")],
            false,
            Some(root.clone()),
        );

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "/dev/null startup config must be a silent no-op"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn startup_config_skips_file_with_eof_parse_error() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("eof-parse-recovery");
        let config_path = root.join("bad-eof.conf");
        write_test_config(
            config_path.clone(),
            "set -g status off\nif-shell -F '1' {\n",
        );
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![config_path],
            false,
            Some(root.clone()),
        );

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let errors = handler.startup_config_errors.lock().await;
        assert!(
            errors
                .first()
                .is_some_and(|error| error.to_string().contains("bad-eof.conf")),
            "startup config EOF parse error should be retained, got {errors:?}"
        );
        drop(errors);
        let state = handler.state.lock().await;
        assert_eq!(
            state.options.global_value(OptionName::Status),
            None,
            "startup config must retain the error but skip the bad file"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn startup_config_runtime_errors_are_retained() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("runtime-load-error");
        let config_path = root.join("bad-runtime.conf");
        write_test_config(
            config_path.clone(),
            "set -g status off\nsource-file /definitely/missing.conf\n",
        );
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![config_path],
            false,
            Some(root.clone()),
        );

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let errors = handler.startup_config_errors.lock().await;
        let rendered = errors
            .first()
            .expect("runtime config execution error should be retained")
            .to_string();
        assert!(
            rendered.contains("definitely/missing.conf"),
            "expected missing nested source-file error, got {rendered}"
        );
        drop(errors);
        let state = handler.state.lock().await;
        assert_eq!(
            state.options.global_value(OptionName::Status),
            Some("off"),
            "startup config must keep earlier valid commands despite runtime errors"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tmux_fallback_executes_runtime_config_when_no_rmux_config_exists() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-runtime-config");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        let marker = root.join("fallback-run-shell.txt");
        write_test_config(
            tmux_user_config_path(&home),
            &format!(
                "unbind-key -a\n\
             if-shell 'test -f ~/.enable-rmux' {{\n\
             set -g status on\n\
             }}\n\
             set -g status off\n\
             run-shell 'touch {}'\n",
                shell_quote_path(&marker)
            ),
        );

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "tmux fallback import should be best-effort and error-free"
        );
        let state = handler.state.lock().await;
        assert_eq!(state.options.global_value(OptionName::Status), Some("off"));
        drop(state);
        assert!(marker.is_file(), "tmux fallback must execute run-shell");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tmux_fallback_loads_symlinked_legacy_config_without_discarding_file() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-symlink-legacy-config");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        let tmux_root = xdg.join("tmux");
        fs::create_dir_all(&tmux_root).expect("create xdg tmux directory");
        let real_config = root.join("real-tmux.conf");
        write_test_config(
            real_config.clone(),
            "set -q -g status-utf8 on\nset -g base-index 1\n",
        );
        std::os::unix::fs::symlink(&real_config, tmux_root.join("tmux.conf"))
            .expect("create xdg tmux config symlink");

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "quiet legacy fallback config should not report startup errors"
        );
        let state = handler.state.lock().await;
        assert_eq!(state.options.global_value(OptionName::BaseIndex), Some("1"));
        assert!(
            state.message_log.iter().all(|entry| {
                !entry.msg.contains("status-utf8") && !entry.msg.contains("invalid option: utf8")
            }),
            "quiet legacy fallback config should not leak ignored options into show-messages: {:?}",
            state
                .message_log
                .iter()
                .map(|entry| entry.msg.as_str())
                .collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_ignores_unreadable_entries_and_keeps_later_safe_files() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-best-effort");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        create_test_dir_entry(first_tmux_fallback_path(&home, &xdg, &appdata));
        write_test_config(
            later_tmux_fallback_path(&home, &xdg, &appdata),
            "set -g status off\n",
        );

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "tmux fallback read errors should be ignored"
        );
        let state = handler.state.lock().await;
        assert_eq!(state.options.global_value(OptionName::Status), Some("off"));

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_can_be_disabled_by_env() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-env-disabled");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        write_test_config(tmux_user_config_path(&home), "set -g status off\n");

        let _env = TestConfigEnv::install(&home, &xdg, &appdata, Some("1"));
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        assert!(
            handler.startup_config_errors.lock().await.is_empty(),
            "disabled tmux fallback should not report config errors"
        );
        let state = handler.state.lock().await;
        assert_ne!(state.options.global_value(OptionName::Status), Some("off"));

        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("rmux-{label}-{}-{unique}", std::process::id()))
    }

    struct TestConfigEnv {
        _disable: EnvVarGuard,
        _home: EnvVarGuard,
        _xdg: EnvVarGuard,
        _userprofile: EnvVarGuard,
        _appdata: EnvVarGuard,
        _rmux_config: EnvVarGuard,
    }

    impl TestConfigEnv {
        fn install(
            home: &Path,
            xdg: &Path,
            appdata: &Path,
            disable_tmux_fallback: Option<&str>,
        ) -> Self {
            let home = home.to_string_lossy();
            let xdg = xdg.to_string_lossy();
            let appdata = appdata.to_string_lossy();

            Self {
                _disable: EnvVarGuard::set("RMUX_DISABLE_TMUX_FALLBACK", disable_tmux_fallback),
                _home: EnvVarGuard::set("HOME", Some(&home)),
                _xdg: EnvVarGuard::set("XDG_CONFIG_HOME", Some(&xdg)),
                _userprofile: EnvVarGuard::set("USERPROFILE", Some(&home)),
                _appdata: EnvVarGuard::set("APPDATA", Some(&appdata)),
                _rmux_config: EnvVarGuard::set("RMUX_CONFIG_FILE", None),
            }
        }
    }

    fn create_test_config_dirs(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let home = root.join("home");
        let xdg = root.join("xdg");
        let appdata = root.join("appdata");
        fs::create_dir_all(&home).expect("home directory");
        fs::create_dir_all(&xdg).expect("xdg directory");
        fs::create_dir_all(&appdata).expect("appdata directory");
        (home, xdg, appdata)
    }

    fn rmux_user_config_path(home: &Path) -> PathBuf {
        home.join(".rmux.conf")
    }

    fn tmux_user_config_path(home: &Path) -> PathBuf {
        home.join(".tmux.conf")
    }

    #[cfg(windows)]
    fn first_tmux_fallback_path(_home: &Path, xdg: &Path, _appdata: &Path) -> PathBuf {
        xdg.join("tmux").join("tmux.conf")
    }

    #[cfg(not(windows))]
    fn first_tmux_fallback_path(home: &Path, _xdg: &Path, _appdata: &Path) -> PathBuf {
        home.join(".tmux.conf")
    }

    #[cfg(windows)]
    fn later_tmux_fallback_path(home: &Path, _xdg: &Path, _appdata: &Path) -> PathBuf {
        home.join(".tmux.conf")
    }

    #[cfg(not(windows))]
    fn later_tmux_fallback_path(_home: &Path, xdg: &Path, _appdata: &Path) -> PathBuf {
        xdg.join("tmux").join("tmux.conf")
    }

    fn create_test_dir_entry(path: PathBuf) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("test config parent directory");
        }
        fs::create_dir(path).expect("unreadable directory entry");
    }

    fn write_test_config(path: PathBuf, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("test config parent directory");
        }
        fs::write(path, contents).expect("test config file");
    }

    #[cfg(unix)]
    fn shell_quote_path(path: &Path) -> String {
        let value = path.to_string_lossy();
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
