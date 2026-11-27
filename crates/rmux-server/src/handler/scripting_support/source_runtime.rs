use crate::control::QueueExecutionError;

use rmux_core::{
    command_inventory::RMUX_EXTENSION_COMMANDS,
    command_parser::{EnvironmentAssignment, ParsedCommands, SOURCE_FILE_MAX_COMMAND_BYTES},
};
use rmux_proto::{
    CommandOutput, ErrorResponse, PaneTarget, Response, RmuxError, SourceFileRequest,
    SourceFileResponse, Target,
};

use super::super::control_support::current_control_queue_identity;
use super::super::target_support::{
    inherited_pane_target, requester_environment_context, RequesterEnvironmentContext,
};
use super::super::RequestHandler;
use super::config_engine::{append_error_output, config_error_lines};
use super::parser_context::command_parser_from_state;
use super::queue::QueueExecutionContext;
use super::source_files::{LoadedSourceFile, ParsedSourceFileCommand, SourceSyntax};
use super::source_internal::{
    canonical_command_execution_request, validate_internal_source_file_path,
    CanonicalCommandExecution,
};
use super::targets::active_session_target;

mod execution;
mod loading;
mod parsing;
mod startup;
mod validation;
mod verbose;

use verbose::{append_verbose_loaded_commands, source_error_location_for_loaded};

const CONFIG_MESSAGE_MAX_BYTES: usize = 512;

#[derive(Default)]
pub(super) struct FileSourceOffer {
    pub input: Option<super::invocation_input::InvocationInput>,
    pub queue_status: Option<super::source_input_queue_status::SourceInputQueueStatus>,
}

struct SourceParseOptions<'a> {
    stop_at_first_error: bool,
    syntax: SourceSyntax,
    parse_time_assignments: &'a [EnvironmentAssignment],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StaleImplicitSource {
    Reject,
    DeferToMarkedCommand,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_source_file(
        &self,
        requester_pid: u32,
        request: SourceFileRequest,
    ) -> Response {
        self.handle_source_file_with_input(requester_pid, request, None)
            .await
    }

    pub(super) async fn handle_source_file_with_input(
        &self,
        requester_pid: u32,
        mut request: SourceFileRequest,
        file_offer: Option<FileSourceOffer>,
    ) -> Response {
        if let Err(error) = validate_internal_source_file_path(&request) {
            return Response::Error(ErrorResponse { error });
        }
        let canonical_execution = match canonical_command_execution_request(&request) {
            Ok(CanonicalCommandExecution::Disabled) => false,
            Ok(CanonicalCommandExecution::Canonical) => true,
            Ok(CanonicalCommandExecution::ListWindowsAll(arguments)) => {
                let canonical = {
                    let state = self.state.lock().await;
                    let parser = command_parser_from_state(&state)
                        .with_command_aliases(std::iter::empty::<String>())
                        .with_exact_commands(RMUX_EXTENSION_COMMANDS)
                        .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
                    match parser.parse_arguments_with_assignments(&arguments) {
                        Ok(parsed)
                            if parsed.assignments().is_empty()
                                && matches!(
                                    parsed.commands(),
                                    [command] if command.name() == "list-windows"
                                ) =>
                        {
                            parsed.to_tmux_reparse_string()
                        }
                        Ok(_) => {
                            return Response::Error(ErrorResponse {
                                error: RmuxError::Server(
                                    "invalid internal list-windows command expansion".to_owned(),
                                ),
                            });
                        }
                        Err(error) => {
                            return Response::Error(ErrorResponse {
                                error: RmuxError::Server(error.message().to_owned()),
                            });
                        }
                    }
                };
                request.stdin = Some(canonical);
                true
            }
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        if let Some(response) = self
            .handle_internal_source_file_request(requester_pid, &request)
            .await
        {
            return response;
        }
        let mut command = ParsedSourceFileCommand::from(request);
        if canonical_execution {
            command.paths = vec!["-".to_owned()];
            command.syntax = SourceSyntax::Canonical;
        }
        let explicit_target = command.target.is_some();
        let socket_path = self.socket_path();
        let requester_environment = requester_environment_context(requester_pid, &socket_path);
        // The full CLI's internal canonical envelope is only a transport for
        // already parsed queue commands. Let each carried command choose its
        // own explicit, pinned, or inherited context instead of resolving an
        // implicit source target before the command can be inspected.
        if command.target.is_none() && !canonical_execution {
            command.target = match self
                .implicit_source_file_target_with_environment(
                    requester_pid,
                    requester_environment,
                    StaleImplicitSource::DeferToMarkedCommand,
                )
                .await
            {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
        }
        // A file offer wraps one source-file command. That command charges
        // its real file when it enters the queue.
        let depth = requester_environment
            .source_depth
            .unwrap_or(0)
            .saturating_add(usize::from(file_offer.is_none()));
        let invoked_from_sourced_shell = requester_environment.source_depth.is_some();
        let mut loaded = match self
            .load_explicit_source_file_command(&command, depth, explicit_target)
            .await
        {
            Ok(loaded) => loaded,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        // The canonical CLI envelope is transport, not a sourced stdin file.
        // Its synthetic path must not become a callback diagnostic location;
        // nested source-file commands install their own real locations.
        if canonical_execution {
            for batch in &mut loaded.commands {
                batch.current_file = None;
            }
        }
        let strict_errors = command.syntax != SourceSyntax::TmuxCompat;
        let mut stdout_errors = Vec::new();
        let mut stderr_errors = Vec::new();
        let mut stderr = Vec::new();
        if let Some(error) = loaded.take_read_error() {
            append_error_output(&mut stderr, &error);
            if invoked_from_sourced_shell {
                self.log_source_file_error_messages(&error, true).await;
            }
        }
        let load_error = loaded.take_error();

        let mut context = QueueExecutionContext::new(command.caller_cwd.clone());
        let offer = file_offer.unwrap_or_default();
        context.source_input_queue_status = offer.queue_status.map(|status| status.at_depth(depth));
        context.invocation_input = offer
            .input
            .map(super::invocation_input::SharedInvocationInput::new);
        let context = match command.target.clone().map(Target::Pane) {
            Some(target) if explicit_target => context.with_current_target(Some(target)),
            // Without -t the target was resolved from the requesting client.
            Some(target) => context
                .with_implicit_current_target(Some(target))
                .with_current_target_following_client(),
            None => context,
        };
        let mut stdout = std::mem::take(&mut loaded.stdout);
        let loaded_batch_count = loaded.commands.len();
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
                stdout_errors.push(error);
            }
        } else {
            if let Some(error) = load_error {
                stdout_errors.push(error);
            }
            let SourceFileExecution {
                output,
                source_file_error,
                execution_errors,
                exit_status: execution_exit_status,
            } = self
                .execute_loaded_source_file(requester_pid, loaded, context, depth)
                .await;
            stdout.extend_from_slice(output.stdout());
            if let Some(status) = execution_exit_status {
                exit_status = Some(status);
            }
            if let Some(error) = source_file_error {
                stdout_errors.push(error);
            }
            stderr_errors.extend(execution_errors);
        }

        let mut error_exit_status = false;
        if let Some(error) = super::aggregate_rmux_errors(stdout_errors) {
            self.log_source_file_error_messages(&error, invoked_from_sourced_shell)
                .await;
            if strict_errors {
                if !stdout.is_empty()
                    || !stderr.is_empty()
                    || (!command.parse_only && loaded_batch_count != 0)
                {
                    append_error_output(&mut stdout, &error);
                    error_exit_status = true;
                } else {
                    return Response::Error(ErrorResponse { error });
                }
            } else {
                append_error_output(&mut stdout, &error);
                error_exit_status = true;
            }
        }
        for diagnostic in stderr_errors {
            let error = diagnostic.error();
            self.log_source_file_error_messages(error, invoked_from_sourced_shell)
                .await;
            if strict_errors {
                match diagnostic {
                    QueueExecutionError::Runtime(error) => {
                        append_execution_error_output(&mut stderr, &error);
                    }
                    QueueExecutionError::CallbackParse(error)
                    | QueueExecutionError::CommandValidation(error) => {
                        append_error_output(&mut stderr, &error);
                    }
                }
                error_exit_status = true;
            } else {
                append_error_output(&mut stdout, error);
                error_exit_status = true;
            }
        }

        let exit_status = nonzero_exit_status(exit_status)
            .or_else(|| (error_exit_status || !stderr.is_empty()).then_some(1));
        Response::SourceFile(source_file_response(stdout, stderr, exit_status))
    }

    pub(super) async fn implicit_source_file_target(
        &self,
        requester_pid: u32,
    ) -> Result<Option<PaneTarget>, RmuxError> {
        let socket_path = self.socket_path();
        let requester_environment = requester_environment_context(requester_pid, &socket_path);
        self.implicit_source_file_target_with_environment(
            requester_pid,
            requester_environment,
            StaleImplicitSource::Reject,
        )
        .await
    }

    async fn implicit_source_file_target_with_environment(
        &self,
        requester_pid: u32,
        requester_environment: RequesterEnvironmentContext,
        stale_source: StaleImplicitSource,
    ) -> Result<Option<PaneTarget>, RmuxError> {
        let requester_attached_session = if current_control_queue_identity(requester_pid).is_some()
        {
            self.current_session_candidate(requester_pid).await
        } else {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&requester_pid)
                .map(|active| active.session_name.clone())
        };
        let attached_session = match requester_attached_session.as_ref() {
            Some(session_name) => Some(session_name.clone()),
            None => self.current_session_candidate(requester_pid).await,
        };
        let preferred_session = self.preferred_session_name().await.ok();
        let state = self.state.lock().await;
        // The requester's own attach or control queue is its direct current
        // context. A sole attach owned by another PID is only a default
        // fallback and must not replace an inherited identity that is present
        // but stale.
        let requester_attached_target = requester_attached_session
            .as_ref()
            .and_then(|session_name| active_session_target(&state.sessions, session_name));
        let fallback_attached_target = if requester_attached_target.is_none() {
            attached_session
                .as_ref()
                .and_then(|session_name| active_session_target(&state.sessions, session_name))
        } else {
            None
        };
        let inherited_target = if requester_attached_target.is_some() {
            None
        } else {
            match inherited_pane_target(&state.sessions, requester_environment, None) {
                Ok(target) => target,
                Err(_)
                    if stale_source == StaleImplicitSource::DeferToMarkedCommand
                        && state.marked_pane_target().is_some() =>
                {
                    // The source file has not been parsed yet, so the mark may
                    // be the command's real default source. Leave the current
                    // target unresolved; per-command validation will re-read
                    // the live mark under the parsing lock and will retain the
                    // inherited-context error if the command needs current.
                    return Ok(None);
                }
                Err(error) => return Err(error),
            }
        };
        let target = requester_attached_target
            .or(inherited_target)
            .or(fallback_attached_target)
            .or_else(|| {
                preferred_session
                    .as_ref()
                    .and_then(|session_name| active_session_target(&state.sessions, session_name))
            })
            .and_then(|target| match target {
                Target::Pane(target) => Some(target),
                _ => None,
            });
        Ok(target)
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
}

#[derive(Default)]
struct ParsedSourceInput {
    commands: Vec<ParsedCommands>,
    errors: Vec<RmuxError>,
    has_fatal_parse_error: bool,
}

struct SourceFileExecution {
    output: CommandOutput,
    source_file_error: Option<RmuxError>,
    execution_errors: Vec<QueueExecutionError>,
    exit_status: Option<i32>,
}

fn nonzero_exit_status(exit_status: Option<i32>) -> Option<i32> {
    exit_status.filter(|status| *status != 0)
}

fn source_file_response(
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_status: Option<i32>,
) -> SourceFileResponse {
    let response = if stdout.is_empty() {
        SourceFileResponse::no_output()
    } else {
        SourceFileResponse::from_output(CommandOutput::from_stdout(stdout))
    };
    response.with_stderr(stderr).with_exit_status(exit_status)
}

fn append_execution_error_output(stderr: &mut Vec<u8>, error: &RmuxError) {
    for line in config_error_lines(error) {
        let line = strip_execution_error_prefixes(&line);
        stderr.extend_from_slice(line.as_bytes());
        stderr.push(b'\n');
    }
}

fn strip_execution_error_prefixes(line: &str) -> &str {
    let line = strip_source_file_line_prefix(line);
    let Some(rest) = line.strip_prefix("invalid target '") else {
        return line;
    };
    match rest.split_once("': ") {
        Some((_, reason)) => reason,
        None => line,
    }
}

fn strip_source_file_line_prefix(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b':' {
            index += 1;
            continue;
        }

        let mut cursor = index + 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > index + 1 && cursor < bytes.len() && bytes[cursor] == b':' {
            let message = &line[cursor + 1..];
            return message.strip_prefix(' ').unwrap_or(message);
        }
        index += 1;
    }
    line
}

fn source_error_has_line_prefix(error: &RmuxError) -> bool {
    error.to_string().lines().any(|line| {
        let bytes = line.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != b':' {
                index += 1;
                continue;
            }

            let mut cursor = index + 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                cursor += 1;
            }
            if cursor > index + 1 && cursor < bytes.len() && bytes[cursor] == b':' {
                return true;
            }
            index += 1;
        }
        false
    })
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    use std::time::Duration;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::super::super::RequestHandler;
    use super::super::format_context::global_format_context;
    use super::super::source_files::{default_config_paths, default_tmux_fallback_paths};
    use crate::test_env::EnvVarGuard;
    use crate::DaemonConfig;
    use rmux_core::formats::FormatVariables;
    use rmux_proto::{OptionName, OptionScopeSelector};

    #[test]
    fn execution_error_prefix_stripping_handles_windows_paths() {
        assert_eq!(
            super::strip_source_file_line_prefix(
                r"C:\tmp\rmux\main.conf:12: invalid option: xyzzy"
            ),
            "invalid option: xyzzy"
        );
        assert_eq!(
            super::strip_source_file_line_prefix("/tmp/rmux/main.conf:12: invalid option: xyzzy"),
            "invalid option: xyzzy"
        );
        assert_eq!(
            super::strip_source_file_line_prefix("invalid option: xyzzy"),
            "invalid option: xyzzy"
        );
    }

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

    #[tokio::test]
    async fn explicit_startup_config_files_preserve_selection_order_and_spelling() {
        let root = unique_temp_root("explicit-config-files-format");
        write_test_config(root.join("first.conf"), "set -g status on\n");
        write_test_config(root.join("second.conf"), "set -g status off\n");
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![
                PathBuf::from("first.conf"),
                PathBuf::from("missing.conf"),
                PathBuf::from("second.conf"),
            ],
            false,
            Some(root.clone()),
        );

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let state = handler.state.lock().await;
        let runtime = global_format_context(&state, config.socket_path());
        assert_eq!(
            runtime.format_value_by_name("config_files").as_deref(),
            Some("first.conf,missing.conf,second.conf")
        );
        drop(state);

        write_test_config(root.join("later.conf"), "set -g status on\n");
        handler
            .load_explicit_source_file_command(
                &super::ParsedSourceFileCommand {
                    paths: vec!["later.conf".to_owned()],
                    quiet: false,
                    parse_only: false,
                    verbose: false,
                    expand_paths: false,
                    target: None,
                    target_missing_canfail: false,
                    caller_cwd: Some(root.clone()),
                    stdin: None,
                    current_file: None,
                    syntax: super::SourceSyntax::Rmux,
                },
                1,
                false,
            )
            .await
            .expect("later source-file should load");
        let state = handler.state.lock().await;
        let runtime = global_format_context(&state, config.socket_path());
        assert_eq!(
            runtime.format_value_by_name("config_files").as_deref(),
            Some("first.conf,missing.conf,second.conf"),
            "later source-file must not rewrite startup selection"
        );
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn explicit_startup_config_files_carry_assignments_and_continue_after_file_error() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("explicit-config-sequential-assignments");
        write_test_config(root.join("first.conf"), "STARTUP_ORDER=from-first\n");
        write_test_config(
            root.join("bad.conf"),
            "BROKEN_STARTUP=from-bad\ndefinitely-not-a-command\nset-option -g @startup_must_not_run yes\n",
        );
        write_test_config(
            root.join("third.conf"),
            "set-option -g @startup_order \"$STARTUP_ORDER\"\n\
             set-option -g @startup_after_bad \"$BROKEN_STARTUP\"\n",
        );
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_config_files(
            vec![
                PathBuf::from("first.conf"),
                PathBuf::from("bad.conf"),
                PathBuf::from("third.conf"),
            ],
            false,
            Some(root.clone()),
        );

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let errors = handler.startup_config_errors.lock().await;
        let rendered = errors
            .first()
            .expect("startup parse error should be retained")
            .to_string();
        assert!(
            rendered.contains("bad.conf:2: unknown command: definitely-not-a-command"),
            "expected intermediate startup config error, got {rendered}"
        );
        drop(errors);

        let state = handler.state.lock().await;
        for (name, value) in [
            ("@startup_order", "from-first"),
            ("@startup_after_bad", "from-bad"),
        ] {
            assert_eq!(
                state
                    .options
                    .show_options_lines_filtered(
                        &OptionScopeSelector::SessionGlobal,
                        Some(name),
                        true,
                    )
                    .expect("startup option should be set"),
                vec![value.to_owned()]
            );
        }
        assert!(
            state
                .options
                .show_options_lines_filtered(
                    &OptionScopeSelector::SessionGlobal,
                    Some("@startup_must_not_run"),
                    true,
                )
                .is_err(),
            "commands after a fatal startup parse error in one file must not execute"
        );
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn default_startup_config_files_report_rmux_search_selection() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("default-config-files-format");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        write_test_config(rmux_user_config_path(&home), "set -g status off\n");
        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let expected = default_config_paths().join(",");
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let state = handler.state.lock().await;
        let runtime = global_format_context(&state, config.socket_path());
        assert_eq!(
            runtime.format_value_by_name("config_files").as_deref(),
            Some(expected.as_str())
        );
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tmux_fallback_updates_the_reported_startup_selection() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("fallback-config-files-format");
        let (home, xdg, appdata) = create_test_config_dirs(&root);
        write_test_config(tmux_user_config_path(&home), "set -g status off\n");
        let _env = TestConfigEnv::install(&home, &xdg, &appdata, None);
        let expected = default_tmux_fallback_paths().join(",");
        let handler = RequestHandler::new();
        let config = DaemonConfig::new(root.join("rmux.sock")).with_default_config_load(true, None);

        handler
            .load_startup_config(config.config_load().clone())
            .await;

        let state = handler.state.lock().await;
        let runtime = global_format_context(&state, config.socket_path());
        assert_eq!(
            runtime.format_value_by_name("config_files").as_deref(),
            Some(expected.as_str())
        );
        drop(state);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_readiness_remains_busy_until_blocking_run_shell_finishes() {
        let _lock = crate::test_env::lock_async().await;
        let root = unique_temp_root("slow-run-shell-readiness");
        let config_path = root.join("slow.conf");
        write_test_config(config_path.clone(), "run-shell 'sleep 0.25'\n");
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

        let premature_readiness = async {
            while handler.config_loading_active() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(100), premature_readiness)
                .await
                .is_err(),
            "startup readiness must remain busy while the sourced command is running"
        );
        assert!(
            !task.is_finished(),
            "startup config should still be executing during the readiness check"
        );
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("startup config should finish")
            .expect("startup config task should not panic");
        assert!(
            !handler.config_loading_active(),
            "readiness should clear after the complete source queue finishes"
        );
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
    async fn explicit_startup_config_lookup_errors_stop_before_later_commands() {
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
            None,
            "startup config must retain lookup errors and stop before later commands"
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
