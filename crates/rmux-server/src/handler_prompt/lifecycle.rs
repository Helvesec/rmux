use rmux_proto::{OptionName, RmuxError, SessionId, SessionName};
use tokio::sync::oneshot;
use tracing::warn;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::control_support::ManagedClient;
use super::super::scripting_support::{
    command_parser_from_state, ParsedPromptHistoryCommand, PromptHistoryAction, QueueCommandAction,
};
use super::super::{with_expected_attach_and_session_identity, RequestHandler};
use super::events::process_prompt_event;
use super::substitution::substitute_prompt_template;
use super::{
    prompt_accept_should_dismiss_mode_tree, ClientPromptState, CommandPromptPlan,
    ConfirmBeforePlan, FinishedPrompt, FinishedPromptKind, PromptCompletion, PromptDispatch,
    PromptFinalizeKind, PromptInputEvent, PromptQueueResult, PromptStartOutcome, PromptType,
};
use crate::pane_io::{AttachControl, OverlayFrame};
#[cfg(test)]
use crate::renderer::RenderedPrompt;

type PromptAttachSnapshot = (ActiveAttachIdentity, SessionName, SessionId);

impl RequestHandler {
    async fn prompt_attach_snapshot(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
    ) -> Result<PromptAttachSnapshot, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| {
                expected_attach_id.is_none_or(|attach_id| active.id == attach_id)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        Ok((
            active.identity(attach_pid),
            active.session_name.clone(),
            active.session_id,
        ))
    }

    pub(in crate::handler) async fn start_command_prompt(
        &self,
        plan: CommandPromptPlan,
    ) -> Result<PromptStartOutcome, RmuxError> {
        let managed = match self
            .resolve_target_managed_client(
                plan.origin.requester_pid(),
                plan.target_client.as_deref(),
                "command-prompt",
            )
            .await
        {
            Ok(managed) => managed,
            Err(RmuxError::Server(message))
                if message == "command-prompt requires an attached client" =>
            {
                return Err(RmuxError::Message("no current client".to_owned()));
            }
            Err(RmuxError::Server(message)) if message.starts_with("can't find client: ") => {
                return Err(RmuxError::Message(message));
            }
            Err(error) => return Err(error),
        };
        let (attach_pid, attach_id) = match managed {
            ManagedClient::Attach { pid, attach_id } => (pid, attach_id),
            ManagedClient::Control(_) => {
                return Ok(PromptStartOutcome::Immediate);
            }
        };

        self.start_command_prompt_for_attach_identity(plan, attach_pid, attach_id)
            .await
    }

    pub(in crate::handler) async fn start_command_prompt_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        plan: CommandPromptPlan,
    ) -> Result<PromptStartOutcome, RmuxError> {
        self.start_command_prompt_for_attach_identity(
            plan,
            identity.attach_pid(),
            identity.attach_id(),
        )
        .await
    }

    async fn start_command_prompt_for_attach_identity(
        &self,
        plan: CommandPromptPlan,
        attach_pid: u32,
        attach_id: u64,
    ) -> Result<PromptStartOutcome, RmuxError> {
        let (prompt, outcome) = if plan.background {
            (
                ClientPromptState::new_command(plan, PromptCompletion::Background),
                PromptStartOutcome::Immediate,
            )
        } else {
            let (tx, rx) = oneshot::channel();
            (
                ClientPromptState::new_command(plan, PromptCompletion::Foreground(tx)),
                PromptStartOutcome::Waiting(rx),
            )
        };
        let initial_dispatch = prompt.initial_incremental_dispatch();
        let installed = self.install_prompt(attach_pid, attach_id, prompt).await?;
        if !installed {
            return Ok(PromptStartOutcome::Immediate);
        }
        if let Some(dispatch) = initial_dispatch {
            let snapshot = self
                .prompt_attach_snapshot(attach_pid, Some(attach_id))
                .await
                .ok();
            self.dispatch_prompt_commands(dispatch, snapshot).await;
        }
        Ok(outcome)
    }

    pub(in crate::handler) async fn start_confirm_before(
        &self,
        plan: ConfirmBeforePlan,
    ) -> Result<PromptStartOutcome, RmuxError> {
        let managed = match self
            .resolve_target_managed_client(
                plan.origin.requester_pid(),
                plan.target_client.as_deref(),
                "confirm-before",
            )
            .await
        {
            Ok(managed) => managed,
            Err(RmuxError::Server(message))
                if message == "confirm-before requires an attached client" =>
            {
                return Err(RmuxError::Message("no current client".to_owned()));
            }
            Err(RmuxError::Server(message)) if message.starts_with("can't find client: ") => {
                return Err(RmuxError::Message(message));
            }
            Err(error) => return Err(error),
        };
        let (attach_pid, attach_id) = match managed {
            ManagedClient::Attach { pid, attach_id } => (pid, attach_id),
            ManagedClient::Control(_) => {
                return Ok(PromptStartOutcome::Immediate);
            }
        };

        self.start_confirm_before_for_attach_identity(plan, attach_pid, attach_id)
            .await
    }

    pub(in crate::handler) async fn start_confirm_before_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        plan: ConfirmBeforePlan,
    ) -> Result<PromptStartOutcome, RmuxError> {
        self.start_confirm_before_for_attach_identity(
            plan,
            identity.attach_pid(),
            identity.attach_id(),
        )
        .await
    }

    async fn start_confirm_before_for_attach_identity(
        &self,
        plan: ConfirmBeforePlan,
        attach_pid: u32,
        attach_id: u64,
    ) -> Result<PromptStartOutcome, RmuxError> {
        let (prompt, outcome) = if plan.background {
            (
                ClientPromptState::new_confirm(plan, PromptCompletion::Background),
                PromptStartOutcome::Immediate,
            )
        } else {
            let (tx, rx) = oneshot::channel();
            (
                ClientPromptState::new_confirm(plan, PromptCompletion::Foreground(tx)),
                PromptStartOutcome::Waiting(rx),
            )
        };
        if !self.install_prompt(attach_pid, attach_id, prompt).await? {
            return Ok(PromptStartOutcome::Immediate);
        }
        Ok(outcome)
    }

    async fn install_prompt(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        prompt: ClientPromptState,
    ) -> Result<bool, RmuxError> {
        let (identity, session_name, session_id, control_tx, render_generation, overlay_generation) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    active.id == expected_attach_id
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if active.prompt.is_some() {
                return Ok(false);
            }
            active.prompt = Some(prompt);
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            (
                active.identity(attach_pid),
                active.session_name.clone(),
                active.session_id,
                active.control_tx.clone(),
                active.render_generation,
                active.overlay_generation,
            )
        };

        let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::new(
            Vec::new(),
            render_generation,
            overlay_generation,
        )));
        self.refresh_attached_client_base_for_session_identity(identity, &session_name, session_id)
            .await;
        Ok(true)
    }

    #[cfg(test)]
    pub(in crate::handler) async fn attached_prompt_render(
        &self,
        attach_pid: u32,
    ) -> Option<RenderedPrompt> {
        let active_attach = self.active_attach.lock().await;
        active_attach.by_pid.get(&attach_pid).and_then(|active| {
            active
                .prompt
                .as_ref()
                .map(ClientPromptState::rendered_prompt)
        })
    }

    #[cfg(test)]
    pub(in crate::handler) async fn prompt_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.prompt.is_some())
    }

    pub(in crate::handler) async fn prompt_active_for_identity(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
    ) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.prompt.is_some()
            })
    }

    pub(in crate::handler) async fn handle_prompt_event_deferred_refresh_for_identity(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
        event: PromptInputEvent,
        deferred_refresh: &mut bool,
    ) -> Result<(), RmuxError> {
        self.handle_prompt_event_deferred_refresh_with_identity(
            identity.attach_pid(),
            Some(identity),
            event,
            deferred_refresh,
        )
        .await
    }

    async fn handle_prompt_event_deferred_refresh_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<super::super::attach_support::ActiveAttachIdentity>,
        event: PromptInputEvent,
        deferred_refresh: &mut bool,
    ) -> Result<(), RmuxError> {
        let (prompt_identity, session_name, session_id) = self
            .prompt_attach_snapshot(attach_pid, identity.map(ActiveAttachIdentity::attach_id))
            .await?;
        let separators = {
            let state = self.state.lock().await;
            if state
                .sessions
                .session(&session_name)
                .is_none_or(|session| session.id() != session_id)
            {
                return Ok(());
            }
            state
                .options
                .resolve(Some(&session_name), OptionName::WordSeparators)
                .unwrap_or_default()
                .to_owned()
        };
        let history_limit = {
            let state = self.state.lock().await;
            state
                .options
                .resolve(None, OptionName::PromptHistoryLimit)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(100)
        };

        let (action, finished) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    prompt_identity.matches_active_session(active, &session_name, session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(prompt) = active.prompt.as_mut() else {
                return Ok(());
            };
            let mut history = self.prompt_history.lock().await;
            let action =
                process_prompt_event(prompt, event, &mut history, &separators, history_limit);
            let finished = action.finalize.as_ref().map(|kind| {
                active
                    .prompt
                    .take()
                    .expect("prompt exists")
                    .into_finished(kind.clone())
            });
            (action, finished)
        };

        if action.refresh && finished.is_none() {
            *deferred_refresh = true;
        }
        if let Some(dispatch) = action.dispatch {
            self.dispatch_prompt_commands(
                dispatch,
                Some((prompt_identity, session_name.clone(), session_id)),
            )
            .await;
        }
        if let Some(finished) = finished {
            self.finish_prompt(finished, Some((prompt_identity, session_name, session_id)))
                .await;
        }

        Ok(())
    }

    pub(in crate::handler) async fn try_handle_prompt_text_deferred_refresh_for_identity(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
        text: &str,
        deferred_refresh: &mut bool,
    ) -> Result<bool, RmuxError> {
        self.try_handle_prompt_text_deferred_refresh_with_identity(
            identity.attach_pid(),
            Some(identity),
            text,
            deferred_refresh,
        )
        .await
    }

    async fn try_handle_prompt_text_deferred_refresh_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<super::super::attach_support::ActiveAttachIdentity>,
        text: &str,
        deferred_refresh: &mut bool,
    ) -> Result<bool, RmuxError> {
        let (prompt_identity, session_name, session_id) = self
            .prompt_attach_snapshot(attach_pid, identity.map(ActiveAttachIdentity::attach_id))
            .await?;
        let inserted = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    prompt_identity.matches_active_session(active, &session_name, session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active
                .prompt
                .as_mut()
                .is_some_and(|prompt| prompt.insert_batched_text(text))
        };
        if inserted {
            *deferred_refresh = true;
        }
        Ok(inserted)
    }

    pub(in crate::handler) async fn flush_attached_prompt_refresh_for_identity(
        &self,
        identity: super::super::attach_support::ActiveAttachIdentity,
    ) -> Result<(), RmuxError> {
        if !self.prompt_active_for_identity(identity).await {
            return Ok(());
        }
        let (session_name, session_id) = self
            .attached_session_identity_for_identity(identity)
            .await?;
        self.refresh_attached_client_base_for_session_identity(identity, &session_name, session_id)
            .await;
        Ok(())
    }

    pub(in crate::handler) async fn clear_prompt_for_attach(&self, attach_pid: u32) {
        self.clear_prompt_for_attach_with_expected_identity(attach_pid, None)
            .await;
    }

    pub(in crate::handler) async fn clear_prompt_for_attach_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
    ) {
        self.clear_prompt_for_attach_with_expected_identity(attach_pid, Some(expected_attach_id))
            .await;
    }

    async fn clear_prompt_for_attach_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
    ) {
        let finished = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| expected_attach_id.is_none_or(|expected| active.id == expected))
                .filter(|active| !active.closing.load(std::sync::atomic::Ordering::SeqCst));
            active.and_then(|active| {
                let snapshot = (
                    active.identity(attach_pid),
                    active.session_name.clone(),
                    active.session_id,
                );
                active
                    .prompt
                    .take()
                    .map(|prompt| (prompt.into_finished(PromptFinalizeKind::Cancel), snapshot))
            })
        };
        if let Some((finished, snapshot)) = finished {
            self.finish_prompt(finished, Some(snapshot)).await;
        }
    }

    async fn finish_prompt(
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

    /// Runs `show-prompt-history`, returning the rendered tmux-compatible history body.
    pub(super) async fn show_prompt_history(
        &self,
        selected: Option<PromptType>,
    ) -> Result<String, RmuxError> {
        let history = self.prompt_history.lock().await;
        Ok(history.render(selected))
    }

    /// Runs `clear-prompt-history`, dropping entries for the selected type or all types.
    pub(super) async fn clear_prompt_history(
        &self,
        selected: Option<PromptType>,
    ) -> Result<(), RmuxError> {
        let mut history = self.prompt_history.lock().await;
        history.clear(selected);
        Ok(())
    }

    /// Routes a parsed prompt-history queue command to the right store operation.
    pub(in crate::handler) async fn execute_queued_prompt_history(
        &self,
        command: ParsedPromptHistoryCommand,
    ) -> Result<QueueCommandAction, RmuxError> {
        match command.action {
            PromptHistoryAction::Show => {
                let body = self.show_prompt_history(command.prompt_type).await?;
                Ok(QueueCommandAction::Normal {
                    output: Some(rmux_proto::CommandOutput::from_stdout(body.into_bytes())),
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                })
            }
            PromptHistoryAction::Clear => {
                self.clear_prompt_history(command.prompt_type).await?;
                Ok(QueueCommandAction::Normal {
                    output: None,
                    error: None,
                    source_file_error: None,
                    exit_status: None,
                })
            }
        }
    }

    async fn dispatch_prompt_commands(
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
