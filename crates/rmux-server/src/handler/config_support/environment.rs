//! `set-environment` and `show-environment`: request validation, entry rendering and shell escaping.

use rmux_core::ENVIRON_HIDDEN;
use rmux_proto::{
    ErrorResponse, Response, RmuxError, ScopeSelector, SetEnvironmentMode, SetEnvironmentResponse,
    ShowEnvironmentResponse,
};

#[cfg(windows)]
use super::super::pane_support::format_references_pane_pid;
use super::super::RequestHandler;
use super::command_output_from_lines;
use crate::format_runtime::render_runtime_template;
use crate::handler_support::ensure_scope_session_exists;

impl RequestHandler {
    pub(in crate::handler) async fn handle_set_environment(
        &self,
        request: rmux_proto::SetEnvironmentRequest,
    ) -> Response {
        if !matches!(
            request.scope,
            ScopeSelector::Global | ScopeSelector::Session(_)
        ) {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "set-environment only supports global or session scope".to_owned(),
                ),
            });
        }

        if let Err(error) = validate_set_environment_request(&request) {
            return Response::Error(ErrorResponse { error });
        }

        #[cfg(windows)]
        if request.format && format_references_pane_pid(Some(&request.value)) {
            self.wait_for_windows_deferred_all_pane_pids().await;
        }

        let mut state = self.state.lock().await;

        if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
            return Response::Error(ErrorResponse { error });
        }

        let mode = request.mode.unwrap_or(SetEnvironmentMode::Set);
        let value = if request.format && matches!(mode, SetEnvironmentMode::Set) {
            let current_target = super::super::target_for_scope_selector(&state, &request.scope);
            let socket_path = self.socket_path();
            let context = match current_target.as_ref() {
                Some(target) => {
                    match super::super::scripting_support::format_context_for_target_with_server_values(
                        &state,
                        target,
                        0,
                        &socket_path,
                    ) {
                        Ok(context) => context,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    }
                }
                None => super::super::scripting_support::global_format_context(&state, &socket_path),
            };
            render_runtime_template(&request.value, &context, false)
        } else {
            request.value.clone()
        };

        match mode {
            SetEnvironmentMode::Set => state.environment.set_with_flags(
                request.scope.clone(),
                request.name.clone(),
                value,
                if request.hidden { ENVIRON_HIDDEN } else { 0 },
            ),
            SetEnvironmentMode::Clear => state
                .environment
                .clear(request.scope.clone(), request.name.clone()),
            SetEnvironmentMode::Unset => {
                let _ = state
                    .environment
                    .unset(request.scope.clone(), &request.name);
            }
        }

        Response::SetEnvironment(SetEnvironmentResponse {
            scope: request.scope,
            name: request.name,
        })
    }

    pub(in crate::handler) async fn handle_show_environment(
        &self,
        request: rmux_proto::ShowEnvironmentRequest,
    ) -> Response {
        if !matches!(
            request.scope,
            ScopeSelector::Global | ScopeSelector::Session(_)
        ) {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(
                    "show-environment only supports global or session scope".to_owned(),
                ),
            });
        }

        let state = self.state.lock().await;

        if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
            return Response::Error(ErrorResponse { error });
        }

        match state.environment.show_environment_entries(
            &request.scope,
            request.hidden,
            request.name.as_deref(),
        ) {
            Ok(entries) => Response::ShowEnvironment(ShowEnvironmentResponse {
                scope: request.scope,
                output: command_output_from_lines(
                    &entries
                        .into_iter()
                        .filter_map(|entry| {
                            render_show_environment_entry(&entry, request.shell_format)
                        })
                        .collect::<Vec<_>>(),
                ),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }
}

fn validate_set_environment_request(
    request: &rmux_proto::SetEnvironmentRequest,
) -> Result<(), RmuxError> {
    if request.name.is_empty() {
        return Err(RmuxError::Server("empty variable name".to_owned()));
    }
    if request.name.contains('=') {
        return Err(RmuxError::Server("variable name contains =".to_owned()));
    }

    match request.mode.unwrap_or(SetEnvironmentMode::Set) {
        SetEnvironmentMode::Set => Ok(()),
        SetEnvironmentMode::Clear if request.value.is_empty() => Ok(()),
        SetEnvironmentMode::Unset if request.value.is_empty() => Ok(()),
        SetEnvironmentMode::Clear => Err(RmuxError::Server(
            "can't specify a value with -r".to_owned(),
        )),
        SetEnvironmentMode::Unset => Err(RmuxError::Server(
            "can't specify a value with -u".to_owned(),
        )),
    }
}

fn render_show_environment_entry(
    entry: &rmux_core::ShowEnvironmentEntry,
    shell_format: bool,
) -> Option<String> {
    if !shell_format {
        return Some(match &entry.value {
            Some(value) => format!("{}={value}", entry.name),
            None => format!("-{}", entry.name),
        });
    }

    Some(match &entry.value {
        Some(value) => {
            let escaped = shell_escape_environment_value(value, !entry.value_is_display_escape);
            format!("{name}=\"{escaped}\"; export {name};", name = entry.name)
        }
        None => format!("unset {};", entry.name),
    })
}

fn shell_escape_environment_value(value: &str, escape_backslash: bool) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '$' | '`' | '"' => ['\\', character].into_iter().collect::<Vec<_>>(),
            '\\' if escape_backslash => ['\\', character].into_iter().collect::<Vec<_>>(),
            other => [other].into_iter().collect::<Vec<_>>(),
        })
        .collect()
}
