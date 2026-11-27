use rmux_core::{
    command_inventory::RMUX_EXTENSION_COMMANDS,
    command_parser::{
        CommandParser, EnvironmentAssignment, ParsedCommands, SOURCE_FILE_MAX_COMMAND_BYTES,
    },
};
use rmux_proto::{PaneTarget, RmuxError, Target};

use super::super::format_context::{
    apply_client_loop_context, format_context_for_target_with_server_values, global_format_context,
    ParseTimeFormats,
};
use super::super::parser_context::command_parser_from_state;
use super::super::queue::QueueExecutionContext;
use super::super::source_files::{
    source_inputs_for_path_with_diagnostics, source_selection_limit_error, LoadedSourceFile,
    ParsedSourceFileCommand, SourceInput, SourceSyntax, SourcedParsedCommands,
    MAX_SOURCE_AGGREGATE_BYTES, MAX_SOURCE_MATCHED_FILES,
};
use super::super::tmux_compat::tmux_compat_input;
use super::parsing::{
    parse_source_fragment_for_execution, parse_source_fragment_recovering_for_import_compat,
    parse_source_fragment_until_first_error,
};
use super::verbose::append_verbose_commands;
use super::{ParsedSourceInput, RequestHandler, SourceParseOptions};
use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::render_runtime_template;
#[cfg(windows)]
use crate::handler::pane_support::format_references_pane_pid;

impl RequestHandler {
    pub(super) async fn load_startup_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
    ) -> Result<LoadedSourceFile, RmuxError> {
        self.load_source_file_command(command, depth, false, true, None, None)
            .await
    }

    pub(super) async fn load_explicit_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
        explicit_target: bool,
    ) -> Result<LoadedSourceFile, RmuxError> {
        self.load_source_file_command(
            command,
            depth,
            explicit_target,
            !explicit_target,
            None,
            None,
        )
        .await
    }

    pub(super) async fn load_nested_source_file_command_with_context(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
        explicit_target: bool,
        context: &QueueExecutionContext,
    ) -> Result<LoadedSourceFile, RmuxError> {
        self.load_source_file_command(
            command,
            depth,
            explicit_target,
            !explicit_target,
            context
                .client_loop_snapshots()
                .map(<[ClientLoopSnapshot]>::to_vec),
            context.client_name.as_deref(),
        )
        .await
    }

    async fn load_source_file_command(
        &self,
        command: &ParsedSourceFileCommand,
        depth: usize,
        explicit_target: bool,
        implicit_target_refresh: bool,
        client_loop_snapshots: Option<Vec<ClientLoopSnapshot>>,
        client_name: Option<&str>,
    ) -> Result<LoadedSourceFile, RmuxError> {
        debug_assert!(
            !(explicit_target && implicit_target_refresh),
            "explicit target and implicit target refresh are mutually exclusive"
        );
        if depth > super::super::SOURCE_FILE_NESTING_LIMIT {
            return Err(RmuxError::Server("too many nested files".to_owned()));
        }

        let owned_client_loop_snapshots = match client_loop_snapshots {
            Some(snapshots) => snapshots,
            None => self.client_loop_snapshots().await,
        };

        let mut loaded = LoadedSourceFile::default();
        let mut matched_files = crate::config_paths::ConfigPathMatchBudget::default();
        let mut content_bytes = 0_usize;
        let mut parse_time_assignments = Vec::new();

        for path in &command.paths {
            let expanded_path = if command.expand_paths {
                self.render_source_file_path(
                    path,
                    command.target.as_ref(),
                    command.current_file.as_deref(),
                    &owned_client_loop_snapshots,
                    client_name,
                )
                .await?
            } else {
                path.clone()
            };
            let read = match source_inputs_for_path_with_diagnostics(
                &expanded_path,
                command.caller_cwd.as_deref(),
                command.quiet,
                command.stdin.as_deref(),
                command.read_policy(),
            ) {
                Ok(inputs) => inputs,
                Err(error) => {
                    loaded.push_read_error(error);
                    continue;
                }
            };
            loaded.record_matched_files(read.matched_files);
            if let Some(error) = read.error {
                loaded.push_read_error(error);
            }
            let next_content_bytes = content_bytes.saturating_add(read.content_bytes);
            if matched_files.reserve(read.budgeted_files).is_err()
                || next_content_bytes > MAX_SOURCE_AGGREGATE_BYTES
            {
                // A per-path rejection already carries its precise diagnostic.
                if read.matched_files <= MAX_SOURCE_MATCHED_FILES
                    && read.content_bytes <= MAX_SOURCE_AGGREGATE_BYTES
                {
                    loaded.push_read_error(source_selection_limit_error());
                }
                break;
            }
            content_bytes = next_content_bytes;
            let inputs = read.inputs;
            if !inputs.is_empty() {
                loaded.record_loaded_files(inputs.len());
            }
            for input in inputs {
                let input = match command.syntax {
                    SourceSyntax::Rmux | SourceSyntax::Canonical => input,
                    SourceSyntax::TmuxCompat => tmux_compat_input(&input),
                };
                if input.contents.trim().is_empty() {
                    continue;
                }
                let parsed = match self
                    .parse_source_input(
                        &input,
                        command.target.as_ref(),
                        SourceParseOptions {
                            stop_at_first_error: command.parse_only,
                            syntax: command.syntax,
                            parse_time_assignments: &parse_time_assignments,
                        },
                        &owned_client_loop_snapshots,
                        client_name,
                    )
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
                    for commands in parsed.commands {
                        if command.verbose && !command.parse_only {
                            append_verbose_commands(
                                &mut loaded.stdout,
                                &input.current_file,
                                &commands,
                            );
                        }
                        if !command.parse_only {
                            extend_parse_time_assignments(&mut parse_time_assignments, &commands);
                            if let Some(assignments_only) = assignments_only_commands(&commands) {
                                loaded.commands.push(SourcedParsedCommands {
                                    commands: assignments_only,
                                    current_file: Some(input.current_file.clone()),
                                });
                            }
                        }
                    }
                    continue;
                }
                for commands in parsed.commands {
                    if !command.parse_only {
                        extend_parse_time_assignments(&mut parse_time_assignments, &commands);
                    }
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

    async fn render_source_file_path(
        &self,
        path: &str,
        target: Option<&PaneTarget>,
        current_file: Option<&str>,
        client_loop_snapshots: &[ClientLoopSnapshot],
        client_name: Option<&str>,
    ) -> Result<String, RmuxError> {
        #[cfg(windows)]
        if format_references_pane_pid(Some(path)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }
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
        context = apply_client_loop_context(context, Some(client_loop_snapshots), client_name);

        if let Some(current_file) = current_file {
            context = context.with_named_value("current_file", current_file);
        }
        Ok(render_runtime_template(path, &context, false))
    }

    async fn parse_source_input(
        &self,
        input: &SourceInput,
        target: Option<&PaneTarget>,
        options: SourceParseOptions<'_>,
        client_loop_snapshots: &[ClientLoopSnapshot],
        client_name: Option<&str>,
    ) -> Result<ParsedSourceInput, RmuxError> {
        let SourceParseOptions {
            stop_at_first_error,
            syntax,
            parse_time_assignments,
        } = options;
        let attached_count = if let Some(target) = target {
            self.attached_count(target.session_name()).await
        } else {
            0
        };
        let socket_path = self.socket_path();
        let state = self.state.lock().await;
        let mut parser = command_parser_from_state(&state)
            .with_exact_commands(RMUX_EXTENSION_COMMANDS)
            .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
        if syntax == SourceSyntax::Canonical {
            parser = parser.with_command_aliases(std::iter::empty::<String>());
        }
        if !stop_at_first_error {
            for assignment in parse_time_assignments {
                parser = parser.with_environment_value(assignment.name(), assignment.value());
            }
        }
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
        let context = apply_client_loop_context(context, Some(client_loop_snapshots), client_name);
        parser = ParseTimeFormats::of(&context).seed_parser(parser, &input.contents);
        let mut parsed = ParsedSourceInput::default();
        if stop_at_first_error {
            parse_source_fragment_until_first_error(
                &parser,
                input,
                &input.contents,
                0,
                &mut parsed,
            );
        } else if syntax == SourceSyntax::TmuxCompat {
            parse_source_fragment_recovering_for_import_compat(
                &parser,
                input,
                &input.contents,
                0,
                &mut parsed,
            );
        } else {
            parse_source_fragment_for_execution(&parser, input, &input.contents, 0, &mut parsed);
        }
        Ok(parsed)
    }
}

fn extend_parse_time_assignments(
    target: &mut Vec<EnvironmentAssignment>,
    commands: &ParsedCommands,
) {
    target.extend(commands.assignments().iter().cloned());
}

fn assignments_only_commands(commands: &ParsedCommands) -> Option<ParsedCommands> {
    let source = commands.assignments_to_tmux_reparse_string();
    if source.is_empty() {
        return None;
    }
    Some(
        CommandParser::new()
            .parse_one_group(&source)
            .expect("parsed assignments should reparse losslessly"),
    )
}

#[cfg(test)]
mod tests {
    // Proves that source-file captures the parse-time values only through
    // `ParseTimeFormats`: a file without a directive must not capture them.
    use rmux_proto::{NewSessionRequest, PaneTarget, Request, SessionName, TerminalSize};

    use super::super::super::format_context::parse_time_captures_on_this_thread;
    use super::super::super::source_files::{SourceInput, SourceSyntax};
    use super::{RequestHandler, SourceParseOptions};

    /// Parses `contents` as a file sourced on session alpha and returns the
    /// command names it yields with the captures it made on this thread.
    async fn source_on_alpha(contents: &str) -> (Vec<String>, usize) {
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
        let input = SourceInput {
            current_file: "gate.conf".to_owned(),
            contents: contents.to_owned(),
        };
        let captures_before = parse_time_captures_on_this_thread();
        let parsed = handler
            .parse_source_input(
                &input,
                Some(&PaneTarget::new(session_name, 0)),
                SourceParseOptions {
                    stop_at_first_error: false,
                    syntax: SourceSyntax::Rmux,
                    parse_time_assignments: &[],
                },
                &[],
                None,
            )
            .await
            .expect("source input parses");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let names = parsed
            .commands
            .iter()
            .flat_map(|commands| commands.commands())
            .map(|command| command.name().to_owned())
            .collect();
        (
            names,
            parse_time_captures_on_this_thread() - captures_before,
        )
    }

    #[tokio::test]
    async fn source_file_captures_the_parse_time_values_only_for_a_directive() {
        assert_eq!(
            source_on_alpha(
                "%if #{==:#{session_name},alpha}\nlist-sessions\n%elif #{current_file}\nlist-buffers\n%else\nlist-windows\n%endif\n",
            )
            .await,
            (vec!["list-sessions".to_owned()], 1)
        );
        assert_eq!(
            source_on_alpha("%if #{==:#{current_file},gate.conf}\nlist-buffers\n%endif\n").await,
            (vec!["list-buffers".to_owned()], 1)
        );
        assert_eq!(
            source_on_alpha("display-message -p \"#{session_name}\"\nlist-sessions\n").await,
            (
                vec!["display-message".to_owned(), "list-sessions".to_owned()],
                0
            )
        );
    }
}
