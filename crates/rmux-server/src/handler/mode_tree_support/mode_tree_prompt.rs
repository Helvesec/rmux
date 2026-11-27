use rmux_core::{command_parser::CommandParser, KeyCode};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{BindKeyRequest, Response, RmuxError, SetOptionByNameRequest, SetOptionMode};
use tokio::sync::oneshot;

use super::super::prompt_support::{
    substitute_prompt_template, CommandPromptPlan, ConfirmBeforePlan, PromptField,
    PromptQueueResult, PromptStartOutcome, PromptType,
};
use super::super::scripting_support::QueueExecutionContext;
use super::super::{RequestHandler, RequesterOrigin};
use super::mode_tree_model::{
    ModeTreeActionIdentity, ModeTreeDeferredAction, ModeTreePromptCallback, SearchDirection,
    SearchState,
};
use super::mode_tree_selection::{repeat_search, selected_items};
use super::SAFE_PROMPT_TEMPLATE;

impl RequestHandler {
    pub(super) async fn start_mode_tree_prompt_for_action_identity(
        &self,
        action_identity: ModeTreeActionIdentity,
        callback: ModeTreePromptCallback,
    ) -> Result<(), RmuxError> {
        let attach_pid = action_identity.attach_pid();
        let _ = self.mode_tree_for_action_identity(action_identity).await?;
        let (mode, identity) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == action_identity.attach_id()
                        && active.mode_tree_state_id == action_identity.state_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            (
                active
                    .mode_tree
                    .clone()
                    .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?,
                active.identity(attach_pid),
            )
        };
        let (prompt, initial, prompt_type) = match &callback {
            ModeTreePromptCallback::Filter => (
                "(filter) ".to_owned(),
                mode.filter_text.clone().unwrap_or_default(),
                PromptType::Search,
            ),
            ModeTreePromptCallback::Search(direction) => (
                match direction {
                    SearchDirection::Forward => "(search down) ".to_owned(),
                    SearchDirection::Backward => "(search up) ".to_owned(),
                },
                mode.search
                    .as_ref()
                    .map(|search| search.value.clone())
                    .unwrap_or_default(),
                PromptType::Search,
            ),
            ModeTreePromptCallback::Command => (":".to_owned(), String::new(), PromptType::Command),
            ModeTreePromptCallback::CustomizeSetOption { .. } => {
                ("value ".to_owned(), String::new(), PromptType::Command)
            }
            ModeTreePromptCallback::CustomizeSetKey { .. } => {
                ("command ".to_owned(), String::new(), PromptType::Command)
            }
        };
        let plan = CommandPromptPlan {
            origin: mode.origin.clone(),
            target_client: None,
            context: QueueExecutionContext::without_caller_cwd(),
            fields: vec![PromptField {
                prompt,
                input: initial,
            }],
            template: SAFE_PROMPT_TEMPLATE.to_owned(),
            flags: 0,
            prompt_type,
            background: false,
            format_values: Vec::new(),
        };
        let outcome = self
            .start_command_prompt_for_identity(identity, plan)
            .await?;
        if let PromptStartOutcome::Waiting(rx) = outcome {
            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .await_mode_tree_prompt(action_identity, callback, rx)
                    .await;
            });
        }
        Ok(())
    }

    async fn await_mode_tree_prompt(
        &self,
        identity: ModeTreeActionIdentity,
        callback: ModeTreePromptCallback,
        rx: oneshot::Receiver<PromptQueueResult>,
    ) {
        let Ok(result) = rx.await else {
            return;
        };
        let Some(origin) = result.origin else {
            return;
        };
        let _access = self.begin_requester_origin_access(&origin);
        let Some(value) = result
            .responses
            .as_ref()
            .and_then(|responses| responses.first())
            .cloned()
        else {
            return;
        };
        let output = value.trim().to_owned();
        match callback {
            ModeTreePromptCallback::Filter => {
                let _ = self.apply_mode_tree_filter(identity, output).await;
            }
            ModeTreePromptCallback::Search(direction) => {
                let _ = self
                    .apply_mode_tree_search(identity, output, direction)
                    .await;
            }
            ModeTreePromptCallback::Command => {
                let _ = self
                    .apply_mode_tree_command(identity, &origin, output)
                    .await;
            }
            ModeTreePromptCallback::CustomizeSetOption { scope, name } => {
                let _ = self
                    .apply_customize_option_value(identity, &origin, scope, name, output)
                    .await;
            }
            ModeTreePromptCallback::CustomizeSetKey { table_name, key } => {
                let _ = self
                    .apply_customize_key_value(identity, &origin, table_name, key, output)
                    .await;
            }
        }
    }

    pub(super) async fn confirm_mode_tree_action_for_identity(
        &self,
        action_identity: ModeTreeActionIdentity,
        prompt: String,
        action: ModeTreeDeferredAction,
    ) -> Result<(), RmuxError> {
        let attach_pid = action_identity.attach_pid();
        let _ = self.mode_tree_for_action_identity(action_identity).await?;
        let (auto_accept, origin, identity) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == action_identity.attach_id()
                        && active.mode_tree_state_id == action_identity.state_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let mode = active
                .mode_tree
                .as_ref()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?;
            (
                mode.auto_accept,
                mode.origin.clone(),
                active.identity(attach_pid),
            )
        };
        if auto_accept {
            return self
                .execute_mode_tree_deferred_action(action_identity, &origin, action)
                .await;
        }

        let plan = ConfirmBeforePlan {
            origin: origin.clone(),
            target_client: None,
            context: QueueExecutionContext::without_caller_cwd(),
            prompt,
            template: SAFE_PROMPT_TEMPLATE.to_owned(),
            confirm_key: 'y',
            default_yes: false,
            background: false,
            format_values: Vec::new(),
        };
        let outcome = self
            .start_confirm_before_for_identity(identity, plan)
            .await?;
        if let PromptStartOutcome::Waiting(rx) = outcome {
            let handler = self.clone();
            tokio::spawn(async move {
                let Ok(result) = rx.await else {
                    return;
                };
                let Some(origin) = result.origin else {
                    return;
                };
                if result.inserted.is_some() {
                    let _ = handler
                        .execute_mode_tree_deferred_action(action_identity, &origin, action)
                        .await;
                }
            });
        }
        Ok(())
    }

    pub(super) async fn execute_mode_tree_deferred_action(
        &self,
        identity: ModeTreeActionIdentity,
        origin: &RequesterOrigin,
        action: ModeTreeDeferredAction,
    ) -> Result<(), RmuxError> {
        #[cfg(test)]
        super::mode_tree_test_support::pause_mode_tree_identity(
            super::mode_tree_test_support::ModeTreeIdentityPausePoint::DeferredAction(
                identity.attach_pid(),
            ),
        )
        .await;
        let _ = self.mode_tree_for_action_identity(identity).await?;
        let _access = self.require_requester_origin_write(origin).await?;
        match action {
            ModeTreeDeferredAction::DeleteBuffers { targets } => {
                self.perform_buffer_delete_actions_for_identity(identity, targets)
                    .await
            }
            ModeTreeDeferredAction::DetachClients { targets } => {
                self.perform_client_detach_actions_for_identity(identity, targets)
                    .await
            }
            ModeTreeDeferredAction::KillCurrentTreeSelection { targets } => {
                self.perform_tree_kill_actions_for_identity(identity, targets)
                    .await
            }
            ModeTreeDeferredAction::KillTaggedTreeSelections { targets } => {
                self.perform_tree_kill_tagged_actions_for_identity(identity, targets)
                    .await
            }
        }
    }

    pub(super) async fn apply_mode_tree_filter(
        &self,
        identity: ModeTreeActionIdentity,
        value: String,
    ) -> Result<(), RmuxError> {
        let state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                active.id == identity.attach_id()
                    && active.mode_tree_state_id == identity.state_id()
                    && active.mode_tree.as_ref().is_some_and(|mode| {
                        super::mode_tree_runtime_identity::mode_tree_host_is_current(&state, mode)
                    })
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        let Some(mode) = active.mode_tree.as_mut() else {
            return Ok(());
        };
        mode.filter_text = (!value.is_empty()).then_some(value);
        mode.scroll = 0;
        mode.preview_scroll = 0;
        drop(active_attach);
        drop(state);
        self.refresh_mode_tree_overlay_for_action_identity(identity)
            .await
    }

    async fn apply_mode_tree_search(
        &self,
        identity: ModeTreeActionIdentity,
        value: String,
        direction: SearchDirection,
    ) -> Result<(), RmuxError> {
        let attach_pid = identity.attach_pid();
        let mut mode = self.mode_tree_for_action_identity(identity).await?;
        mode.search = (!value.is_empty()).then_some(SearchState { value, direction });
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        repeat_search(&mut mode, &build, false);
        let identity = self
            .store_mode_tree_state_for_action_identity(identity, mode)
            .await?;
        self.refresh_mode_tree_overlay_for_action_identity(identity)
            .await
    }

    pub(super) async fn apply_mode_tree_command(
        &self,
        identity: ModeTreeActionIdentity,
        origin: &RequesterOrigin,
        command: String,
    ) -> Result<(), RmuxError> {
        if command.trim().is_empty() {
            return Ok(());
        }
        let attach_pid = identity.attach_pid();
        let mut mode = self.mode_tree_for_action_identity(identity).await?;
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let targets = selected_items(&mode, &build)
            .into_iter()
            .filter_map(|item| item.action.target_string())
            .collect::<Vec<_>>();
        let refresh_sessions = self.dismiss_mode_tree_for_action_identity(identity).await?;
        let requester_pid = origin.requester_pid();
        if targets.is_empty() {
            let parsed = CommandParser::new()
                .parse_one_group(&command)
                .map_err(|error| RmuxError::Server(error.message().to_owned()))?;
            let context = QueueExecutionContext::without_caller_cwd();
            let _ = self
                .execute_parsed_commands(requester_pid, parsed, context)
                .await?;
        } else {
            for target in targets {
                let substituted = substitute_prompt_template(&command, &[target]);
                let parsed = CommandParser::new()
                    .parse_one_group(&substituted)
                    .map_err(|error| RmuxError::Server(error.message().to_owned()))?;
                let context = QueueExecutionContext::without_caller_cwd();
                let _ = self
                    .execute_parsed_commands(requester_pid, parsed, context)
                    .await?;
            }
        }
        for session_name in refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        Ok(())
    }

    async fn apply_customize_option_value(
        &self,
        identity: ModeTreeActionIdentity,
        origin: &RequesterOrigin,
        scope: OptionScopeSelector,
        name: String,
        value: String,
    ) -> Result<(), RmuxError> {
        let _ = self.mode_tree_for_action_identity(identity).await?;
        let _access = self.require_requester_origin_write(origin).await?;
        #[cfg(test)]
        super::mode_tree_test_support::pause_mode_tree_identity(
            super::mode_tree_test_support::ModeTreeIdentityPausePoint::Mutation(
                identity.attach_pid(),
            ),
        )
        .await;
        let response = self
            .handle_set_option_by_name_for_mode_tree(
                SetOptionByNameRequest {
                    scope,
                    name,
                    value: Some(value),
                    mode: SetOptionMode::Replace,
                    only_if_unset: false,
                    unset: false,
                    unset_pane_overrides: false,
                    format: false,
                    format_target: None,
                },
                identity,
            )
            .await;
        if let Response::Error(error) = response {
            return Err(error.error);
        }
        self.refresh_mode_tree_overlay_for_action_identity(identity)
            .await
    }

    async fn apply_customize_key_value(
        &self,
        identity: ModeTreeActionIdentity,
        origin: &RequesterOrigin,
        table_name: String,
        key: KeyCode,
        value: String,
    ) -> Result<(), RmuxError> {
        let _ = self.mode_tree_for_action_identity(identity).await?;
        let _access = self.require_requester_origin_write(origin).await?;
        let parsed = CommandParser::new()
            .parse_one_group(&value)
            .map_err(|error| RmuxError::Server(error.message().to_owned()))?;
        #[cfg(test)]
        super::mode_tree_test_support::pause_mode_tree_identity(
            super::mode_tree_test_support::ModeTreeIdentityPausePoint::Mutation(
                identity.attach_pid(),
            ),
        )
        .await;
        let response = self
            .handle_bind_key_for_mode_tree(
                BindKeyRequest {
                    table_name,
                    key: rmux_core::key_string_lookup_key(key, false),
                    note: None,
                    repeat: false,
                    command: Some(vec![parsed.to_tmux_reparse_string()]),
                },
                identity,
            )
            .await;
        if let Response::Error(error) = response {
            return Err(error.error);
        }
        self.refresh_mode_tree_overlay_for_action_identity(identity)
            .await
    }
}
