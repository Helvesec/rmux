use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use super::super::prompt_support::ClientPromptState;
use super::super::RequestHandler;
use super::state::ActiveAttach;
use crate::pane_io::AttachControl;

impl RequestHandler {
    pub(crate) async fn refresh_attached_session(&self, session_name: &rmux_proto::SessionName) {
        let _refresh_span = crate::perf_instrument::span("attach_refresh")
            .with_str("scope", "session")
            .with_str("session", session_name.as_str());
        #[cfg(windows)]
        self.wait_for_windows_deferred_session_pane_pids(session_name)
            .await;
        let removed_stale_clients = self
            .prune_stale_attached_clients_for_session(session_name)
            .await;
        if !removed_stale_clients.is_empty() {
            let _ = self
                .reconcile_attached_session_size_and_emit(session_name)
                .await;
        }
        let (
            refresh_contexts,
            display_panes_identities,
            mode_tree_pids,
            overlay_pids,
            stale_clients,
        ) = {
            let mut active_attach = self.active_attach.lock().await;
            let mut refresh_contexts = Vec::new();
            let mut display_panes_identities = Vec::new();
            let mut mode_tree_pids = Vec::new();
            let mut overlay_pids = Vec::new();
            let mut stale_clients = Vec::new();
            for (pid, active) in &mut active_attach.by_pid {
                if &active.session_name != session_name || active.suspended {
                    continue;
                }
                if active.mode_tree.is_some() {
                    mode_tree_pids.push(*pid);
                }
                if active.display_panes.is_some() {
                    display_panes_identities.push((*pid, active.id));
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
                            stale_clients.push(active.identity(*pid));
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
            (
                refresh_contexts,
                display_panes_identities,
                mode_tree_pids,
                overlay_pids,
                stale_clients,
            )
        };
        let removed_stale_clients = self
            .remove_attached_clients_for_session(session_name, stale_clients)
            .await;
        if !removed_stale_clients.is_empty() {
            let _ = self
                .reconcile_attached_session_size_and_emit(session_name)
                .await;
        }
        let attached_count = { self.attached_count(session_name).await };
        let targets = {
            let state = self.state.lock().await;
            let _lock_span = crate::perf_instrument::span("state_lock_hold")
                .with_str("site", "attach_refresh_session_targets");
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
        let mut stale_clients = Vec::new();
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
                stale_clients.push(active.identity(*pid));
            }
        }
        drop(active_attach);
        let removed_stale_clients = self
            .remove_attached_clients_for_session(session_name, stale_clients)
            .await;
        if !removed_stale_clients.is_empty() {
            let _ = self
                .reconcile_attached_session_size_and_emit(session_name)
                .await;
        }
        self.refresh_clock_overlays_for_session(session_name).await;
        for (attach_pid, attach_id) in display_panes_identities {
            let _ = self
                .refresh_display_panes_overlay_for_client_identity(
                    attach_pid,
                    attach_id,
                    session_name,
                )
                .await;
        }
        for attach_pid in overlay_pids {
            let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
        }
        for attach_pid in mode_tree_pids {
            let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
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
        let stale_clients = {
            let mut active_attach = self.active_attach.lock().await;
            let mut stale_clients = Vec::new();
            for (pid, active) in &mut active_attach.by_pid {
                if &active.session_name != session_name || active.suspended {
                    continue;
                }
                if !enqueue_tracked_interactive_input_control(active) {
                    stale_clients.push(active.identity(*pid));
                }
            }
            stale_clients
        };
        let removed_stale_clients = self
            .remove_attached_clients_for_session(session_name, stale_clients)
            .await;
        if !removed_stale_clients.is_empty() {
            let _ = self
                .reconcile_attached_session_size_and_emit(session_name)
                .await;
        }
    }

    pub(crate) async fn refresh_attached_client(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) {
        let _ = self
            .refresh_attached_client_with_expected_identity(attach_pid, None, session_name)
            .await;
    }

    pub(crate) async fn refresh_attached_client_for_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
        command_name: &str,
    ) -> Result<(), rmux_proto::RmuxError> {
        if self
            .refresh_attached_client_with_expected_identity(
                attach_pid,
                Some(expected_attach_id),
                session_name,
            )
            .await
        {
            Ok(())
        } else {
            Err(crate::handler_support::attached_client_required(
                command_name,
            ))
        }
    }

    async fn refresh_attached_client_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
        session_name: &rmux_proto::SessionName,
    ) -> bool {
        let _refresh_span = crate::perf_instrument::span("attach_refresh")
            .with_str("scope", "client")
            .with_u64("attach_pid", u64::from(attach_pid))
            .with_str("session", session_name.as_str());
        #[cfg(windows)]
        self.wait_for_windows_deferred_session_pane_pids(session_name)
            .await;
        let attached_count = self.attached_count(session_name).await;
        let prompt = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    expected_attach_id.is_none_or(|expected| {
                        active.id == expected && !active.closing.load(Ordering::SeqCst)
                    }) && &active.session_name == session_name
                        && !active.suspended
                })
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
            return false;
        };
        let target = {
            let state = self.state.lock().await;
            let _lock_span = crate::perf_instrument::span("state_lock_hold")
                .with_str("site", "attach_refresh_client_target");
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
            return false;
        };
        if mode_tree_active {
            target.persistent_overlay_state_id = Some(mode_tree_state_id);
        }

        let mut active_attach = self.active_attach.lock().await;
        let (delivered, stale_client) = match active_attach.by_pid.get_mut(&attach_pid) {
            Some(active)
                if expected_attach_id.is_none_or(|expected| {
                    active.id == expected && !active.closing.load(Ordering::SeqCst)
                }) && &active.session_name == session_name
                    && !active.suspended =>
            {
                active.render_generation = active.render_generation.saturating_add(1);
                let delivered =
                    enqueue_tracked_render_control(active, AttachControl::switch(target));
                (delivered, (!delivered).then(|| active.identity(attach_pid)))
            }
            _ => (false, None),
        };
        drop(active_attach);
        if let Some(stale_client) = stale_client {
            let removed_stale_clients = self
                .remove_attached_clients_for_session(session_name, vec![stale_client])
                .await;
            if !removed_stale_clients.is_empty() {
                let _ = self
                    .reconcile_attached_session_size_and_emit(session_name)
                    .await;
            }
        }
        if let Some(expected_attach_id) = expected_attach_id {
            if !delivered {
                return false;
            }
            if self
                .refresh_clock_overlay_for_client_identity(
                    attach_pid,
                    expected_attach_id,
                    session_name,
                )
                .await
                .is_err()
            {
                return false;
            }
            if self
                .refresh_display_panes_overlay_for_client_identity(
                    attach_pid,
                    expected_attach_id,
                    session_name,
                )
                .await
                .is_err()
            {
                return false;
            }
            if self
                .refresh_interactive_overlay_for_client_identity(
                    attach_pid,
                    expected_attach_id,
                    session_name,
                )
                .await
                .is_err()
            {
                return false;
            }
            return self
                .refresh_mode_tree_overlay_for_client_identity(
                    attach_pid,
                    expected_attach_id,
                    session_name,
                )
                .await
                .is_ok();
        }
        self.refresh_clock_overlays_for_session(session_name).await;
        let current_attach_id = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| &active.session_name == session_name && !active.suspended)
                .map(|active| active.id)
        };
        if let Some(current_attach_id) = current_attach_id {
            let _ = self
                .refresh_display_panes_overlay_for_client_identity(
                    attach_pid,
                    current_attach_id,
                    session_name,
                )
                .await;
        }
        let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
        let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
        delivered
    }

    #[cfg(test)]
    pub(crate) async fn refresh_attached_client_base_only(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
    ) {
        let _refresh_span = crate::perf_instrument::span("attach_refresh")
            .with_str("scope", "client_base_only")
            .with_u64("attach_pid", u64::from(attach_pid))
            .with_str("session", session_name.as_str());
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
            let _lock_span = crate::perf_instrument::span("state_lock_hold")
                .with_str("site", "attach_refresh_client_base_target");
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
        let stale_client = match active_attach.by_pid.get_mut(&attach_pid) {
            Some(active) if &active.session_name == session_name && !active.suspended => {
                active.render_generation = active.render_generation.saturating_add(1);
                (!enqueue_tracked_render_control(active, AttachControl::switch(target)))
                    .then(|| active.identity(attach_pid))
            }
            _ => None,
        };
        drop(active_attach);
        if let Some(stale_client) = stale_client {
            let removed_stale_clients = self
                .remove_attached_clients_for_session(session_name, vec![stale_client])
                .await;
            if !removed_stale_clients.is_empty() {
                let _ = self
                    .reconcile_attached_session_size_and_emit(session_name)
                    .await;
            }
        }
        self.refresh_clock_overlays_for_session(session_name).await;
    }

    pub(in crate::handler) async fn refresh_all_attached_sessions(&self) {
        let session_names = {
            let active_attach = self.active_attach.lock().await;
            let mut seen = HashSet::new();
            let mut session_names = Vec::new();
            for active in active_attach.by_pid.values() {
                if seen.insert(active.session_name.clone()) {
                    session_names.push(active.session_name.clone());
                }
            }
            session_names
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

pub(super) fn enqueue_tracked_render_control(
    active: &mut ActiveAttach,
    command: AttachControl,
) -> bool {
    debug_assert!(matches!(
        command,
        AttachControl::Refresh | AttachControl::Switch(_)
    ));
    if let Err(error) = active.control_tx.send(command) {
        if error.is_full() {
            active.closing.store(true, Ordering::SeqCst);
        }
        return false;
    }
    true
}

fn enqueue_tracked_interactive_input_control(active: &mut ActiveAttach) -> bool {
    if let Err(error) = active.control_tx.send(AttachControl::InteractiveInput) {
        if error.is_full() {
            active.closing.store(true, Ordering::SeqCst);
        }
        return false;
    }
    true
}
