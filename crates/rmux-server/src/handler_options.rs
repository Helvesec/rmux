use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{ErrorResponse, OptionName, Response, SetOptionByNameResponse, SetOptionResponse};

use crate::handler_support::{ensure_option_scope_exists, ensure_scope_session_exists};

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
        let alert_scope = legacy_scope_to_refresh_scope(&request.scope);
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
                    Response::SetOption(SetOptionResponse {
                        scope: request.scope,
                        option: request.option,
                        mode: request.mode,
                    })
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
        }

        response
    }

    pub(super) async fn handle_set_option_by_name(
        &self,
        request: rmux_proto::SetOptionByNameRequest,
    ) -> Response {
        if let Err(error) = rmux_core::validate_option_name_mutation(
            &request.name,
            &request.scope,
            request.mode,
            request.value.as_deref(),
            request.unset,
        ) {
            return Response::Error(ErrorResponse { error });
        }

        let refresh_scope = request.scope.clone();
        let mut alerts_changed = false;
        let response = {
            let mut state = self.state.lock().await;

            if let Err(error) = ensure_option_scope_exists(&state, &request.scope) {
                return Response::Error(ErrorResponse { error });
            }

            match state.options.set_by_name(
                request.scope.clone(),
                &request.name,
                request.value,
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
                        if let Some(scope) = option_scope_to_legacy_scope(&request.scope) {
                            state.refresh_transcript_limits_for_scope(&scope, option);
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
                    Response::SetOptionByName(SetOptionByNameResponse {
                        scope: request.scope,
                        name: outcome.name,
                        mode: request.mode,
                    })
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
        }

        response
    }
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
