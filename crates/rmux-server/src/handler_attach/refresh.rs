use std::collections::HashMap;

use super::super::prompt_support::ClientPromptState;
use super::super::RequestHandler;
use crate::pane_io::AttachControl;

impl RequestHandler {
    pub(crate) async fn refresh_attached_session(&self, session_name: &rmux_proto::SessionName) {
        let attached_count = { self.attached_count(session_name).await };
        let prompts = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| &active.session_name == session_name && !active.suspended)
                .map(|(pid, active)| {
                    (
                        *pid,
                        active
                            .prompt
                            .as_ref()
                            .map(ClientPromptState::rendered_prompt),
                        active.terminal_context.clone(),
                        active.client_size,
                        active.mode_tree_state_id,
                        active.mode_tree.is_some(),
                        active.key_table_name.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mode_tree_pids = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (&active.session_name == session_name
                        && !active.suspended
                        && active.mode_tree.is_some())
                    .then_some(*pid)
                })
                .collect::<Vec<_>>()
        };
        let overlay_pids = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (&active.session_name == session_name
                        && !active.suspended
                        && active.overlay.is_some())
                    .then_some(*pid)
                })
                .collect::<Vec<_>>()
        };
        let targets = {
            let state = self.state.lock().await;
            let mut targets = Vec::with_capacity(prompts.len());
            for (
                pid,
                prompt,
                terminal_context,
                client_size,
                mode_tree_state_id,
                mode_tree_active,
                key_table,
            ) in &prompts
            {
                let Ok(mut target) = super::attach_target_for_session_with_prompt(
                    &state,
                    session_name,
                    attached_count,
                    prompt.as_ref(),
                    key_table.as_deref(),
                    terminal_context,
                    Some(*client_size),
                ) else {
                    return;
                };
                if *mode_tree_active {
                    target.persistent_overlay_state_id = Some(*mode_tree_state_id);
                }
                targets.push((*pid, target));
            }
            targets
        };

        let mut target_by_pid = targets.into_iter().collect::<HashMap<_, _>>();
        let mut active_attach = self.active_attach.lock().await;
        active_attach.by_pid.retain(|pid, active| {
            if &active.session_name != session_name {
                return true;
            }
            if active.suspended {
                return true;
            }
            let Some(target) = target_by_pid.remove(pid) else {
                return false;
            };
            active.render_generation = active.render_generation.saturating_add(1);
            active
                .control_tx
                .send(AttachControl::switch(target))
                .is_ok()
        });
        drop(active_attach);
        self.refresh_clock_overlays_for_session(session_name).await;
        for attach_pid in mode_tree_pids {
            let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
        }
        for attach_pid in overlay_pids {
            let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
        }
        self.refresh_control_session(session_name).await;
    }

    pub(crate) async fn refresh_attached_client(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) {
        let attached_count = self.attached_count(session_name).await;
        let prompt = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| &active.session_name == session_name && !active.suspended)
                .map(|active| {
                    (
                        active
                            .prompt
                            .as_ref()
                            .map(ClientPromptState::rendered_prompt),
                        active.terminal_context.clone(),
                        active.client_size,
                        active.mode_tree_state_id,
                        active.mode_tree.is_some(),
                        active.key_table_name.clone(),
                    )
                })
        };
        let Some((
            prompt,
            terminal_context,
            client_size,
            mode_tree_state_id,
            mode_tree_active,
            key_table,
        )) = prompt
        else {
            return;
        };
        let target = {
            let state = self.state.lock().await;
            super::attach_target_for_session_with_prompt(
                &state,
                session_name,
                attached_count,
                prompt.as_ref(),
                key_table.as_deref(),
                &terminal_context,
                Some(client_size),
            )
            .ok()
        };
        let Some(mut target) = target else {
            return;
        };
        if mode_tree_active {
            target.persistent_overlay_state_id = Some(mode_tree_state_id);
        }

        let mut active_attach = self.active_attach.lock().await;
        let remove = match active_attach.by_pid.get_mut(&attach_pid) {
            Some(active) if &active.session_name == session_name && !active.suspended => {
                active.render_generation = active.render_generation.saturating_add(1);
                active
                    .control_tx
                    .send(AttachControl::switch(target))
                    .is_err()
            }
            _ => false,
        };
        if remove {
            active_attach.by_pid.remove(&attach_pid);
        }
        drop(active_attach);
        self.refresh_clock_overlays_for_session(session_name).await;
        let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
        let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
    }

    pub(crate) async fn refresh_attached_client_base_only(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) {
        let attached_count = self.attached_count(session_name).await;
        let prompt = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| &active.session_name == session_name && !active.suspended)
                .map(|active| {
                    (
                        active
                            .prompt
                            .as_ref()
                            .map(ClientPromptState::rendered_prompt),
                        active.terminal_context.clone(),
                        active.client_size,
                        active.mode_tree_state_id,
                        active.mode_tree.is_some(),
                        active.key_table_name.clone(),
                    )
                })
        };
        let Some((
            prompt,
            terminal_context,
            client_size,
            mode_tree_state_id,
            mode_tree_active,
            key_table,
        )) = prompt
        else {
            return;
        };
        let target = {
            let state = self.state.lock().await;
            super::attach_target_for_session_with_prompt(
                &state,
                session_name,
                attached_count,
                prompt.as_ref(),
                key_table.as_deref(),
                &terminal_context,
                Some(client_size),
            )
            .ok()
        };
        let Some(mut target) = target else {
            return;
        };
        if mode_tree_active {
            target.persistent_overlay_state_id = Some(mode_tree_state_id);
        }

        let mut active_attach = self.active_attach.lock().await;
        let remove = match active_attach.by_pid.get_mut(&attach_pid) {
            Some(active) if &active.session_name == session_name && !active.suspended => {
                active.render_generation = active.render_generation.saturating_add(1);
                active
                    .control_tx
                    .send(AttachControl::switch(target))
                    .is_err()
            }
            _ => false,
        };
        if remove {
            active_attach.by_pid.remove(&attach_pid);
        }
        drop(active_attach);
        self.refresh_clock_overlays_for_session(session_name).await;
    }

    pub(in crate::handler) async fn refresh_all_attached_sessions(&self) {
        let session_names = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .values()
                .map(|active| active.session_name.clone())
                .collect::<Vec<_>>()
        };

        for session_name in session_names {
            self.refresh_attached_session(&session_name).await;
        }
        self.refresh_all_control_sessions().await;
    }

    pub(in crate::handler) async fn refresh_persistent_overlays_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
    ) {
        let (mode_tree_pids, overlay_pids) = {
            let active_attach = self.active_attach.lock().await;
            let mode_tree_pids = active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (&active.session_name == session_name
                        && !active.suspended
                        && active.mode_tree.is_some())
                    .then_some(*pid)
                })
                .collect::<Vec<_>>();
            let overlay_pids = active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (&active.session_name == session_name
                        && !active.suspended
                        && active.overlay.is_some())
                    .then_some(*pid)
                })
                .collect::<Vec<_>>();
            (mode_tree_pids, overlay_pids)
        };

        for attach_pid in mode_tree_pids {
            let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
        }
        for attach_pid in overlay_pids {
            let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
        }
    }
}
