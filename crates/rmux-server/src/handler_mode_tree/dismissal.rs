use rmux_core::LifecycleEvent;
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeActionIdentity, ModeTreeClientState};
use crate::handler::attach_support::ActiveAttachState;
use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::SharedPaneTranscript;

pub(in crate::handler) struct ModeTreeDismissPlan {
    attach_pid: u32,
    attach_id: u64,
    mode_tree_state_id: u64,
    mode: ModeTreeClientState,
    host_transcript: Option<SharedPaneTranscript>,
}

pub(in crate::handler) struct ModeTreeDismissEffects {
    pane_mode_changed: Option<PaneTarget>,
    refresh_sessions: Vec<SessionName>,
    cleanup_errors: Vec<String>,
}

fn clear_active_mode_trees_for_session(
    active_attach: &mut ActiveAttachState,
    session_name: &SessionName,
) {
    for active in active_attach.by_pid.values_mut() {
        if &active.session_name != session_name || active.mode_tree.take().is_none() {
            continue;
        }
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

impl RequestHandler {
    pub(in crate::handler) fn prepare_mode_tree_dismissal_for_committed_switch(
        &self,
        state: &HandlerState,
        active_attach: &ActiveAttachState,
        attach_pid: u32,
        expected_attach_id: u64,
    ) -> Result<Option<ModeTreeDismissPlan>, RmuxError> {
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| {
                active.id == expected_attach_id
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| attached_client_required("switch-client"))?;
        let Some(mode) = active.mode_tree.clone() else {
            return Ok(None);
        };
        let host_transcript = mode
            .host_pane
            .as_ref()
            .map(|target| state.transcript_handle(target))
            .transpose()?;
        Ok(Some(ModeTreeDismissPlan {
            attach_pid,
            attach_id: expected_attach_id,
            mode_tree_state_id: active.mode_tree_state_id,
            mode,
            host_transcript,
        }))
    }

    pub(in crate::handler) fn apply_committed_mode_tree_dismissal(
        &self,
        state: &mut HandlerState,
        active_attach: &mut ActiveAttachState,
        plan: ModeTreeDismissPlan,
        committed_session_name: &SessionName,
    ) -> ModeTreeDismissEffects {
        let mut effects = ModeTreeDismissEffects {
            pane_mode_changed: None,
            refresh_sessions: Vec::new(),
            cleanup_errors: Vec::new(),
        };
        let source_is_current = active_attach
            .by_pid
            .get(&plan.attach_pid)
            .is_some_and(|active| {
                active.id == plan.attach_id
                    && active.mode_tree_state_id == plan.mode_tree_state_id
                    && active.mode_tree.is_some()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            });
        if !source_is_current {
            effects
                .cleanup_errors
                .push("mode-tree identity changed after committed switch delivery".to_owned());
            return effects;
        }

        effects
            .refresh_sessions
            .push(plan.mode.session_name.clone());
        if committed_session_name != &plan.mode.session_name {
            // The switch frame was necessarily queued before the dismissal
            // committed. A grouped/linked target can share the zoomed source
            // window, so refresh the newly attached identity after cleanup to
            // guarantee its next queued frame reflects the restored layout.
            effects
                .refresh_sessions
                .push(committed_session_name.clone());
        }
        clear_active_mode_trees_for_session(active_attach, &plan.mode.session_name);
        if let (Some(target), Some(transcript)) =
            (plan.mode.host_pane.clone(), plan.host_transcript)
        {
            if transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .clear_mode_tree()
            {
                effects.pane_mode_changed = Some(target);
            }
        }

        if let Some(target) = plan.mode.zoom_restore {
            let restore =
                state.mutate_session_and_resize_terminals(target.session_name(), |session| {
                    session.toggle_zoom_in_window(target.window_index(), target.pane_index())?;
                    Ok(())
                });
            match restore {
                Ok(()) => {
                    if !effects
                        .refresh_sessions
                        .iter()
                        .any(|name| name == target.session_name())
                    {
                        effects.refresh_sessions.push(target.session_name().clone());
                    }
                }
                Err(error) => effects.cleanup_errors.push(format!(
                    "failed to restore mode-tree zoom after committed switch: {error}"
                )),
            }
        }
        effects
    }

    pub(in crate::handler) async fn finish_committed_mode_tree_dismissal(
        &self,
        effects: ModeTreeDismissEffects,
    ) {
        if let Some(target) = effects.pane_mode_changed {
            self.sync_automatic_window_name_for_pane_target(&target)
                .await;
            self.emit_without_attached_refresh(LifecycleEvent::PaneModeChanged { target })
                .await;
        }
        for session_name in effects.refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        for error in effects.cleanup_errors {
            tracing::warn!(error = %error, "committed switch mode-tree cleanup was incomplete");
        }
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

    pub(super) async fn dismiss_mode_tree_with_refresh_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<(), RmuxError> {
        let attach_pid = identity.attach_pid();
        let session_name = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == identity.attach_id()
                        && active.mode_tree_state_id == identity.state_id()
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|active| active.session_name.clone())
                .ok_or_else(|| attached_client_required("choose-buffer"))?
        };
        let session_names = self.dismiss_mode_tree_for_action_identity(identity).await?;
        let _ = self
            .refresh_attached_client_for_identity(
                attach_pid,
                identity.attach_id(),
                &session_name,
                "choose-buffer",
            )
            .await;
        tokio::task::yield_now().await;
        for session_name in session_names {
            self.refresh_attached_session(&session_name).await;
        }
        Ok(())
    }

    pub(in crate::handler) async fn dismiss_mode_tree(
        &self,
        attach_pid: u32,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.dismiss_mode_tree_with_expected_identity(attach_pid, None, None)
            .await
    }

    pub(in crate::handler) async fn dismiss_mode_tree_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.dismiss_mode_tree_with_expected_identity(attach_pid, Some(expected_attach_id), None)
            .await
    }

    pub(super) async fn dismiss_mode_tree_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<Vec<SessionName>, RmuxError> {
        self.dismiss_mode_tree_with_expected_identity(
            identity.attach_pid(),
            Some(identity.attach_id()),
            Some(identity.state_id()),
        )
        .await
    }

    async fn dismiss_mode_tree_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
        expected_state_id: Option<u64>,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let removed = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    expected_attach_id.is_none_or(|expected| active.id == expected)
                        && expected_state_id
                            .is_none_or(|expected| active.mode_tree_state_id == expected)
                        && (expected_attach_id.is_none()
                            || !active.closing.load(std::sync::atomic::Ordering::SeqCst))
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let Some(mode) = active.mode_tree.clone() else {
                return Ok(Vec::new());
            };
            clear_active_mode_trees_for_session(&mut active_attach, &mode.session_name);
            Some(mode)
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
}
