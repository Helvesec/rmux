//! Finishing a prompt: mode-tree dismissal, client refresh and command dispatch.

use rmux_proto::{RmuxError, SessionId, SessionName};
use tracing::warn;

use super::super::super::attach_support::ActiveAttachIdentity;
use super::super::super::scripting_support::command_parser_from_state;
use super::super::super::{with_expected_attach_and_session_identity, RequestHandler};
use super::super::substitution::substitute_prompt_template;
use super::super::{
    prompt_accept_should_dismiss_mode_tree, FinishedPrompt, FinishedPromptKind, PromptCompletion,
    PromptDispatch, PromptQueueResult,
};
use super::PromptAttachSnapshot;
use crate::pane_io::AttachControl;

impl RequestHandler {
    pub(super) async fn finish_prompt(
        &self,
        finished: FinishedPrompt,
        snapshot: Option<PromptAttachSnapshot>,
    ) {
        if let Some((identity, session_name, session_id)) = snapshot.as_ref() {
            if prompt_accept_should_dismiss_mode_tree(&finished) {
                self.dismiss_mode_tree_for_prompt_session_identity(
                    *identity,
                    session_name,
                    *session_id,
                )
                .await;
            }
            if matches!(&finished.kind, FinishedPromptKind::Cancel) {
                self.refresh_attached_client_for_session_identity(
                    *identity,
                    session_name,
                    *session_id,
                )
                .await;
            } else {
                self.refresh_attached_client_base_for_session_identity(
                    *identity,
                    session_name,
                    *session_id,
                )
                .await;
            }
        }

        match finished.kind {
            FinishedPromptKind::Cancel => {
                if let PromptCompletion::Foreground(sender) = finished.completion {
                    let _ = sender.send(PromptQueueResult::cancelled(finished.origin));
                }
            }
            FinishedPromptKind::Command {
                template,
                format_values,
                responses,
            } => {
                let parsed = self
                    .parse_prompt_commands(&template, &format_values, &responses)
                    .await;
                match finished.completion {
                    PromptCompletion::Foreground(sender) => {
                        let _ = sender.send(match parsed {
                            Ok(parsed) => PromptQueueResult {
                                inserted: Some((parsed, finished.context)),
                                error: None,
                                responses: Some(responses),
                                origin: Some(finished.origin),
                            },
                            Err(error) => PromptQueueResult {
                                inserted: None,
                                error: Some(error),
                                responses: Some(responses),
                                origin: Some(finished.origin),
                            },
                        });
                    }
                    PromptCompletion::Background => match parsed {
                        Ok(parsed) => {
                            let handler = self.clone();
                            let origin = finished.origin;
                            let requester_pid = origin.requester_pid();
                            let context = finished.context;
                            let execution_snapshot = snapshot
                                .filter(|(identity, _, _)| identity.attach_pid() == requester_pid);
                            let _ = self.spawn_background_task(
                                "rmux-prompt-finish",
                                move || async move {
                                    let _access = handler.begin_requester_origin_access(&origin);
                                    let execution = handler.execute_parsed_commands(
                                        requester_pid,
                                        parsed,
                                        context,
                                    );
                                    let _ = match execution_snapshot {
                                        Some((identity, session_name, session_id)) => {
                                            with_expected_attach_and_session_identity(
                                                identity,
                                                session_name,
                                                session_id,
                                                execution,
                                            )
                                            .await
                                        }
                                        None => execution.await,
                                    };
                                },
                            );
                        }
                        Err(error) => {
                            warn!("background prompt command failed to parse: {error}");
                        }
                    },
                }
            }
        }
    }

    async fn dismiss_mode_tree_for_prompt_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &SessionName,
        session_id: SessionId,
    ) {
        let mut active_attach = self.active_attach.lock().await;
        let subject_current = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active_session(active, session_name, session_id)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            });
        if !subject_current {
            return;
        }
        for active in active_attach.by_pid.values_mut() {
            if &active.session_name != session_name
                || active.session_id != session_id
                || active.suspended
                || active.mode_tree.is_none()
            {
                continue;
            }
            active.mode_tree = None;
            active.mode_tree_frame = None;
            active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
            active.persistent_overlay_epoch.store(
                active.mode_tree_state_id,
                std::sync::atomic::Ordering::SeqCst,
            );
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let _ = active
                .control_tx
                .send(AttachControl::AdvancePersistentOverlayState(
                    active.mode_tree_state_id,
                ));
        }
    }

    pub(super) async fn dispatch_prompt_commands(
        &self,
        dispatch: PromptDispatch,
        snapshot: Option<PromptAttachSnapshot>,
    ) {
        let parsed = self
            .parse_prompt_commands(
                &dispatch.template,
                &dispatch.format_values,
                &dispatch.responses,
            )
            .await;
        match parsed {
            Ok(parsed) => {
                let handler = self.clone();
                let requester_pid = dispatch.origin.requester_pid();
                let execution_snapshot =
                    snapshot.filter(|(identity, _, _)| identity.attach_pid() == requester_pid);
                let _ = self.spawn_background_task("rmux-prompt-dispatch", move || async move {
                    let _access = handler.begin_requester_origin_access(&dispatch.origin);
                    let execution =
                        handler.execute_parsed_commands(requester_pid, parsed, dispatch.context);
                    let _ = match execution_snapshot {
                        Some((identity, session_name, session_id)) => {
                            with_expected_attach_and_session_identity(
                                identity,
                                session_name,
                                session_id,
                                execution,
                            )
                            .await
                        }
                        None => execution.await,
                    };
                });
            }
            Err(error) => warn!("prompt command failed to parse: {error}"),
        }
    }

    async fn parse_prompt_commands(
        &self,
        template: &str,
        format_values: &[(String, String)],
        responses: &[String],
    ) -> Result<rmux_core::command_parser::ParsedCommands, RmuxError> {
        let substituted = substitute_prompt_template(template, responses);
        let state = self.state.lock().await;
        let mut parser = command_parser_from_state(&state);
        for (name, value) in format_values {
            parser = parser.with_format_value(name, value.clone());
        }
        parser
            .parse_one_group(&substituted)
            .map_err(|error| RmuxError::Server(error.to_string()))
    }
}
