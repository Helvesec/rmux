use rmux_core::{
    validate_hook_registration, HookBindingView, HookGlobalRoot, HookSetOptions, HookStore,
    ENVIRON_HIDDEN,
};
use rmux_proto::{
    CommandOutput, ErrorResponse, Response, RmuxError, ScopeSelector, SetEnvironmentMode,
    SetEnvironmentResponse, SetHookMutationRequest, SetHookResponse, ShowEnvironmentResponse,
    ShowHooksResponse, ShowOptionsResponse, WindowTarget,
};

use super::RequestHandler;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::handler_support::{ensure_option_scope_exists, ensure_scope_session_exists};
use crate::hook_compat::{normalize_set_hook_mutation_request, normalize_set_hook_request};

impl RequestHandler {
    pub(super) async fn handle_set_environment(
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

        let mut state = self.state.lock().await;

        if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
            return Response::Error(ErrorResponse { error });
        }

        let mode = request.mode.unwrap_or(SetEnvironmentMode::Set);
        let value = if request.format && matches!(mode, SetEnvironmentMode::Set) {
            let current_target = super::target_for_scope_selector(&state, &request.scope);
            let context = match current_target.as_ref() {
                Some(target) => {
                    match super::scripting_support::format_context_for_target(&state, target, 0) {
                        Ok(context) => context,
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    }
                }
                None => RuntimeFormatContext::new(rmux_core::formats::FormatContext::new())
                    .with_state(&state),
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

    pub(super) async fn handle_set_hook(&self, request: rmux_proto::SetHookRequest) -> Response {
        let request = normalize_set_hook_request(request);
        self.handle_set_hook_mutation_inner(SetHookMutationRequest {
            scope: request.scope,
            hook: request.hook,
            command: Some(request.command),
            lifecycle: request.lifecycle,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        })
        .await
    }

    pub(super) async fn handle_set_hook_mutation(
        &self,
        request: SetHookMutationRequest,
    ) -> Response {
        self.handle_set_hook_mutation_inner(normalize_set_hook_mutation_request(request))
            .await
    }

    async fn handle_set_hook_mutation_inner(&self, request: SetHookMutationRequest) -> Response {
        if let Err(error) = validate_hook_mutation_request(&request) {
            return Response::Error(ErrorResponse { error });
        }

        if request.run_immediately {
            let state = self.state.lock().await;

            if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }
            if let Err(error) = validate_hook_registration(request.hook, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }
            let current_target = super::target_for_scope_selector(&state, &request.scope);
            drop(state);

            self.queue_inline_hook(
                request.hook,
                request.scope.clone(),
                current_target,
                crate::hook_runtime::PendingInlineHookFormat::HookOnly,
            );

            return Response::SetHook(SetHookResponse {
                scope: request.scope,
                hook: request.hook,
                lifecycle: request.lifecycle,
            });
        }

        {
            let mut state = self.state.lock().await;

            if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }
            if let Err(error) = validate_hook_registration(request.hook, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }

            if request.unset {
                if let Err(error) =
                    state
                        .hooks
                        .unset(request.scope.clone(), request.hook, request.index)
                {
                    return Response::Error(ErrorResponse { error });
                }
            } else if let Some(command) = request.command.clone() {
                if let Err(error) = state.hooks.set_with_options(
                    request.scope.clone(),
                    request.hook,
                    command,
                    request.lifecycle,
                    HookSetOptions {
                        append: request.append,
                        index: request.index,
                    },
                ) {
                    return Response::Error(ErrorResponse { error });
                }
            }
        }

        Response::SetHook(SetHookResponse {
            scope: request.scope,
            hook: request.hook,
            lifecycle: request.lifecycle,
        })
    }

    pub(super) async fn handle_show_options(
        &self,
        request: rmux_proto::ShowOptionsRequest,
    ) -> Response {
        let state = self.state.lock().await;

        if let Err(error) = ensure_option_scope_exists(&state, &request.scope) {
            return Response::Error(ErrorResponse { error });
        }

        match state.options.show_options_lines_filtered(
            &request.scope,
            request.name.as_deref(),
            request.value_only,
        ) {
            Ok(lines) => Response::ShowOptions(ShowOptionsResponse {
                scope: request.scope,
                output: command_output_from_lines(&lines),
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(super) async fn handle_show_environment(
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

    pub(super) async fn handle_show_hooks(
        &self,
        request: rmux_proto::ShowHooksRequest,
    ) -> Response {
        let state = self.state.lock().await;

        if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
            return Response::Error(ErrorResponse { error });
        }

        match resolve_show_hooks_selection(&request) {
            Ok(ShowHooksSelection::GlobalSession) => Response::ShowHooks(ShowHooksResponse {
                scope: request.scope,
                output: command_output_from_lines(&render_global_hook_lines(
                    &state.hooks,
                    HookGlobalRoot::Session,
                    request.hook,
                )),
            }),
            Ok(ShowHooksSelection::GlobalWindow) => Response::ShowHooks(ShowHooksResponse {
                scope: request.scope,
                output: command_output_from_lines(&render_global_hook_lines(
                    &state.hooks,
                    HookGlobalRoot::Window,
                    request.hook,
                )),
            }),
            Ok(ShowHooksSelection::Session(session_name)) => {
                Response::ShowHooks(ShowHooksResponse {
                    scope: request.scope,
                    output: command_output_from_lines(&render_hook_lines(
                        &state
                            .hooks
                            .session_bindings_view(&session_name, request.hook),
                    )),
                })
            }
            Ok(ShowHooksSelection::Window(target)) => Response::ShowHooks(ShowHooksResponse {
                scope: request.scope,
                output: command_output_from_lines(&render_hook_lines(
                    &state.hooks.window_bindings_view(&target, request.hook),
                )),
            }),
            Ok(ShowHooksSelection::Pane(target)) => Response::ShowHooks(ShowHooksResponse {
                scope: request.scope,
                output: command_output_from_lines(&render_hook_lines(
                    &state.hooks.pane_bindings_view(&target, request.hook),
                )),
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
            let escaped = value
                .chars()
                .flat_map(|character| match character {
                    '$' | '`' | '"' | '\\' => ['\\', character].into_iter().collect::<Vec<_>>(),
                    other => [other].into_iter().collect::<Vec<_>>(),
                })
                .collect::<String>();
            format!("{name}=\"{escaped}\"; export {name};", name = entry.name)
        }
        None => format!("unset {};", entry.name),
    })
}

fn validate_hook_mutation_request(request: &SetHookMutationRequest) -> Result<(), RmuxError> {
    if request.run_immediately {
        if request.command.is_some() || request.append || request.unset || request.index.is_some() {
            return Err(RmuxError::Server(
                "set-hook -R only accepts a hook name".to_owned(),
            ));
        }
        return Ok(());
    }

    if request.unset {
        if request.command.is_some() {
            return Err(RmuxError::Server(
                "set-hook -u does not accept a command".to_owned(),
            ));
        }
        return Ok(());
    }

    if request.command.is_none() {
        return Err(RmuxError::Server("set-hook requires a command".to_owned()));
    }

    Ok(())
}

#[derive(Debug, Clone)]
enum ShowHooksSelection {
    GlobalSession,
    GlobalWindow,
    Session(rmux_proto::SessionName),
    Window(WindowTarget),
    Pane(rmux_proto::PaneTarget),
}

fn resolve_show_hooks_selection(
    request: &rmux_proto::ShowHooksRequest,
) -> Result<ShowHooksSelection, RmuxError> {
    if request.window && request.pane {
        return Err(RmuxError::Server(
            "show-hooks does not support combining -w and -p".to_owned(),
        ));
    }

    match (&request.scope, request.window, request.pane) {
        (ScopeSelector::Global, false, false) => Ok(ShowHooksSelection::GlobalSession),
        (ScopeSelector::Global, true, false) | (ScopeSelector::Global, false, true) => {
            Ok(ShowHooksSelection::GlobalWindow)
        }
        (ScopeSelector::Session(session_name), false, false) => {
            Ok(ShowHooksSelection::Session(session_name.clone()))
        }
        (ScopeSelector::Session(session_name), true, false) => Ok(ShowHooksSelection::Window(
            WindowTarget::new(session_name.clone()),
        )),
        (ScopeSelector::Session(_), false, true) => Err(RmuxError::Server(
            "show-hooks -p requires a pane target".to_owned(),
        )),
        (ScopeSelector::Window(target), true, false)
        | (ScopeSelector::Window(target), false, false) => {
            Ok(ShowHooksSelection::Window(target.clone()))
        }
        (ScopeSelector::Window(_), false, true) => Err(RmuxError::Server(
            "show-hooks -p requires a pane target".to_owned(),
        )),
        (ScopeSelector::Pane(target), false, true)
        | (ScopeSelector::Pane(target), false, false) => {
            Ok(ShowHooksSelection::Pane(target.clone()))
        }
        (ScopeSelector::Pane(_), true, false) => Err(RmuxError::Server(
            "show-hooks -w requires a session or window target".to_owned(),
        )),
        _ => unreachable!("validated show-hooks scope combinations"),
    }
}

fn render_hook_lines(bindings: &[HookBindingView]) -> Vec<String> {
    bindings
        .iter()
        .map(|binding| {
            format!(
                "{}[{}] {}",
                binding.hook(),
                binding.index(),
                binding.command()
            )
        })
        .collect()
}

fn render_global_hook_lines(
    store: &HookStore,
    root: HookGlobalRoot,
    filter: Option<rmux_proto::HookName>,
) -> Vec<String> {
    let explicit = store.global_bindings_view(root, filter);
    HookStore::shipped_global_hooks(root, filter)
        .into_iter()
        .flat_map(|hook| {
            let bindings = explicit
                .iter()
                .filter(|binding| binding.hook() == hook)
                .cloned()
                .collect::<Vec<_>>();
            if bindings.is_empty() {
                vec![hook.to_string()]
            } else {
                render_hook_lines(&bindings)
            }
        })
        .collect()
}

fn command_output_from_lines(lines: &[String]) -> CommandOutput {
    if lines.is_empty() {
        return CommandOutput::from_stdout(Vec::new());
    }

    CommandOutput::from_stdout(format!("{}\n", lines.join("\n")).into_bytes())
}
