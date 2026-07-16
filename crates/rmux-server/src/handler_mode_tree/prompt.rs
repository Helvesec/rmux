use rmux_core::{command_parser::CommandParser, KeyCode};
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{BindKeyRequest, Response, RmuxError, SetOptionByNameRequest, SetOptionMode};
use tokio::sync::oneshot;

use super::super::prompt_support::{
    substitute_prompt_template, CommandPromptPlan, ConfirmBeforePlan, PromptField,
    PromptQueueResult, PromptStartOutcome, PromptType,
};
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use super::mode_tree_model::{
    ModeTreeDeferredAction, ModeTreePromptCallback, SearchDirection, SearchState,
};
use super::mode_tree_selection::{repeat_search, selected_items};
use super::SAFE_PROMPT_TEMPLATE;

impl RequestHandler {
    pub(super) async fn start_mode_tree_prompt(
        &self,
        attach_pid: u32,
        callback: ModeTreePromptCallback,
    ) -> Result<(), RmuxError> {
        let mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?
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
            requester_pid: attach_pid,
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
        let outcome = self.start_command_prompt(plan).await?;
        if let PromptStartOutcome::Waiting(rx) = outcome {
            let handler = self.clone();
            tokio::spawn(async move {
                handler
                    .await_mode_tree_prompt(attach_pid, callback, rx)
                    .await;
            });
        }
        Ok(())
    }

    async fn await_mode_tree_prompt(
        &self,
        attach_pid: u32,
        callback: ModeTreePromptCallback,
        rx: oneshot::Receiver<PromptQueueResult>,
    ) {
        let Ok(result) = rx.await else {
            return;
        };
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
                let _ = self.apply_mode_tree_filter(attach_pid, output).await;
            }
            ModeTreePromptCallback::Search(direction) => {
                let _ = self
                    .apply_mode_tree_search(attach_pid, output, direction)
                    .await;
            }
            ModeTreePromptCallback::Command => {
                let _ = self.apply_mode_tree_command(attach_pid, output).await;
            }
            ModeTreePromptCallback::CustomizeSetOption { scope, name } => {
                let _ = self
                    .apply_customize_option_value(attach_pid, scope, name, output)
                    .await;
            }
            ModeTreePromptCallback::CustomizeSetKey { table_name, key } => {
                let _ = self
                    .apply_customize_key_value(attach_pid, table_name, key, output)
                    .await;
            }
        }
    }

    pub(super) async fn confirm_mode_tree_action(
        &self,
        attach_pid: u32,
        prompt: String,
        action: ModeTreeDeferredAction,
    ) -> Result<(), RmuxError> {
        let auto_accept = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .and_then(|active| active.mode_tree.as_ref())
                .is_some_and(|mode| mode.auto_accept)
        };
        if auto_accept {
            return match action {
                ModeTreeDeferredAction::DeleteBuffers => {
                    self.perform_buffer_delete(attach_pid).await
                }
                ModeTreeDeferredAction::DetachClients => {
                    self.perform_client_detach(attach_pid).await
                }
                ModeTreeDeferredAction::KillCurrentTreeSelection => {
                    self.perform_tree_kill_current(attach_pid).await
                }
                ModeTreeDeferredAction::KillTaggedTreeSelections => {
                    self.perform_tree_kill_tagged(attach_pid).await
                }
            };
        }

        let plan = ConfirmBeforePlan {
            requester_pid: attach_pid,
            target_client: None,
            context: QueueExecutionContext::without_caller_cwd(),
            prompt,
            template: SAFE_PROMPT_TEMPLATE.to_owned(),
            confirm_key: 'y',
            default_yes: false,
            background: false,
            format_values: Vec::new(),
        };
        let outcome = self.start_confirm_before(plan).await?;
        if let PromptStartOutcome::Waiting(rx) = outcome {
            let handler = self.clone();
            tokio::spawn(async move {
                let Ok(result) = rx.await else {
                    return;
                };
                if result.inserted.is_some() {
                    let _ = match action {
                        ModeTreeDeferredAction::DeleteBuffers => {
                            handler.perform_buffer_delete(attach_pid).await
                        }
                        ModeTreeDeferredAction::DetachClients => {
                            handler.perform_client_detach(attach_pid).await
                        }
                        ModeTreeDeferredAction::KillCurrentTreeSelection => {
                            handler.perform_tree_kill_current(attach_pid).await
                        }
                        ModeTreeDeferredAction::KillTaggedTreeSelections => {
                            handler.perform_tree_kill_tagged(attach_pid).await
                        }
                    };
                }
            });
        }
        Ok(())
    }

    async fn apply_mode_tree_filter(
        &self,
        attach_pid: u32,
        value: String,
    ) -> Result<(), RmuxError> {
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        let Some(mode) = active.mode_tree.as_mut() else {
            return Ok(());
        };
        mode.filter_text = (!value.is_empty()).then_some(value);
        mode.scroll = 0;
        mode.preview_scroll = 0;
        drop(active_attach);
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    async fn apply_mode_tree_search(
        &self,
        attach_pid: u32,
        value: String,
        direction: SearchDirection,
    ) -> Result<(), RmuxError> {
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?
        };
        mode.search = (!value.is_empty()).then_some(SearchState { value, direction });
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        repeat_search(&mut mode, &build, false);
        self.store_mode_tree_state(attach_pid, mode).await?;
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    async fn apply_mode_tree_command(
        &self,
        attach_pid: u32,
        command: String,
    ) -> Result<(), RmuxError> {
        if command.trim().is_empty() {
            return Ok(());
        }
        let mut mode = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            active
                .mode_tree
                .clone()
                .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?
        };
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let targets = selected_items(&mode, &build)
            .into_iter()
            .filter_map(|item| item.action.target_string())
            .collect::<Vec<_>>();
        let refresh_sessions = self.dismiss_mode_tree(attach_pid).await?;
        if targets.is_empty() {
            let parsed = CommandParser::new()
                .parse_one_group(&command)
                .map_err(|error| RmuxError::Server(error.message().to_owned()))?;
            let context = QueueExecutionContext::without_caller_cwd();
            let _ = self
                .execute_parsed_commands(attach_pid, parsed, context)
                .await?;
        } else {
            for target in targets {
                let substituted = substitute_prompt_template(&command, &[target]);
                let parsed = CommandParser::new()
                    .parse_one_group(&substituted)
                    .map_err(|error| RmuxError::Server(error.message().to_owned()))?;
                let context = QueueExecutionContext::without_caller_cwd();
                let _ = self
                    .execute_parsed_commands(attach_pid, parsed, context)
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
        attach_pid: u32,
        scope: OptionScopeSelector,
        name: String,
        value: String,
    ) -> Result<(), RmuxError> {
        let response = self
            .handle_set_option_by_name(SetOptionByNameRequest {
                scope,
                name,
                value: Some(value),
                mode: SetOptionMode::Replace,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                format: false,
                format_target: None,
            })
            .await;
        if let Response::Error(error) = response {
            return Err(error.error);
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    async fn apply_customize_key_value(
        &self,
        attach_pid: u32,
        table_name: String,
        key: KeyCode,
        value: String,
    ) -> Result<(), RmuxError> {
        let parsed = CommandParser::new()
            .parse_one_group(&value)
            .map_err(|error| RmuxError::Server(error.message().to_owned()))?;
        let response = self
            .handle_bind_key(BindKeyRequest {
                table_name,
                key: rmux_core::key_string_lookup_key(key, false),
                note: None,
                repeat: false,
                command: Some(vec![parsed.to_tmux_reparse_string()]),
            })
            .await;
        if let Response::Error(error) = response {
            return Err(error.error);
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }
}
