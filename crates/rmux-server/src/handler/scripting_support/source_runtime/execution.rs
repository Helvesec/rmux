use crate::control::QueueExecutionError;

use rmux_proto::{CommandOutput, RmuxError, Target};

use super::super::config_engine::nonempty_stdout;
use super::super::queue::{QueueCommandAction, QueueExecutionContext, QueueMode};
use super::super::queue_special_target::QueueSpecialTargetBinding;
use super::super::source_files::{LoadedSourceFile, ParsedSourceFileCommand};
use super::super::targets::command_has_independent_target;
use super::{source_error_has_line_prefix, RequestHandler, SourceFileExecution};

impl RequestHandler {
    pub(in crate::handler::scripting_support) async fn execute_queued_source_file(
        &self,
        requester_pid: u32,
        mut command: ParsedSourceFileCommand,
        context: &QueueExecutionContext,
        target_binding: Option<&QueueSpecialTargetBinding>,
    ) -> Result<QueueCommandAction, RmuxError> {
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        let historical_retired =
            target_binding.is_some_and(QueueSpecialTargetBinding::is_historical_retired);
        let depth = context.source_file_depth.saturating_add(1);
        command.current_file = context.current_file.clone();
        let explicit_target = target_binding
            .map(QueueSpecialTargetBinding::is_explicit)
            .unwrap_or(command.target.is_some() || command.target_missing_canfail);
        if command.target_missing_canfail {
            command.target = self
                .implicit_source_file_target(requester_pid)
                .await
                .unwrap_or_default();
        } else if command.target.is_none() && !historical_retired {
            if let Some(Target::Pane(target)) = context.current_target() {
                command.target = Some(target.clone());
            }
        }
        let sourced_target = command.target.clone().map(Target::Pane);
        let mut loaded = self
            .load_nested_source_file_command_with_context(&command, depth, explicit_target, context)
            .await?;
        if let Some(target_binding) = target_binding {
            target_binding.require_live_for(self).await?;
        }
        if loaded.selection_failed() {
            if let Some(status) = &context.source_input_queue_status {
                status.record_selection_failure(context.source_file_depth);
            }
        }
        let mut source_file_errors = Vec::new();
        let mut execution_errors = Vec::new();
        // Keep read/budget diagnostics on stderr even when a parse error in
        // another operand has a source line prefix. Classifying their combined
        // text would move the whole aggregate to stdout (or hide it under -n).
        if let Some(error) = loaded.take_read_error() {
            execution_errors.push(error);
        }
        let load_error = loaded.take_error();

        let mut batches = Vec::new();
        for batch in loaded.commands {
            let batch_context = if command.target_missing_canfail {
                target_binding
                    .map(|binding| binding.child_context(context))
                    .unwrap_or_else(|| context.clone())
                    .with_implicit_current_target(sourced_target.clone())
                    .without_retained_lifecycle_target()
            } else {
                target_binding
                    .map(|binding| binding.child_context(context))
                    .unwrap_or_else(|| context.clone())
            };
            let mut batch_context = batch_context.for_sourced_commands(depth, batch.current_file);
            if target_binding.is_none() && !command.target_missing_canfail && explicit_target {
                batch_context = batch_context.with_current_target(sourced_target.clone());
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
                Err(error) => source_file_errors.push(error),
            }
            if command.parse_only && !source_file_errors.is_empty() {
                break;
            }
        }
        if let Some(error) = load_error {
            if !command.parse_only || source_file_errors.is_empty() {
                if source_error_has_line_prefix(&error) {
                    source_file_errors.push(error);
                } else {
                    execution_errors.push(error);
                }
            }
        }
        let error = super::super::aggregate_rmux_errors(execution_errors);
        let source_file_error = super::super::aggregate_rmux_errors(source_file_errors);

        if command.parse_only || batches.is_empty() {
            return Ok(QueueCommandAction::Normal {
                output: nonempty_stdout(loaded.stdout),
                error,
                source_file_error,
                exit_status: None,
            });
        }

        Ok(QueueCommandAction::InsertAfter {
            batches,
            output: nonempty_stdout(loaded.stdout),
            error,
            source_file_error,
            exit_status: None,
        })
    }

    pub(super) async fn execute_loaded_source_file(
        &self,
        requester_pid: u32,
        loaded: LoadedSourceFile,
        mut context: QueueExecutionContext,
        depth: usize,
    ) -> SourceFileExecution {
        let mut stdout = Vec::new();
        let mut source_file_errors = Vec::new();
        let mut execution_errors = Vec::new();
        let mut exit_status = None;
        let mut batches = loaded.commands.into_iter().peekable();
        while let Some(batch) = batches.next() {
            // The request stopped for a shutdown in an earlier file: nothing further of it runs,
            // and what it acquired so far is its answer.
            if crate::listener::shutdown_answer::request_stopping_for_shutdown() {
                break;
            }
            // Only the canonical CLI envelope clears the location of its
            // batches: their validation failure is the command's own error.
            let located_source = batch.current_file.is_some();
            let mut batch_context = context.for_sourced_commands(depth, batch.current_file);
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
                if located_source {
                    source_file_errors.push(error);
                } else {
                    execution_errors.push(QueueExecutionError::CommandValidation(error));
                }
                continue;
            }
            let result = self
                .execute_command_queue(
                    requester_pid,
                    batch.commands,
                    &mut batch_context,
                    QueueMode::Detached,
                    None,
                )
                .await;
            stdout.extend_from_slice(&result.stdout);
            if let Some(status) = result.exit_status {
                exit_status = Some(status);
            }
            if let Some(error) = result.source_file_error {
                source_file_errors.push(error);
            }
            execution_errors.extend(result.execution_errors);
            context = batch_context;
            // Refresh only at a boundary whose first command will consume the
            // implicit target. Qualified commands and stable pinned contexts do
            // not depend on the requester's inherited environment.
            let next_batch_consumes_implicit_source_target = {
                let state = self.state.lock().await;
                let marked_target = state.marked_pane_target();
                batches
                    .peek()
                    .and_then(|batch| batch.commands.commands().first())
                    .is_some_and(|command| {
                        context.can_refresh_implicit_source_target()
                            && !command_has_independent_target(command, marked_target.as_ref())
                    })
            };
            if next_batch_consumes_implicit_source_target {
                match self.implicit_source_file_target(requester_pid).await {
                    Ok(target) => {
                        context = context.refresh_implicit_current_target(target.map(Target::Pane));
                    }
                    Err(error) => {
                        execution_errors.push(QueueExecutionError::Runtime(error));
                        break;
                    }
                }
            }
        }

        SourceFileExecution {
            output: CommandOutput::from_stdout(stdout),
            source_file_error: super::super::aggregate_rmux_errors(source_file_errors),
            execution_errors,
            exit_status,
        }
    }

    pub(super) async fn validate_loaded_source_file(
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
}
