use std::collections::HashMap;
use std::sync::atomic::Ordering;

use super::super::prompt_support::ClientPromptState;
use super::super::RequestHandler;
use super::state::ActiveAttach;
use crate::pane_io::AttachControl;

impl RequestHandler {
    pub(crate) async fn refresh_attached_session(&self, session_name: &rmux_proto::SessionName) {
        let attached_count = { self.attached_count(session_name).await };
        let (refresh_contexts, mode_tree_pids, overlay_pids) = {
            let mut active_attach = self.active_attach.lock().await;
            let mut refresh_contexts = Vec::new();
            let mut mode_tree_pids = Vec::new();
            let mut overlay_pids = Vec::new();
            let mut stale_pids = Vec::new();
            for (pid, active) in &mut active_attach.by_pid {
                if &active.session_name != session_name || active.suspended {
                    continue;
                }
                if active.mode_tree.is_some() {
                    mode_tree_pids.push(*pid);
                }
                if active.overlay.is_some() {
                    overlay_pids.push(*pid);
                }
                let coalescible_web_refresh = active.render_stream
                    && active.prompt.is_none()
                    && active.mode_tree.is_none()
                    && active.overlay.is_none()
                    && active.display_panes.is_none()
                    && active.key_table_name.is_none();
                if coalescible_web_refresh {
                    if !active.render_refresh_pending {
                        active.render_refresh_pending = true;
                        if !enqueue_tracked_render_control(active, AttachControl::Refresh) {
                            stale_pids.push(*pid);
                        }
                    }
                    continue;
                }
                refresh_contexts.push((
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
                ));
            }
            for pid in stale_pids {
                active_attach.by_pid.remove(&pid);
            }
            (refresh_contexts, mode_tree_pids, overlay_pids)
        };
        let targets = {
            let state = self.state.lock().await;
            let mut targets = Vec::with_capacity(refresh_contexts.len());
            for (
                pid,
                prompt,
                terminal_context,
                client_size,
                mode_tree_state_id,
                mode_tree_active,
                key_table,
            ) in &refresh_contexts
            {
                let Ok(mut target) = super::attach_render_target_for_session_with_prompt(
                    &state,
                    session_name,
                    attached_count,
                    super::AttachRenderTargetRequest {
                        prompt: prompt.as_ref(),
                        key_table: key_table.as_deref(),
                        terminal_context,
                        render_size: Some(*client_size),
                        socket_path: &self.socket_path(),
                    },
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
        let mut stale_pids = Vec::new();
        for (pid, active) in &mut active_attach.by_pid {
            if &active.session_name != session_name || active.suspended {
                continue;
            }
            let Some(target) = target_by_pid.remove(pid) else {
                continue;
            };
            active.render_generation = active.render_generation.saturating_add(1);
            active.render_refresh_pending = false;
            if !enqueue_tracked_render_control(active, AttachControl::switch(target)) {
                stale_pids.push(*pid);
            }
        }
        for pid in stale_pids {
            active_attach.by_pid.remove(&pid);
        }
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

    pub(crate) async fn clear_attached_render_refresh_pending(&self, attach_pid: u32) {
        let mut active_attach = self.active_attach.lock().await;
        if let Some(active) = active_attach.by_pid.get_mut(&attach_pid) {
            active.render_refresh_pending = false;
        }
    }

    pub(crate) async fn mark_attached_session_interactive_input(
        &self,
        session_name: &rmux_proto::SessionName,
    ) {
        let mut active_attach = self.active_attach.lock().await;
        let mut stale_pids = Vec::new();
        for (pid, active) in &mut active_attach.by_pid {
            if &active.session_name != session_name || active.suspended {
                continue;
            }
            if !enqueue_tracked_interactive_input_control(active) {
                stale_pids.push(*pid);
            }
        }
        for pid in stale_pids {
            active_attach.by_pid.remove(&pid);
        }
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
            super::attach_render_target_for_session_with_prompt(
                &state,
                session_name,
                attached_count,
                super::AttachRenderTargetRequest {
                    prompt: prompt.as_ref(),
                    key_table: key_table.as_deref(),
                    terminal_context: &terminal_context,
                    render_size: Some(client_size),
                    socket_path: &self.socket_path(),
                },
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
                !enqueue_tracked_render_control(active, AttachControl::switch(target))
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
            super::attach_render_target_for_session_with_prompt(
                &state,
                session_name,
                attached_count,
                super::AttachRenderTargetRequest {
                    prompt: prompt.as_ref(),
                    key_table: key_table.as_deref(),
                    terminal_context: &terminal_context,
                    render_size: Some(client_size),
                    socket_path: &self.socket_path(),
                },
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
                !enqueue_tracked_render_control(active, AttachControl::switch(target))
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

fn enqueue_tracked_render_control(active: &mut ActiveAttach, command: AttachControl) -> bool {
    debug_assert!(matches!(
        command,
        AttachControl::Refresh | AttachControl::Switch(_)
    ));
    if active.control_backlog.load(Ordering::Acquire) >= super::ATTACH_CONTROL_BACKLOG_LIMIT {
        active.closing.store(true, Ordering::SeqCst);
        let _ = active.control_tx.send(AttachControl::Detach);
        return false;
    }
    active.control_backlog.fetch_add(1, Ordering::AcqRel);
    if active.control_tx.send(command).is_err() {
        let _ = active
            .control_backlog
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_sub(1)
            });
        return false;
    }
    true
}

fn enqueue_tracked_interactive_input_control(active: &mut ActiveAttach) -> bool {
    if active.control_backlog.load(Ordering::Acquire) >= super::ATTACH_CONTROL_BACKLOG_LIMIT {
        active.closing.store(true, Ordering::SeqCst);
        let _ = active.control_tx.send(AttachControl::Detach);
        return false;
    }
    active.control_backlog.fetch_add(1, Ordering::AcqRel);
    if active
        .control_tx
        .send(AttachControl::InteractiveInput)
        .is_err()
    {
        let _ = active
            .control_backlog
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_sub(1)
            });
        return false;
    }
    true
}
