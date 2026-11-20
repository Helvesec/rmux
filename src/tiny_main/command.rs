use super::*;
use crate::cli_args::schema::{self, CommandSpec};

impl TinyCommand {
    pub(super) const fn name(&self) -> &'static str {
        match self {
            Self::StartServer { .. } => schema::StartServer::NAME,
            Self::ListSessions { .. } => schema::ListSessions::NAME,
            Self::HasSession { .. } => schema::HasSession::NAME,
            Self::ListWindows { .. } => schema::ListWindows::NAME,
            Self::ListPanes { .. } => schema::ListPanes::NAME,
            Self::KillServer { .. } => schema::KillServer::NAME,
            Self::CapturePane { .. } => schema::CapturePane::NAME,
            Self::AttachSession { .. } => schema::AttachSession::NAME,
            Self::SplitWindow { .. } => schema::SplitWindow::NAME,
            Self::NewWindow { .. } => schema::NewWindow::NAME,
            Self::NewSession { .. } => schema::NewSession::NAME,
            Self::KillSession { .. } => schema::KillSession::NAME,
            Self::ShowOptions { command_name, .. } => command_name,
            Self::RenameWindow { .. } => schema::RenameWindow::NAME,
            Self::SelectWindow { .. } => schema::SelectWindow::NAME,
            Self::KillPane { .. } => schema::KillPane::NAME,
            Self::JoinPane { .. } => schema::JoinPane::NAME,
            Self::SetOption { command_name, .. } => command_name,
            Self::ResizePane { .. } => schema::ResizePane::NAME,
            Self::DisplayMessage { .. } => schema::DisplayMessage::NAME,
            Self::SendKeys { .. } => schema::SendKeys::NAME,
            Self::SourceFile { .. } => schema::SourceFile::NAME,
        }
    }

    pub(super) fn run(
        self,
        original_args: &[OsString],
        command_connection: &mut TinyCommandConnection,
    ) -> Result<i32, String> {
        match self {
            Self::StartServer { socket_path } => run_start_server(&socket_path),
            Self::ListSessions { socket_path } => {
                run_list_sessions(&socket_path, command_connection)
            }
            Self::HasSession {
                socket_path,
                request,
            } => run_has_session(original_args, &socket_path, request, command_connection),
            Self::ListWindows {
                socket_path,
                request,
            } => run_list_windows(original_args, &socket_path, request, command_connection),
            Self::ListPanes {
                socket_path,
                request,
            } => run_list_panes(original_args, &socket_path, request, command_connection),
            Self::KillServer { socket_path } => run_kill_server(&socket_path, command_connection),
            Self::CapturePane {
                socket_path,
                request,
            } => run_target_action(
                original_args,
                &socket_path,
                "capture-pane",
                RetryPolicy::DecodeOrEof,
                command_connection,
                |connection| connection.capture_pane_target_action(request),
            ),
            Self::AttachSession {
                socket_path,
                request,
            } => run_attach_session(original_args, &socket_path, request),
            Self::SplitWindow {
                socket_path,
                mut request,
            } => {
                if request.start_directory.is_none() {
                    request.start_directory = env::current_dir().ok();
                }
                run_target_action(
                    original_args,
                    &socket_path,
                    "split-window",
                    RetryPolicy::DecodeOnly,
                    command_connection,
                    |connection| connection.split_window_target_action(request),
                )
            }
            Self::NewWindow {
                socket_path,
                request,
            } => run_new_window(original_args, &socket_path, request, command_connection),
            Self::NewSession {
                socket_path,
                request,
            } => run_new_session(original_args, &socket_path, request),
            Self::KillSession {
                socket_path,
                request,
            } => run_kill_session(original_args, &socket_path, request, command_connection),
            Self::ShowOptions {
                socket_path,
                request,
                command_name,
            } => run_show_options(&socket_path, request, command_name, command_connection),
            Self::RenameWindow {
                socket_path,
                request,
            } => run_rename_window(original_args, &socket_path, request, command_connection),
            Self::SelectWindow {
                socket_path,
                request,
            } => run_select_window(original_args, &socket_path, request, command_connection),
            Self::KillPane {
                socket_path,
                request,
            } => run_kill_pane(original_args, &socket_path, request, command_connection),
            Self::JoinPane {
                socket_path,
                request,
            } => run_join_pane(original_args, &socket_path, request, command_connection),
            Self::SetOption {
                socket_path,
                request,
                command_name,
            } => run_set_option(&socket_path, request, command_name, command_connection),
            Self::ResizePane {
                socket_path,
                request,
            } => run_target_action(
                original_args,
                &socket_path,
                "resize-pane",
                RetryPolicy::DecodeOnly,
                command_connection,
                |connection| connection.resize_pane_target_action(request),
            ),
            Self::DisplayMessage {
                socket_path,
                request,
            } => run_display_message(original_args, &socket_path, request, command_connection),
            Self::SendKeys {
                socket_path,
                request,
            } => run_send_keys(&socket_path, request, command_connection),
            Self::SourceFile {
                socket_path,
                request,
            } => run_source_file(&socket_path, request, command_connection),
        }
    }

    pub(super) fn runtime_alias_dispatch(
        &self,
        original_args: &[OsString],
        command_connection: &mut TinyCommandConnection,
    ) -> Result<RuntimeAliasDispatch, String> {
        // Keep startup classification independent of the invoking environment.
        // Nested attach needs the full helper's switch-client validation before
        // any connection or alias probe, on both cold and running servers.
        if matches!(self, Self::AttachSession { .. }) && parse::nested_multiplexer_context() {
            return Ok(RuntimeAliasDispatch::FullHelper);
        }
        if matches!(self, Self::SplitWindow { request, .. } if request.keep_alive_on_exit == Some(true))
        {
            return Ok(RuntimeAliasDispatch::FullHelper);
        }
        let arguments = tiny_invoked_command_arguments(original_args)
            .ok_or_else(|| "tiny command invocation contains invalid UTF-8".to_owned())?;
        if arguments.is_empty() {
            return Err("tiny command invocation did not contain a command name".to_owned());
        }
        let socket_path = self.socket_path();
        let mut connection = match connect_or_absent(socket_path) {
            Ok(ConnectResult::Connected(connection)) => connection,
            Ok(ConnectResult::Absent) if self.starts_server() => {
                return Ok(RuntimeAliasDispatch::FullHelper);
            }
            Ok(ConnectResult::Absent) => {
                return Ok(RuntimeAliasDispatch::Direct);
            }
            Err(error)
                if self.is_kill_server()
                    && legacy_shutdown_fallback_wire_version(&error).is_some() =>
            {
                return Ok(RuntimeAliasDispatch::Direct);
            }
            Err(error) => return Err(client_error(socket_path, error)),
        };
        // Send-keys owns target resolution and its diagnostics on the direct
        // route. Check alias equivalence below before entering that route;
        // a generic execution offer can fall back merely for a missing target.
        if !self.starts_server()
            && !self.is_kill_server()
            && !matches!(self, Self::SourceFile { .. } | Self::SendKeys { .. })
        {
            // Alias expansion is read-only. Never use the historical mutating
            // offer to discover that an alias introduces split/key.
            match expand_runtime_command_segment(&mut connection, &arguments)
                .map_err(|error| format!("{}: {error}", socket_path.display()))?
            {
                Some(canonical) => {
                    if runtime_execution::requires_explicit_admission(&canonical)? {
                        return Ok(RuntimeAliasDispatch::FullHelper);
                    }
                }
                None => {
                    return self.legacy_alias_dispatch(
                        connection,
                        &arguments[0],
                        command_connection,
                    )
                }
            }
            let result = runtime_execution::execute(&mut connection, arguments.clone())
                .map_err(|error| format!("{}: {error}", socket_path.display()))?;
            return match result {
                rmux_proto::RuntimeCommandExecutionResponse::Unsupported(error) => {
                    Err(error.to_string())
                }
                rmux_proto::RuntimeCommandExecutionResponse::Fallback => {
                    Ok(RuntimeAliasDispatch::FullHelper)
                }
                rmux_proto::RuntimeCommandExecutionResponse::Complete {
                    command_name,
                    stdout,
                    error,
                    exit_status,
                } => {
                    trace_direct(self.name());
                    write_stdout(&stdout).map_err(|error| error.to_string())?;
                    if let Some(error) = error {
                        write_stderr(
                            format!("{}\n", tmux_cli_error_message(&command_name, &error))
                                .as_bytes(),
                        )
                        .map_err(|error| error.to_string())?;
                    }
                    Ok(RuntimeAliasDispatch::Complete(exit_status))
                }
            };
        }
        let canonical = match expand_runtime_command_segment(&mut connection, &arguments) {
            Ok(Some(canonical)) => canonical,
            Ok(None) => {
                return self.legacy_alias_dispatch(connection, &arguments[0], command_connection);
            }
            Err(error) if self.is_kill_server() && error.previous_wire_version().is_some() => {
                return Ok(RuntimeAliasDispatch::Direct);
            }
            // Like an absent endpoint: the full helper's auto-start waits for
            // the exiting daemon instead of failing on it.
            Err(error) if self.starts_server() && error.is_unanswered_by_exiting_daemon() => {
                return Ok(RuntimeAliasDispatch::FullHelper);
            }
            Err(error) => return Err(format!("{}: {error}", socket_path.display())),
        };
        let local = CommandParser::new()
            .parse_arguments(&arguments)
            .map_err(|error| error.to_string())?;
        let server = if canonical.is_empty() {
            ParsedCommands::default()
        } else {
            CommandParser::new()
                .with_command_aliases(std::iter::empty::<String>())
                .with_exact_commands(RMUX_EXTENSION_COMMANDS)
                .parse_one_group(&canonical)
                .map_err(|error| error.to_string())?
        };
        if local.commands() != server.commands() || local.assignments() != server.assignments() {
            Ok(RuntimeAliasDispatch::FullHelper)
        } else {
            Ok(self.direct_alias_dispatch(connection, command_connection))
        }
    }

    pub(super) fn legacy_alias_dispatch(
        &self,
        mut connection: Connection,
        command_name: &str,
        command_connection: &mut TinyCommandConnection,
    ) -> Result<RuntimeAliasDispatch, String> {
        let response = connection
            .show_options(
                OptionScopeSelector::ServerGlobal,
                Some("command-alias".to_owned()),
                false,
                false,
                true,
            )
            .map_err(|error| client_error(self.socket_path(), error))?;
        let definitions = match response {
            Response::ShowOptions(response) => {
                decode_command_alias_definitions(response.command_output().stdout())
                    .map_err(|error| error.to_string())?
            }
            Response::Error(error)
                if self.is_kill_server() && previous_wire_error(&error.error).is_some() =>
            {
                return Ok(RuntimeAliasDispatch::Direct);
            }
            Response::Error(error) => {
                return Err(tmux_cli_error_message("show-options", &error.error));
            }
            response => {
                return Err(format!(
                    "protocol error: command-alias probe returned {}",
                    response.command_name()
                ));
            }
        };
        if definitions
            .iter()
            .any(|definition| definition_matches_name(definition, command_name))
        {
            Ok(RuntimeAliasDispatch::FullHelper)
        } else {
            Ok(self.direct_alias_dispatch(connection, command_connection))
        }
    }

    pub(super) fn direct_alias_dispatch(
        &self,
        connection: Connection,
        command_connection: &mut TinyCommandConnection,
    ) -> RuntimeAliasDispatch {
        *command_connection = if self.starts_server() {
            // Startup paths must keep their stronger socket and server-freshness validation.
            TinyCommandConnection::disconnected(self.socket_path())
        } else {
            TinyCommandConnection::reusable(self.socket_path(), connection)
        };
        RuntimeAliasDispatch::Direct
    }

    const fn is_kill_server(&self) -> bool {
        matches!(self, Self::KillServer { .. })
    }

    pub(super) const fn starts_server(&self) -> bool {
        matches!(
            self,
            Self::StartServer { .. } | Self::AttachSession { .. } | Self::NewSession { .. }
        )
    }

    pub(super) fn socket_path(&self) -> &Path {
        match self {
            Self::StartServer { socket_path }
            | Self::ListSessions { socket_path }
            | Self::HasSession { socket_path, .. }
            | Self::ListWindows { socket_path, .. }
            | Self::ListPanes { socket_path, .. }
            | Self::KillServer { socket_path }
            | Self::CapturePane { socket_path, .. }
            | Self::AttachSession { socket_path, .. }
            | Self::SplitWindow { socket_path, .. }
            | Self::NewWindow { socket_path, .. }
            | Self::NewSession { socket_path, .. }
            | Self::KillSession { socket_path, .. }
            | Self::ShowOptions { socket_path, .. }
            | Self::RenameWindow { socket_path, .. }
            | Self::SelectWindow { socket_path, .. }
            | Self::KillPane { socket_path, .. }
            | Self::JoinPane { socket_path, .. }
            | Self::SetOption { socket_path, .. }
            | Self::ResizePane { socket_path, .. }
            | Self::DisplayMessage { socket_path, .. }
            | Self::SendKeys { socket_path, .. }
            | Self::SourceFile { socket_path, .. } => socket_path,
        }
    }
}
