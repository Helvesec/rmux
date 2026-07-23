use rmux_core::{
    command_inventory::RMUX_EXTENSION_COMMANDS, command_parser::SOURCE_FILE_MAX_COMMAND_BYTES,
};
use rmux_proto::{
    decode_internal_list_windows_all_arguments, decode_internal_runtime_command_arguments,
    CommandOutput, ErrorResponse, Response, RmuxError, SourceFileRequest, SourceFileResponse,
    INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH, INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH,
    INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH, INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH,
};

use super::super::RequestHandler;
use super::parser_context::command_parser_from_state;

const INTERNAL_SOURCE_FILE_PATHS: [&str; 4] = [
    INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH,
    INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH,
    INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH,
    INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH,
];

pub(super) enum CanonicalCommandExecution {
    Disabled,
    Canonical,
    ListWindowsAll(Vec<String>),
}

impl RequestHandler {
    pub(super) async fn handle_internal_source_file_request(
        &self,
        requester_pid: u32,
        request: &SourceFileRequest,
    ) -> Option<Response> {
        if request.paths.as_slice() == [INTERNAL_RUNTIME_COMMAND_EXPANSION_PATH] {
            return Some(match runtime_command_expansion_payload(request) {
                Ok(payload) => {
                    self.handle_internal_runtime_command_expansion(payload)
                        .await
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            });
        }
        if request.paths.as_slice() == [INTERNAL_PARSE_TIME_ASSIGNMENTS_PATH] {
            return Some(match parse_time_assignments_payload(request) {
                Ok(payload) => {
                    self.handle_internal_parse_time_assignments(requester_pid, payload)
                        .await
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            });
        }
        None
    }

    async fn handle_internal_runtime_command_expansion(&self, payload: &str) -> Response {
        let arguments = match decode_internal_runtime_command_arguments(payload) {
            Ok(arguments) if !arguments.is_empty() => arguments,
            Ok(_) => {
                return server_error("invalid empty runtime command argument vector");
            }
            Err(_) => {
                return server_error("invalid internal runtime command expansion payload");
            }
        };

        let state = self.state.lock().await;
        let parser = command_parser_from_state(&state)
            .with_exact_commands(RMUX_EXTENSION_COMMANDS)
            .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
        let parsed = match parser.parse_arguments_with_assignments(&arguments) {
            Ok(parsed) => parsed,
            Err(error) => return server_error(error.message()),
        };
        let canonical = parsed.to_tmux_reparse_string();
        if canonical.is_empty() {
            Response::SourceFile(SourceFileResponse::no_output())
        } else {
            Response::SourceFile(SourceFileResponse::from_output(CommandOutput::from_stdout(
                canonical,
            )))
        }
    }

    async fn handle_internal_parse_time_assignments(
        &self,
        requester_pid: u32,
        payload: &str,
    ) -> Response {
        let parsed = {
            let state = self.state.lock().await;
            let parser = command_parser_from_state(&state)
                .with_exact_commands(RMUX_EXTENSION_COMMANDS)
                .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES);
            match parser.parse_one_group(payload) {
                Ok(parsed) if parsed.commands().is_empty() && !parsed.assignments().is_empty() => {
                    parsed
                }
                Ok(_) => return server_error("invalid internal parse-time assignment payload"),
                Err(error) => return server_error(error.message()),
            }
        };

        match self
            .apply_parse_time_assignments(requester_pid, &parsed, None)
            .await
        {
            Ok(()) => Response::SourceFile(SourceFileResponse::no_output()),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }
}

pub(super) fn validate_internal_source_file_path(
    request: &SourceFileRequest,
) -> Result<(), RmuxError> {
    if !request.paths.iter().any(|path| path.contains('\0')) {
        return Ok(());
    }
    if request.paths.len() == 1
        && INTERNAL_SOURCE_FILE_PATHS
            .iter()
            .any(|internal| request.paths[0] == *internal)
    {
        return Ok(());
    }
    Err(RmuxError::Server(
        "invalid internal source-file request path".to_owned(),
    ))
}

pub(super) fn canonical_command_execution_request(
    request: &SourceFileRequest,
) -> Result<CanonicalCommandExecution, RmuxError> {
    let execution = match request.paths.as_slice() {
        [path] if path == INTERNAL_CANONICAL_COMMAND_EXECUTION_PATH => {
            CanonicalCommandExecution::Canonical
        }
        [path] if path == INTERNAL_LIST_WINDOWS_ALL_EXECUTION_PATH => {
            let payload = request.stdin.as_deref().ok_or_else(|| {
                RmuxError::Server("missing internal list-windows all-session payload".to_owned())
            })?;
            let arguments =
                decode_internal_list_windows_all_arguments(payload).ok_or_else(|| {
                    RmuxError::Server(
                        "invalid internal list-windows all-session payload".to_owned(),
                    )
                })?;
            CanonicalCommandExecution::ListWindowsAll(arguments)
        }
        _ => return Ok(CanonicalCommandExecution::Disabled),
    };
    let target_is_allowed = matches!(execution, CanonicalCommandExecution::Canonical);
    if request.quiet
        || request.parse_only
        || request.verbose
        || request.expand_paths
        || (request.target.is_some() && !target_is_allowed)
        || request.stdin.is_none()
    {
        return Err(RmuxError::Server(
            "invalid internal canonical command execution request".to_owned(),
        ));
    }
    Ok(execution)
}

fn runtime_command_expansion_payload(request: &SourceFileRequest) -> Result<&str, RmuxError> {
    if request.quiet
        || !request.parse_only
        || !request.verbose
        || request.expand_paths
        || request.target.is_some()
    {
        return Err(RmuxError::Server(
            "invalid internal runtime command expansion request".to_owned(),
        ));
    }
    request.stdin.as_deref().ok_or_else(|| {
        RmuxError::Server("missing internal runtime command expansion payload".to_owned())
    })
}

fn parse_time_assignments_payload(request: &SourceFileRequest) -> Result<&str, RmuxError> {
    if request.quiet
        || request.parse_only
        || request.verbose
        || request.expand_paths
        || request.target.is_some()
    {
        return Err(RmuxError::Server(
            "invalid internal parse-time assignment request".to_owned(),
        ));
    }
    request.stdin.as_deref().ok_or_else(|| {
        RmuxError::Server("missing internal parse-time assignment payload".to_owned())
    })
}

fn server_error(message: &str) -> Response {
    Response::Error(ErrorResponse {
        error: RmuxError::Server(message.to_owned()),
    })
}
