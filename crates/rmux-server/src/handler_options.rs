use std::path::Path;

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, OptionName, Response, RmuxError, ScopeSelector, SessionName,
    SetOptionByNameResponse, SetOptionResponse, WindowTarget,
};

use crate::format_runtime::render_runtime_template;
use crate::handler_support::{ensure_option_scope_exists, ensure_scope_session_exists};

use super::target_support::target_for_option_scope;
use super::{attach_support::option_affects_attached_rendering, RequestHandler};

impl RequestHandler {
    pub(super) async fn handle_set_option(
        &self,
        request: rmux_proto::SetOptionRequest,
    ) -> Response {
        if let Err(error) = rmux_core::validate_option_mutation(
            request.option,
            &request.scope,
            request.mode,
            &request.value,
        ) {
            return Response::Error(ErrorResponse { error });
        }

        let refresh_scope = option_affects_attached_rendering(request.option)
            .then(|| legacy_scope_to_refresh_scope(&request.scope));
        let destroy_unattached_scope = (request.option == OptionName::DestroyUnattached)
            .then(|| legacy_scope_to_refresh_scope(&request.scope));
        let alert_scope = legacy_scope_to_refresh_scope(&request.scope);
        let automatic_rename_scope = (request.option == OptionName::AutomaticRename)
            .then(|| legacy_scope_to_refresh_scope(&request.scope));
        let mut alerts_changed = false;
        let response = {
            let mut state = self.state.lock().await;

            if let Err(error) = ensure_scope_session_exists(&state, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }

            match state.options.set(
                request.scope.clone(),
                request.option,
                request.value,
                request.mode,
            ) {
                Ok(outcome) => {
                    alerts_changed = outcome
                        .notifications
                        .iter()
                        .any(|notification| notification.effects.affects_alerts());
                    state.refresh_transcript_limits_for_scope(&request.scope, request.option);
                    if let rmux_proto::ScopeSelector::Window(target) = &request.scope {
                        state.synchronize_linked_window_options_from_slot(
                            target.session_name(),
                            target.window_index(),
                        );
                    }
                    if request.option == OptionName::MessageLimit {
                        state.trim_message_log();
                    }
                    match resize_terminals_for_option_change(
                        &mut state,
                        request.option,
                        &request.scope,
                    ) {
                        Ok(()) => Response::SetOption(SetOptionResponse {
                            scope: request.scope,
                            option: request.option,
                            mode: request.mode,
                        }),
                        Err(error) => Response::Error(ErrorResponse { error }),
                    }
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SetOption(_)) {
            match &refresh_scope {
                Some(OptionScopeSelector::Session(session_name)) => {
                    self.refresh_attached_session(session_name).await;
                }
                Some(OptionScopeSelector::Window(target)) => {
                    self.refresh_attached_session(target.session_name()).await;
                }
                Some(OptionScopeSelector::Pane(target)) => {
                    self.refresh_attached_session(target.session_name()).await;
                }
                Some(
                    OptionScopeSelector::ServerGlobal
                    | OptionScopeSelector::SessionGlobal
                    | OptionScopeSelector::WindowGlobal,
                ) => {
                    self.refresh_all_attached_sessions().await;
                }
                None => {}
            }
            if alerts_changed {
                self.sync_alert_timers_for_option_scope(&alert_scope).await;
            }
            if let Some(scope) = automatic_rename_scope {
                self.refresh_automatic_names_after_option_change(scope)
                    .await;
            }
            if let Some(scope) = destroy_unattached_scope {
                self.destroy_unattached_sessions_for_option_scope(&scope)
                    .await;
            }
        }

        response
    }

    pub(super) async fn handle_set_option_by_name(
        &self,
        request: rmux_proto::SetOptionByNameRequest,
    ) -> Response {
        let refresh_scope = request.scope.clone();
        let mut alerts_changed = false;
        let mut destroy_unattached_scope = None;
        let response = {
            let mut state = self.state.lock().await;

            if let Err(error) = ensure_option_scope_exists(&state, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }
            let socket_path = self.socket_path();
            let value = match request.value.as_deref() {
                Some(value) if request.format && !request.unset => {
                    match format_option_value(
                        &state,
                        &request.scope,
                        request.format_target.as_ref(),
                        &socket_path,
                        value,
                    ) {
                        Ok(value) => Some(value),
                        Err(error) => return Response::Error(ErrorResponse { error }),
                    }
                }
                _ => request.value.clone(),
            };

            if let Err(error) = rmux_core::validate_option_name_mutation(
                &request.name,
                &request.scope,
                request.mode,
                value.as_deref(),
                request.unset,
            ) {
                return Response::Error(ErrorResponse { error });
            }

            match state.options.set_by_name(
                request.scope.clone(),
                &request.name,
                value,
                request.mode,
                request.only_if_unset,
                request.unset,
                request.unset_pane_overrides,
            ) {
                Ok(outcome) => {
                    alerts_changed = outcome
                        .notifications
                        .iter()
                        .any(|notification| notification.effects.affects_alerts());
                    if let Some(option) = outcome.known_option {
                        if option == OptionName::DestroyUnattached {
                            destroy_unattached_scope = Some(request.scope.clone());
                        }
                        if let Some(scope) = option_scope_to_legacy_scope(&request.scope) {
                            state.refresh_transcript_limits_for_scope(&scope, option);
                        }
                        if option == OptionName::AlternateScreen {
                            state.refresh_transcript_alternate_screen_for_option_scope(
                                &request.scope,
                            );
                        }
                        if option == OptionName::MessageLimit {
                            state.trim_message_log();
                        }
                    }
                    if let OptionScopeSelector::Window(target) = &request.scope {
                        state.synchronize_linked_window_options_from_slot(
                            target.session_name(),
                            target.window_index(),
                        );
                    }
                    match outcome
                        .known_option
                        .map(|option| {
                            resize_terminals_for_named_option_change(
                                &mut state,
                                option,
                                &request.scope,
                            )
                        })
                        .unwrap_or(Ok(()))
                    {
                        Ok(()) => Response::SetOptionByName(SetOptionByNameResponse {
                            scope: request.scope,
                            name: outcome.name,
                            mode: request.mode,
                        }),
                        Err(error) => Response::Error(ErrorResponse { error }),
                    }
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::SetOptionByName(_)) {
            match &refresh_scope {
                OptionScopeSelector::Session(session_name) => {
                    self.refresh_attached_session(session_name).await;
                }
                OptionScopeSelector::Window(target) => {
                    self.refresh_attached_session(target.session_name()).await;
                }
                OptionScopeSelector::Pane(target) => {
                    self.refresh_attached_session(target.session_name()).await;
                }
                OptionScopeSelector::ServerGlobal
                | OptionScopeSelector::SessionGlobal
                | OptionScopeSelector::WindowGlobal => {
                    self.refresh_all_attached_sessions().await;
                }
            }
            if alerts_changed {
                self.sync_alert_timers_for_option_scope(&refresh_scope)
                    .await;
            }
            if response_known_option_is_automatic_rename(&response) {
                self.refresh_automatic_names_after_option_change(refresh_scope)
                    .await;
            }
            if let Some(scope) = destroy_unattached_scope {
                self.destroy_unattached_sessions_for_option_scope(&scope)
                    .await;
            }
        }

        response
    }

    async fn refresh_automatic_names_after_option_change(&self, scope: OptionScopeSelector) {
        let targets = {
            let mut state = self.state.lock().await;
            automatic_rename_targets_for_scope(&mut state, &scope)
        };
        for target in targets {
            self.refresh_automatic_window_name_for_window_target(&target)
                .await;
        }
    }
}

fn response_known_option_is_automatic_rename(response: &Response) -> bool {
    matches!(
        response,
        Response::SetOptionByName(SetOptionByNameResponse { name, .. })
            if name == "automatic-rename"
    )
}

fn format_option_value(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
    target: Option<&rmux_proto::Target>,
    socket_path: &Path,
    value: &str,
) -> Result<String, rmux_proto::RmuxError> {
    let context = match target
        .cloned()
        .or_else(|| target_for_option_scope(state, scope))
    {
        Some(target) => super::scripting_support::format_context_for_target_with_server_values(
            state,
            &target,
            0,
            socket_path,
        )?,
        None => super::scripting_support::global_format_context(state, socket_path),
    };
    Ok(render_runtime_template(value, &context, false))
}

fn legacy_scope_to_refresh_scope(scope: &rmux_proto::ScopeSelector) -> OptionScopeSelector {
    match scope {
        rmux_proto::ScopeSelector::Global => OptionScopeSelector::SessionGlobal,
        rmux_proto::ScopeSelector::Session(session_name) => {
            OptionScopeSelector::Session(session_name.clone())
        }
        rmux_proto::ScopeSelector::Window(target) => OptionScopeSelector::Window(target.clone()),
        rmux_proto::ScopeSelector::Pane(target) => OptionScopeSelector::Pane(target.clone()),
    }
}

fn option_scope_to_legacy_scope(scope: &OptionScopeSelector) -> Option<rmux_proto::ScopeSelector> {
    match scope {
        OptionScopeSelector::ServerGlobal => Some(rmux_proto::ScopeSelector::Global),
        OptionScopeSelector::SessionGlobal => Some(rmux_proto::ScopeSelector::Global),
        OptionScopeSelector::WindowGlobal => None,
        OptionScopeSelector::Session(session_name) => {
            Some(rmux_proto::ScopeSelector::Session(session_name.clone()))
        }
        OptionScopeSelector::Window(target) => {
            Some(rmux_proto::ScopeSelector::Window(target.clone()))
        }
        OptionScopeSelector::Pane(target) => Some(rmux_proto::ScopeSelector::Pane(target.clone())),
    }
}

fn resize_terminals_for_option_change(
    state: &mut crate::pane_terminals::HandlerState,
    option: OptionName,
    scope: &ScopeSelector,
) -> Result<(), RmuxError> {
    if !option_affects_pane_terminal_geometry(option) {
        return Ok(());
    }

    let session_names = match scope {
        ScopeSelector::Global => all_session_names(state),
        ScopeSelector::Session(session_name) => vec![session_name.clone()],
        ScopeSelector::Window(target) => vec![target.session_name().clone()],
        ScopeSelector::Pane(target) => vec![target.session_name().clone()],
    };
    resize_terminals_for_sessions(state, session_names)
}

fn resize_terminals_for_named_option_change(
    state: &mut crate::pane_terminals::HandlerState,
    option: OptionName,
    scope: &OptionScopeSelector,
) -> Result<(), RmuxError> {
    if !option_affects_pane_terminal_geometry(option) {
        return Ok(());
    }

    let session_names = match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => all_session_names(state),
        OptionScopeSelector::Session(session_name) => vec![session_name.clone()],
        OptionScopeSelector::Window(target) => vec![target.session_name().clone()],
        OptionScopeSelector::Pane(target) => vec![target.session_name().clone()],
    };
    resize_terminals_for_sessions(state, session_names)
}

fn option_affects_pane_terminal_geometry(option: OptionName) -> bool {
    matches!(option, OptionName::PaneBorderStatus | OptionName::Status)
}

fn all_session_names(state: &crate::pane_terminals::HandlerState) -> Vec<SessionName> {
    state
        .sessions
        .iter()
        .map(|(session_name, _)| session_name.clone())
        .collect()
}

fn automatic_rename_targets_for_scope(
    state: &mut crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Vec<WindowTarget> {
    let targets = window_targets_for_option_scope(state, scope);
    for target in &targets {
        if state.options.resolve_for_window(
            target.session_name(),
            target.window_index(),
            OptionName::AutomaticRename,
        ) != Some("on")
        {
            continue;
        }
        if let Some(window) = state
            .sessions
            .session_mut(target.session_name())
            .and_then(|session| session.window_at_mut(target.window_index()))
        {
            window.enable_automatic_rename();
        }
    }
    targets
}

fn window_targets_for_option_scope(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Vec<WindowTarget> {
    match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => state
            .sessions
            .iter()
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .keys()
                    .map(|window_index| {
                        WindowTarget::with_window(session_name.clone(), *window_index)
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
        OptionScopeSelector::Session(session_name) => state
            .sessions
            .session(session_name)
            .map(|session| {
                session
                    .windows()
                    .keys()
                    .map(|window_index| {
                        WindowTarget::with_window(session_name.clone(), *window_index)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        OptionScopeSelector::Window(target) => vec![target.clone()],
        OptionScopeSelector::Pane(target) => vec![WindowTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
        )],
    }
}

fn resize_terminals_for_sessions(
    state: &mut crate::pane_terminals::HandlerState,
    session_names: Vec<SessionName>,
) -> Result<(), RmuxError> {
    for session_name in session_names {
        state.resize_terminals(&session_name)?;
    }
    Ok(())
}

pub(super) fn option_value_u32(
    options: &rmux_core::OptionStore,
    session_name: Option<&rmux_proto::SessionName>,
    option: rmux_proto::OptionName,
) -> u32 {
    options
        .resolve(session_name, option)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0)
}
