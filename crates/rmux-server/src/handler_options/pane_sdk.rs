use rmux_proto::{
    ErrorResponse, OptionName, OptionScopeSelector, PaneOptionGetResponse, PaneOptionSetResponse,
    Response,
};

use super::super::pane_support::resolve_pane_target_ref;
use super::super::RequestHandler;
use super::pane_state_events::{
    pane_id_for_resolved_target, pane_option_events_for_outcome,
    synchronize_pane_option_aliases_for_outcome,
};
use super::{option_scope_to_legacy_scope, resize_terminals_for_named_option_change};

impl RequestHandler {
    pub(in crate::handler) async fn handle_pane_option_set(
        &self,
        request: rmux_proto::PaneOptionSetRequest,
    ) -> Response {
        let mut refresh_session = None;
        let mut alert_scope = None;
        let mut alerts_changed = false;
        let response = {
            let mut state = self.state.lock().await;
            let target = match resolve_pane_target_ref(&state, &request.target) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let pane_id = match pane_id_for_resolved_target(&state, &target) {
                Ok(pane_id) => pane_id,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let scope = OptionScopeSelector::Pane(target.clone());
            let previous_options = state.options.clone();
            match state.options.set_by_name(
                scope.clone(),
                &request.name,
                request.value,
                request.mode,
                false,
                request.unset,
                false,
            ) {
                Ok(outcome) => {
                    if let Err(error) =
                        synchronize_pane_option_aliases_for_outcome(&mut state, &outcome)
                    {
                        state.options = previous_options;
                        return Response::Error(ErrorResponse { error });
                    }
                    let successful_response =
                        Response::PaneOptionSet(Box::new(PaneOptionSetResponse {
                            pane_id,
                            name: outcome.name.clone(),
                            old_value: outcome.old_explicit.clone(),
                            new_value: outcome.new_explicit.clone(),
                            changed: outcome.changed,
                        }));
                    if let Err(error) = rmux_proto::encode_frame(&successful_response) {
                        state.options = previous_options;
                        return Response::Error(ErrorResponse { error });
                    }
                    alerts_changed = outcome
                        .notifications
                        .iter()
                        .any(|notification| notification.effects.affects_alerts());
                    let pane_option_events = pane_option_events_for_outcome(&state, &outcome);
                    let mut resize_error = None;
                    if let Some(option) = outcome.known_option {
                        if let Some(scope) = option_scope_to_legacy_scope(&scope) {
                            state.refresh_transcript_limits_for_scope(&scope, option);
                        }
                        if option == OptionName::AlternateScreen {
                            state.refresh_transcript_alternate_screen_for_option_scope(&scope);
                        }
                        if option == OptionName::AllowSetTitle {
                            state.refresh_transcript_title_rename_for_option_scope(&scope);
                        }
                        if option == OptionName::MessageLimit {
                            state.trim_message_log();
                        }
                        if let Err(error) =
                            resize_terminals_for_named_option_change(&mut state, option, &scope)
                        {
                            resize_error = Some(error);
                        }
                    }
                    let response = if let Some(error) = resize_error {
                        Response::Error(ErrorResponse { error })
                    } else {
                        refresh_session = Some(target.session_name().clone());
                        alert_scope = Some(scope);
                        successful_response
                    };
                    self.pause_before_pane_option_journal().await;
                    for (pane_id, generation, outcome) in &pane_option_events {
                        self.record_pane_option_mutation(*pane_id, Some(*generation), outcome);
                    }
                    response
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            }
        };

        if matches!(response, Response::PaneOptionSet(_)) {
            if let Some(session_name) = refresh_session.as_ref() {
                self.refresh_attached_session(session_name).await;
            }
            if alerts_changed {
                if let Some(scope) = alert_scope.as_ref() {
                    self.sync_alert_timers_for_option_scope(scope).await;
                }
            }
        }

        response
    }

    pub(in crate::handler) async fn handle_pane_option_get(
        &self,
        request: rmux_proto::PaneOptionGetRequest,
    ) -> Response {
        let state = self.state.lock().await;
        let target = match resolve_pane_target_ref(&state, &request.target) {
            Ok(target) => target,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let pane_id = match pane_id_for_resolved_target(&state, &target) {
            Ok(pane_id) => pane_id,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        match state.pane_explicit_option_value_by_name(&target, &request.name) {
            Ok((name, value)) => Response::PaneOptionGet(PaneOptionGetResponse {
                pane_id,
                name,
                value,
            }),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }
}
