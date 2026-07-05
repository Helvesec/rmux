use rmux_core::{
    LifecycleEvent, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget, Window,
};
use rmux_proto::{OptionName, PaneTarget, RmuxError, SessionName, Target};
use std::collections::BTreeSet;

use super::super::scripting_support::{QueueCommandAction, QueueExecutionContext};
use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeClientState, ModeTreeKind, ParsedModeTreeCommand};
use super::mode_tree_parse::default_order_seq;
use super::mode_tree_render::render_mode_tree_overlay;
use super::{default_template, DEFAULT_KEY_FORMAT, MODE_TREE_HELP};
use crate::handler_support::attached_client_required;
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(in crate::handler) async fn execute_queued_mode_tree(
        &self,
        requester_pid: u32,
        command: ParsedModeTreeCommand,
        _context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        let attach_pid = match self
            .mode_tree_attach_pid(requester_pid, command.kind.command_name())
            .await
        {
            Ok(attach_pid) => Some(attach_pid),
            Err(error) if is_missing_attached_client(&error, command.kind.command_name()) => None,
            Err(error) => return Err(error),
        };

        let (session_name, host_pane) = match attach_pid {
            Some(attach_pid) => (
                self.attached_session_name(attach_pid).await?,
                self.attached_input_target(attach_pid).await.ok(),
            ),
            None => {
                let target = self
                    .detached_mode_tree_target(command.target.as_deref())
                    .await?;
                (target.session_name().clone(), Some(target))
            }
        };
        let order_seq = default_order_seq(command.kind);
        let sort_order = match command.sort_order {
            Some(sort_order) if order_seq.contains(&sort_order) => Some(sort_order),
            Some(_) => {
                return Err(RmuxError::Server(format!(
                    "invalid sort order for {}",
                    command.kind.command_name()
                )));
            }
            None => order_seq.first().copied(),
        };

        let zoom_restore = if command.zoom && attach_pid.is_some() {
            self.mode_tree_zoom_target(&session_name).await?
        } else {
            None
        };

        let mut mode = ModeTreeClientState {
            kind: command.kind,
            session_name: session_name.clone(),
            host_pane,
            preview_mode: command.preview_mode,
            row_format: command.row_format,
            filter_format: command.filter_format,
            filter_text: None,
            key_format: command
                .key_format
                .unwrap_or_else(|| DEFAULT_KEY_FORMAT.to_owned()),
            template: command.template.or_else(|| default_template(command.kind)),
            search: None,
            tagged: BTreeSet::new(),
            expanded: BTreeSet::new(),
            selected_id: None,
            scroll: 0,
            preview_scroll: 0,
            sort_order,
            order_seq,
            reversed: command.reversed,
            tree_depth: command.tree_depth,
            show_all_group_members: command.show_all_group_members,
            auto_accept: command.auto_accept,
            zoom_restore,
            last_list_rows: 0,
        };

        self.seed_mode_tree_defaults(&mut mode).await?;
        if matches!(mode.kind, ModeTreeKind::Buffer) && self.mode_tree_buffer_empty().await {
            if let Some(attach_pid) = attach_pid {
                self.dismiss_mode_tree_with_refresh(attach_pid).await?;
            }
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }
        if matches!(mode.kind, ModeTreeKind::Client) && self.mode_tree_client_empty().await {
            if let Some(attach_pid) = attach_pid {
                self.dismiss_mode_tree_with_refresh(attach_pid).await?;
            }
            return Ok(QueueCommandAction::Normal {
                output: None,
                error: None,
                source_file_error: None,
                exit_status: None,
            });
        }

        if attach_pid.is_some() {
            self.activate_mode_tree_for_session(&session_name, &mode)
                .await?;
            if let Some(target) = mode.host_pane.as_ref() {
                if self.enter_mode_tree_for_target(target, mode.kind).await? {
                    self.emit(LifecycleEvent::PaneModeChanged {
                        target: target.clone(),
                    })
                    .await;
                }
            }
            self.refresh_attached_session(&session_name).await;
        } else if let Some(target) = mode.host_pane.as_ref() {
            if self.enter_mode_tree_for_target(target, mode.kind).await? {
                self.emit(LifecycleEvent::PaneModeChanged {
                    target: target.clone(),
                })
                .await;
            }
        }

        Ok(QueueCommandAction::Normal {
            output: None,
            error: None,
            source_file_error: None,
            exit_status: None,
        })
    }

    async fn mode_tree_attach_pid(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<u32, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        if active_attach.by_pid.contains_key(&requester_pid) {
            return Ok(requester_pid);
        }
        active_attach
            .by_pid
            .iter()
            .min_by_key(|(_, active)| active.id)
            .map(|(&pid, _)| pid)
            .ok_or_else(|| attached_client_required(command_name))
    }

    async fn detached_mode_tree_target(
        &self,
        target: Option<&str>,
    ) -> Result<PaneTarget, RmuxError> {
        let state = self.state.lock().await;
        let unresolved = target
            .map(|target| UnresolvedTarget::new(target.to_owned()))
            .unwrap_or_else(UnresolvedTarget::none);
        let resolved = state.sessions.resolve_unresolved_target(
            &unresolved,
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            &TargetFindContext::new(None),
        )?;
        match resolved {
            Target::Pane(target) => Ok(target),
            Target::Session(_) | Target::Window(_) => Err(RmuxError::Server(
                "mode tree target did not resolve to a pane".to_owned(),
            )),
        }
    }

    async fn activate_mode_tree_for_session(
        &self,
        session_name: &SessionName,
        mode: &ModeTreeClientState,
    ) -> Result<(), RmuxError> {
        let mut active_attach = self.active_attach.lock().await;
        let mut activated = false;
        for active in active_attach.by_pid.values_mut() {
            if active.session_name != *session_name || active.suspended {
                continue;
            }
            active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
            active.persistent_overlay_epoch.store(
                active.mode_tree_state_id,
                std::sync::atomic::Ordering::SeqCst,
            );
            active.mode_tree = Some(mode.clone());
            activated = true;
        }
        if activated {
            Ok(())
        } else {
            Err(attached_client_required(mode.kind.command_name()))
        }
    }

    async fn active_mode_tree_session_name(
        &self,
        attach_pid: u32,
    ) -> Result<Option<SessionName>, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        Ok(active
            .mode_tree
            .as_ref()
            .map(|mode| mode.session_name.clone()))
    }

    pub(in crate::handler) async fn mode_tree_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.mode_tree.is_some())
    }

    pub(in crate::handler) async fn refresh_mode_tree_overlay_if_active(
        &self,
        attach_pid: u32,
    ) -> Result<(), RmuxError> {
        let (mut mode, mode_tree_state_id) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(mode) = active.mode_tree.clone() else {
                return Ok(());
            };
            (mode, active.mode_tree_state_id)
        };
        let session_name = mode.session_name.clone();

        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        let overlay = {
            let state = self.state.lock().await;
            render_mode_tree_overlay(&state, &mode, &build)
        };

        {
            let mut active_attach = self.active_attach.lock().await;
            active_attach.by_pid.retain(|_, active| {
                if active.session_name != session_name
                    || active.mode_tree.is_none()
                    || active.mode_tree_state_id != mode_tree_state_id
                {
                    return true;
                }
                active.mode_tree = Some(mode.clone());
                if active.suspended {
                    return true;
                }
                active.overlay_generation = active.overlay_generation.saturating_add(1);
                active.mode_tree_frame = Some(overlay.clone());
                active
                    .control_tx
                    .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
                        overlay.clone(),
                        active.render_generation,
                        active.overlay_generation,
                        active.mode_tree_state_id,
                    )))
                    .is_ok()
            });
        }
        self.refresh_control_session(&session_name).await;
        Ok(())
    }

    async fn mode_tree_zoom_target(
        &self,
        session_name: &SessionName,
    ) -> Result<Option<PaneTarget>, RmuxError> {
        let maybe_target = {
            let mut state = self.state.lock().await;
            let session = state
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let window_index = session.active_window_index();
            let pane_index = session.active_pane_index();
            if session
                .window_at(window_index)
                .is_some_and(Window::is_zoomed)
            {
                None
            } else {
                session.toggle_zoom_in_window(window_index, pane_index)?;
                Some(PaneTarget::with_window(
                    session_name.clone(),
                    window_index,
                    pane_index,
                ))
            }
        };
        if maybe_target.is_some() {
            self.refresh_attached_session(session_name).await;
        }
        Ok(maybe_target)
    }

    pub(super) async fn dismiss_mode_tree_with_refresh(
        &self,
        attach_pid: u32,
    ) -> Result<(), RmuxError> {
        let session_names = self.dismiss_mode_tree(attach_pid).await?;
        if let Ok(session_name) = self.attached_session_name(attach_pid).await {
            self.refresh_attached_client(attach_pid, &session_name)
                .await;
            tokio::task::yield_now().await;
        }
        for session_name in session_names {
            self.refresh_attached_session(&session_name).await;
        }
        Ok(())
    }

    pub(in crate::handler) async fn dismiss_mode_tree(
        &self,
        attach_pid: u32,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let Some(mode_session_name) = self.active_mode_tree_session_name(attach_pid).await? else {
            return Ok(Vec::new());
        };
        let removed = {
            let mut active_attach = self.active_attach.lock().await;
            let mut removed = None;
            for active in active_attach.by_pid.values_mut() {
                if active.session_name != mode_session_name {
                    continue;
                }
                let Some(mode) = active.mode_tree.take() else {
                    continue;
                };
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
                if removed.is_none() {
                    removed = Some(mode.clone());
                }
            }
            removed
        };
        let Some(mode) = removed else {
            return Ok(Vec::new());
        };
        if let Some(target) = mode.host_pane.as_ref() {
            if self.clear_mode_tree_for_target(target).await? {
                self.sync_automatic_window_name_for_pane_target(target)
                    .await;
                self.emit_without_attached_refresh(LifecycleEvent::PaneModeChanged {
                    target: target.clone(),
                })
                .await;
            }
        }

        let mut refresh = vec![mode.session_name.clone()];
        if let Some(target) = mode.zoom_restore {
            {
                let mut state = self.state.lock().await;
                state.mutate_session_and_resize_terminals(target.session_name(), |session| {
                    session.toggle_zoom_in_window(target.window_index(), target.pane_index())?;
                    Ok(())
                })?;
            }
            if !refresh.iter().any(|name| name == target.session_name()) {
                refresh.push(target.session_name().clone());
            }
        }
        Ok(refresh)
    }

    pub(super) async fn store_mode_tree_state(
        &self,
        attach_pid: u32,
        mode: ModeTreeClientState,
    ) -> Result<(), RmuxError> {
        let mut active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        let mut stored = false;
        for active in active_attach.by_pid.values_mut() {
            if active.session_name != mode.session_name || active.mode_tree.is_none() {
                continue;
            }
            active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
            active.persistent_overlay_epoch.store(
                active.mode_tree_state_id,
                std::sync::atomic::Ordering::SeqCst,
            );
            active.mode_tree = Some(mode.clone());
            stored = true;
        }
        if !stored {
            return Ok(());
        }
        Ok(())
    }

    pub(super) async fn show_mode_tree_help(&self, attach_pid: u32) -> Result<(), RmuxError> {
        let session_name = self.attached_session_name(attach_pid).await?;
        let (overlay_frame, clear_frame, duration) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            (
                crate::renderer::render_status_message(session, &state.options, MODE_TREE_HELP),
                crate::renderer::render_status_message(session, &state.options, ""),
                std::time::Duration::from_millis(1200),
            )
        };
        let _ = self
            .send_attached_overlay(&session_name, overlay_frame, clear_frame, duration)
            .await;
        Ok(())
    }

    pub(super) async fn mode_tree_active_pane(
        &self,
        session_name: &SessionName,
    ) -> Result<PaneTarget, RmuxError> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        Ok(PaneTarget::with_window(
            session_name.clone(),
            session.active_window_index(),
            session.active_pane_index(),
        ))
    }

    async fn enter_mode_tree_for_target(
        &self,
        target: &PaneTarget,
        kind: ModeTreeKind,
    ) -> Result<bool, RmuxError> {
        let state = self.state.lock().await;
        let transcript = state.transcript_handle(target)?;
        let changed = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .enter_mode_tree(kind.pane_mode_name());
        Ok(changed)
    }

    async fn clear_mode_tree_for_target(&self, target: &PaneTarget) -> Result<bool, RmuxError> {
        let state = self.state.lock().await;
        let transcript = state.transcript_handle(target)?;
        let changed = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .clear_mode_tree();
        Ok(changed)
    }

    pub(super) async fn mode_tree_content_rows(
        &self,
        mode: &ModeTreeClientState,
    ) -> Result<u16, RmuxError> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session(&mode.session_name)
            .ok_or_else(|| session_not_found(&mode.session_name))?;
        let rows = session.window().size().rows;
        let status_on = state
            .options
            .resolve(Some(session.name()), OptionName::Status)
            .map(|value| value != "off")
            .unwrap_or(true);
        Ok(rows.saturating_sub(u16::from(status_on)))
    }
}

fn is_missing_attached_client(error: &RmuxError, command_name: &str) -> bool {
    matches!(
        error,
        RmuxError::Server(message)
            if message == &format!("{command_name} requires an attached client")
    )
}
