use rmux_core::command_parser::CommandParser;
use rmux_proto::{Response, RmuxError, SetOptionByNameRequest, SetOptionMode, UnbindKeyRequest};

use super::super::attach_support::attach_target_for_session;
use super::super::control_support::ManagedClient;
use super::super::prompt_support::substitute_prompt_template;
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use super::mode_tree_model::{
    ChooseTreeTarget, ModeTreeAction, ModeTreeActionIdentity, ModeTreeBuild, ModeTreeClientState,
    ModeTreeKind, ModeTreePromptCallback,
};
use super::mode_tree_selection::selected_items;
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
        let (mut mode, action_identity) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            (
                active
                    .mode_tree
                    .clone()
                    .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))?,
                ModeTreeActionIdentity::new(attach_pid, active.id, active.mode_tree_state_id),
            )
        };
        let had_tagged_items_before_rebuild = !mode.tagged.is_empty();
        let selected_id_before_rebuild = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if had_tagged_items_before_rebuild && mode.tagged.is_empty() {
            return self.refresh_mode_tree_overlay_if_active(attach_pid).await;
        }
        if mode.tagged.is_empty() {
            match selected_id_before_rebuild {
                Some(selected_id) if build.items.contains_key(&selected_id) => {
                    mode.selected_id = Some(selected_id);
                }
                Some(_) => return Ok(()),
                None => {}
            }
        }
        let targets = selected_items(&mode, &build);
        let Some(first) = targets.first() else {
            return Ok(());
        };

        match &first.action {
            ModeTreeAction::TreeTarget {
                session_name,
                session_id,
                window_index,
                window_id,
                window_occurrence_id,
                pane_index,
                pane_id,
            } if mode.template.as_deref() == Some(CHOOSE_TREE_DEFAULT_TEMPLATE) => {
                self.apply_choose_tree_default_target(
                    action_identity,
                    ChooseTreeTarget {
                        session_name: session_name.clone(),
                        session_id: *session_id,
                        window_index: *window_index,
                        window_id: *window_id,
                        window_occurrence_id: *window_occurrence_id,
                        pane_index: *pane_index,
                        pane_id: *pane_id,
                    },
                )
                .await?;
            }
            ModeTreeAction::Buffer { .. }
                if mode.template.as_deref() == Some(CHOOSE_BUFFER_DEFAULT_TEMPLATE) =>
            {
                self.perform_buffer_paste_for_identity(action_identity, false)
                    .await?;
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

    pub(super) async fn apply_choose_tree_default_target(
        &self,
        action_identity: ModeTreeActionIdentity,
        target: ChooseTreeTarget,
    ) -> Result<(), RmuxError> {
        let attach_pid = action_identity.attach_pid();
        let expected_attach_id = action_identity.attach_id();
        let ChooseTreeTarget {
            session_name,
            session_id,
            window_index,
            window_id,
            window_occurrence_id,
            pane_index,
            pane_id,
        } = target;
        {
            let mut state = self.state.lock().await;
            let active_attach = self.active_attach.lock().await;
            if active_attach.by_pid.get(&attach_pid).is_none_or(|active| {
                active.id != expected_attach_id
                    || active.mode_tree_state_id != action_identity.state_id()
                    || active.mode_tree.is_none()
                    || active.closing.load(std::sync::atomic::Ordering::SeqCst)
            }) {
                return Err(crate::handler_support::attached_client_required(
                    "switch-client",
                ));
            }
            if let (Some(window_index), Some(window_occurrence_id)) =
                (window_index, window_occurrence_id)
            {
                if state.window_link_occurrence_id(&session_name, window_index)
                    != Some(window_occurrence_id)
                {
                    return Err(RmuxError::invalid_target(
                        window_index.to_string(),
                        "window occurrence changed before mode-tree selection",
                    ));
                }
            }
            let session = state
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            if session.id() != session_id {
                return Err(RmuxError::SessionNotFound(session_name.to_string()));
            }
            let selected_pane_index = match (
                window_index,
                window_id,
                window_occurrence_id,
                pane_index,
                pane_id,
            ) {
                (None, None, None, None, None) => None,
                (Some(window_index), Some(window_id), Some(_), None, None) => {
                    let window = session.window_at(window_index).ok_or_else(|| {
                        RmuxError::invalid_target(
                            window_index.to_string(),
                            "window identity changed before mode-tree selection",
                        )
                    })?;
                    if window.id() != window_id {
                        return Err(RmuxError::invalid_target(
                            window_index.to_string(),
                            "window identity changed before mode-tree selection",
                        ));
                    }
                    None
                }
                (Some(window_index), Some(window_id), Some(_), Some(_), Some(pane_id)) => {
                    let window = session.window_at(window_index).ok_or_else(|| {
                        RmuxError::invalid_target(
                            window_index.to_string(),
                            "window identity changed before mode-tree selection",
                        )
                    })?;
                    if window.id() != window_id {
                        return Err(RmuxError::invalid_target(
                            window_index.to_string(),
                            "window identity changed before mode-tree selection",
                        ));
                    }
                    Some(
                        window
                            .panes()
                            .iter()
                            .find(|pane| pane.id() == pane_id)
                            .map(rmux_core::Pane::index)
                            .ok_or_else(|| {
                                RmuxError::invalid_target(
                                    pane_id.to_string(),
                                    "pane identity changed before mode-tree selection",
                                )
                            })?,
                    )
                }
                _ => {
                    return Err(RmuxError::Server(
                        "mode-tree target lost its stable identity".to_owned(),
                    ));
                }
            };
            if let Some(window_index) = window_index {
                session.select_window(window_index)?;
                if let Some(pane_index) = selected_pane_index {
                    session.select_pane_in_window(window_index, pane_index)?;
                }
            }
        }

        let current_session = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == expected_attach_id
                        && active.mode_tree_state_id == action_identity.state_id()
                        && active.mode_tree.is_some()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            (active.session_name.clone(), active.session_id)
        };
        let refresh_sessions = self
            .dismiss_mode_tree_for_action_identity(action_identity)
            .await?;
        if current_session == (session_name.clone(), session_id) {
            for session_name in refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
            return Ok(());
        }

        let attached_count = self
            .attached_count_after_switch(
                &session_name,
                ManagedClient::Attach {
                    pid: attach_pid,
                    attach_id: expected_attach_id,
                },
            )
            .await;
        let terminal_context = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == expected_attach_id
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|active| active.terminal_context.clone())
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?
        };
        let (target, target_session_id) = {
            let state = self.state.lock().await;
            let active_attach = self.active_attach.lock().await;
            if active_attach.by_pid.get(&attach_pid).is_none_or(|active| {
                active.id != expected_attach_id
                    || active.closing.load(std::sync::atomic::Ordering::SeqCst)
            }) {
                return Err(crate::handler_support::attached_client_required(
                    "switch-client",
                ));
            }
            let target_session_id = state
                .sessions
                .session(&session_name)
                .filter(|session| session.id() == session_id)
                .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?
                .id();
            let target = attach_target_for_session(
                &state,
                &session_name,
                attached_count,
                &terminal_context,
                &self.socket_path(),
            )?;
            (target, target_session_id)
        };
        let _ = self
            .send_attach_control_for_client_and_session_identity(
                attach_pid,
                expected_attach_id,
                AttachControl::switch(target),
                session_name.clone(),
                target_session_id,
                None,
            )
            .await?;
        self.emit_client_session_changed(attach_pid, session_name.clone(), target_session_id)
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
