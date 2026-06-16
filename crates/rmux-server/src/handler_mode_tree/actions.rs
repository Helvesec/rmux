use rmux_core::{command_parser::CommandParser, LifecycleEvent};
use rmux_proto::{
    DeleteBufferRequest, KillPaneRequest, KillSessionRequest, KillWindowRequest, PaneTarget,
    PasteBufferRequest, Response, RmuxError, SessionName, SetOptionByNameRequest, SetOptionMode,
    UnbindKeyRequest, WindowTarget,
};

use super::super::attach_support::attach_target_for_session;
use super::super::control_support::ManagedClient;
use super::super::prompt_support::substitute_prompt_template;
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use super::mode_tree_model::{
    ModeTreeAction, ModeTreeBuild, ModeTreeClientState, ModeTreeKind, ModeTreePromptCallback,
};
use super::mode_tree_selection::{current_selected_item, selected_items, tree_kill_sort_key};
use super::{
    CHOOSE_BUFFER_DEFAULT_TEMPLATE, CHOOSE_CLIENT_DEFAULT_TEMPLATE, CHOOSE_TREE_DEFAULT_TEMPLATE,
};
use crate::pane_io::AttachControl;
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(super) async fn accept_mode_tree_selection(
        &self,
        attach_pid: u32,
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
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let targets = selected_items(&mode, &build);
        let Some(first) = targets.first() else {
            return Ok(());
        };

        match &first.action {
            ModeTreeAction::TreeTarget {
                session_name,
                window_index,
                pane_index,
                ..
            } if mode.template.as_deref() == Some(CHOOSE_TREE_DEFAULT_TEMPLATE) => {
                self.apply_choose_tree_default_target(
                    attach_pid,
                    session_name.clone(),
                    *window_index,
                    *pane_index,
                )
                .await?;
            }
            ModeTreeAction::Buffer { .. }
                if mode.template.as_deref() == Some(CHOOSE_BUFFER_DEFAULT_TEMPLATE) =>
            {
                self.perform_buffer_paste(attach_pid, false).await?;
            }
            ModeTreeAction::Client { .. }
                if mode.template.as_deref() == Some(CHOOSE_CLIENT_DEFAULT_TEMPLATE) =>
            {
                self.perform_client_detach(attach_pid).await?;
            }
            ModeTreeAction::CustomizeOption { .. } | ModeTreeAction::CustomizeKey { .. }
                if matches!(mode.kind, ModeTreeKind::Customize) =>
            {
                self.start_customize_set_prompt(attach_pid).await?;
            }
            ModeTreeAction::None if matches!(mode.kind, ModeTreeKind::Customize) => {
                // Category headers in customize-mode: no-op on Enter.
            }
            _ => {
                self.run_mode_tree_template(attach_pid, &mode, &build)
                    .await?;
            }
        }
        Ok(())
    }

    async fn apply_choose_tree_default_target(
        &self,
        attach_pid: u32,
        session_name: SessionName,
        window_index: Option<u32>,
        pane_index: Option<u32>,
    ) -> Result<(), RmuxError> {
        {
            let mut state = self.state.lock().await;
            let session = state
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            if let Some(window_index) = window_index {
                session.select_window(window_index)?;
                if let Some(pane_index) = pane_index {
                    session.select_pane_in_window(window_index, pane_index)?;
                }
            }
        }

        let current_session = self.attached_session_name(attach_pid).await?;
        let refresh_sessions = self.dismiss_mode_tree(attach_pid).await?;
        if current_session == session_name {
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
            return Ok(());
        }

        let attached_count = self
            .attached_count_after_switch(&session_name, ManagedClient::Attach(attach_pid))
            .await;
        let Some(terminal_context) = self.terminal_context_for_attached_client(attach_pid).await
        else {
            return Err(RmuxError::Server("attached client disappeared".to_owned()));
        };
        let target = {
            let state = self.state.lock().await;
            attach_target_for_session(
                &state,
                &session_name,
                attached_count,
                &terminal_context,
                &self.socket_path(),
            )?
        };
        let _ = self
            .send_attach_control(
                attach_pid,
                AttachControl::switch(target),
                "switch-client",
                Some(session_name.clone()),
            )
            .await?;
        self.emit(LifecycleEvent::ClientSessionChanged {
            session_name: session_name.clone(),
            client_name: Some(attach_pid.to_string()),
        })
        .await;
        for refresh in refresh_sessions {
            self.refresh_attached_session(&refresh).await;
        }
        self.refresh_attached_session(&session_name).await;
        Ok(())
    }

    async fn run_mode_tree_template(
        &self,
        attach_pid: u32,
        mode: &ModeTreeClientState,
        build: &ModeTreeBuild,
    ) -> Result<(), RmuxError> {
        let Some(template) = mode.template.as_deref() else {
            return Ok(());
        };
        let requester_pid = attach_pid;
        let targets = selected_items(mode, build)
            .into_iter()
            .filter_map(|item| item.action.target_string())
            .collect::<Vec<_>>();
        let current_target = selected_items(mode, build)
            .first()
            .and_then(|item| item.action.current_target());
        let refresh_sessions = self.dismiss_mode_tree(attach_pid).await?;
        for target in targets {
            let substituted = substitute_prompt_template(template, &[target]);
            let parsed = CommandParser::new()
                .parse_one_group(&substituted)
                .map_err(|error| {
                    RmuxError::Server(format!(
                        "mode-tree command parse failed: {}",
                        error.message()
                    ))
                })?;
            let context = QueueExecutionContext::without_caller_cwd()
                .with_current_target(current_target.clone());
            let _ = self
                .execute_parsed_commands(requester_pid, parsed, context)
                .await?;
        }
        for session_name in refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        Ok(())
    }

    pub(super) async fn perform_buffer_paste(
        &self,
        attach_pid: u32,
        delete_after: bool,
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
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let target = self.mode_tree_active_pane(&mode.session_name).await?;
        for item in selected_items(&mode, &build) {
            let ModeTreeAction::Buffer { name } = &item.action else {
                continue;
            };
            let response = self
                .handle_paste_buffer(PasteBufferRequest {
                    name: Some(name.clone()),
                    target: target.clone(),
                    delete_after,
                    separator: None,
                    linefeed: false,
                    raw: false,
                    bracketed: false,
                })
                .await;
            if let Response::Error(error) = response {
                return Err(error.error);
            }
        }
        self.dismiss_mode_tree_with_refresh(attach_pid).await
    }

    pub(super) async fn perform_tree_kill_current(&self, attach_pid: u32) -> Result<(), RmuxError> {
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
        let Some(item) = current_selected_item(&mode, &build) else {
            return Ok(());
        };
        self.perform_tree_kill_actions(attach_pid, vec![item.action.clone()])
            .await
    }

    pub(super) async fn perform_tree_kill_tagged(&self, attach_pid: u32) -> Result<(), RmuxError> {
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
        let actions = build
            .items
            .values()
            .filter(|item| mode.tagged.contains(&item.id))
            .map(|item| item.action.clone())
            .collect::<Vec<_>>();
        self.perform_tree_kill_actions(attach_pid, actions).await
    }

    async fn perform_tree_kill_actions(
        &self,
        attach_pid: u32,
        mut actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        actions.sort_by_key(tree_kill_sort_key);
        for action in actions {
            let response = match action {
                ModeTreeAction::TreeTarget {
                    session_name,
                    window_index: None,
                    ..
                } => {
                    self.handle_kill_session(KillSessionRequest {
                        target: session_name,
                        kill_all_except_target: false,
                        clear_alerts: false,
                    })
                    .await
                }
                ModeTreeAction::TreeTarget {
                    session_name,
                    window_index: Some(window_index),
                    pane_index: None,
                    ..
                } => {
                    self.handle_kill_window(KillWindowRequest {
                        target: WindowTarget::with_window(session_name, window_index),
                        kill_all_others: false,
                    })
                    .await
                }
                ModeTreeAction::TreeTarget {
                    session_name,
                    window_index: Some(window_index),
                    pane_index: Some(pane_index),
                    ..
                } => {
                    self.handle_kill_pane(KillPaneRequest {
                        target: PaneTarget::with_window(session_name, window_index, pane_index),
                        kill_all_except: false,
                    })
                    .await
                }
                ModeTreeAction::None
                | ModeTreeAction::Buffer { .. }
                | ModeTreeAction::Client { .. }
                | ModeTreeAction::CustomizeOption { .. }
                | ModeTreeAction::CustomizeKey { .. } => continue,
            };
            if let Response::Error(error) = response {
                return Err(error.error);
            }
        }
        if self.mode_tree_active(attach_pid).await {
            self.refresh_mode_tree_overlay_if_active(attach_pid).await?;
        }
        Ok(())
    }

    pub(super) async fn perform_buffer_delete(&self, attach_pid: u32) -> Result<(), RmuxError> {
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
        for item in selected_items(&mode, &build) {
            let ModeTreeAction::Buffer { name } = &item.action else {
                continue;
            };
            let response = self
                .handle_delete_buffer(DeleteBufferRequest {
                    name: Some(name.clone()),
                })
                .await;
            if let Response::Error(error) = response {
                return Err(error.error);
            }
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    pub(super) async fn perform_client_detach(&self, attach_pid: u32) -> Result<(), RmuxError> {
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
        let items = selected_items(&mode, &build);

        // Detach self last so other detaches complete while we still have state.
        let mut self_detach = false;
        for item in &items {
            let ModeTreeAction::Client { pid, control } = item.action else {
                continue;
            };
            if pid == attach_pid && !control {
                self_detach = true;
                continue;
            }
            if control {
                let session = self.exit_control_client(pid, None).await?;
                if let Some(session_name) = session {
                    self.emit(LifecycleEvent::ClientDetached {
                        session_name,
                        client_name: Some(pid.to_string()),
                    })
                    .await;
                }
            } else if let Ok(session_name) = self
                .send_attach_control(pid, AttachControl::Detach, "detach-client", None)
                .await
            {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name,
                    client_name: Some(pid.to_string()),
                })
                .await;
            }
        }
        if self_detach {
            if let Ok(session_name) = self
                .send_attach_control(attach_pid, AttachControl::Detach, "detach-client", None)
                .await
            {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name,
                    client_name: Some(attach_pid.to_string()),
                })
                .await;
            }
            return Ok(());
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    pub(super) async fn start_customize_set_prompt(
        &self,
        attach_pid: u32,
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
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let selected = selected_items(&mode, &build);
        let Some(selected) = selected.first() else {
            return Ok(());
        };
        match &selected.action {
            ModeTreeAction::CustomizeOption { scope, name, .. } => {
                self.start_mode_tree_prompt(
                    attach_pid,
                    ModeTreePromptCallback::CustomizeSetOption {
                        scope: scope.clone(),
                        name: name.clone(),
                    },
                )
                .await?;
            }
            ModeTreeAction::CustomizeKey {
                table_name, key, ..
            } => {
                self.start_mode_tree_prompt(
                    attach_pid,
                    ModeTreePromptCallback::CustomizeSetKey {
                        table_name: table_name.clone(),
                        key: *key,
                    },
                )
                .await?;
            }
            ModeTreeAction::None
            | ModeTreeAction::TreeTarget { .. }
            | ModeTreeAction::Buffer { .. }
            | ModeTreeAction::Client { .. } => {}
        }
        Ok(())
    }

    pub(super) async fn perform_customize_unset(&self, attach_pid: u32) -> Result<(), RmuxError> {
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
        let selected = selected_items(&mode, &build);
        let Some(selected) = selected.first() else {
            return Ok(());
        };
        match &selected.action {
            ModeTreeAction::CustomizeOption { scope, name, .. } => {
                let response = self
                    .handle_set_option_by_name(SetOptionByNameRequest {
                        scope: scope.clone(),
                        name: name.clone(),
                        value: None,
                        mode: SetOptionMode::Replace,
                        only_if_unset: false,
                        unset: true,
                        unset_pane_overrides: false,
                        format: false,
                        format_target: None,
                    })
                    .await;
                if let Response::Error(error) = response {
                    return Err(error.error);
                }
            }
            ModeTreeAction::CustomizeKey {
                table_name,
                key_string,
                ..
            } => {
                let response = self
                    .handle_unbind_key(UnbindKeyRequest {
                        table_name: table_name.clone(),
                        all: false,
                        key: Some(key_string.clone()),
                        quiet: true,
                    })
                    .await;
                if let Response::Error(error) = response {
                    return Err(error.error);
                }
            }
            _ => {}
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    pub(super) async fn perform_customize_reset(&self, attach_pid: u32) -> Result<(), RmuxError> {
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
        let selected = selected_items(&mode, &build);
        let Some(selected) = selected.first() else {
            return Ok(());
        };
        match &selected.action {
            ModeTreeAction::CustomizeOption { scope, name, .. } => {
                let response = self
                    .handle_set_option_by_name(SetOptionByNameRequest {
                        scope: scope.clone(),
                        name: name.clone(),
                        value: None,
                        mode: SetOptionMode::Replace,
                        only_if_unset: false,
                        unset: true,
                        unset_pane_overrides: false,
                        format: false,
                        format_target: None,
                    })
                    .await;
                if let Response::Error(error) = response {
                    return Err(error.error);
                }
            }
            ModeTreeAction::CustomizeKey {
                table_name, key, ..
            } => {
                let mut state = self.state.lock().await;
                state.key_bindings.reset_binding(table_name, *key);
                drop(state);
            }
            _ => {}
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }
}
