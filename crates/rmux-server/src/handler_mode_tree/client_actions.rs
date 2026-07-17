use rmux_core::LifecycleEvent;
use rmux_proto::RmuxError;

use crate::pane_io::AttachControl;

use super::super::RequestHandler;
use super::mode_tree_model::ModeTreeAction;
use super::mode_tree_selection::selected_items;

impl RequestHandler {
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
        let had_tagged_items_before_rebuild = !mode.tagged.is_empty();
        let selected_id_before_rebuild = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if had_tagged_items_before_rebuild && mode.tagged.is_empty() {
            return self.refresh_mode_tree_overlay_if_active(attach_pid).await;
        }
        if mode.tagged.is_empty() {
            if let Some(selected_id) = selected_id_before_rebuild {
                if !build.items.contains_key(&selected_id) {
                    return Ok(());
                }
                mode.selected_id = Some(selected_id);
            }
        }
        let actions = selected_items(&mode, &build)
            .into_iter()
            .map(|item| item.action.clone())
            .collect();
        self.perform_client_detach_actions(attach_pid, actions)
            .await
    }

    pub(super) async fn perform_client_detach_actions(
        &self,
        attach_pid: u32,
        actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        // Detach self last so other detaches complete while we still have state.
        let mut self_detach = None;
        for action in actions {
            let ModeTreeAction::Client {
                pid,
                attach_id,
                control,
            } = action
            else {
                continue;
            };
            if pid == attach_pid && !control {
                self_detach = Some(attach_id);
                continue;
            }
            if control {
                let session = self
                    .exit_control_client_for_identity(pid, attach_id, None)
                    .await?;
                if let Some(session_name) = session {
                    self.emit_client_detached(session_name, pid).await;
                }
            } else if let Ok(session_name) = self
                .send_attach_control_for_client_identity(
                    pid,
                    attach_id,
                    AttachControl::Detach,
                    "detach-client",
                )
                .await
            {
                self.emit_client_detached(session_name, pid).await;
            }
        }
        if let Some(attach_id) = self_detach {
            if let Ok(session_name) = self
                .send_attach_control_for_client_identity(
                    attach_pid,
                    attach_id,
                    AttachControl::Detach,
                    "detach-client",
                )
                .await
            {
                self.emit_client_detached(session_name, attach_pid).await;
            }
            return Ok(());
        }
        self.refresh_mode_tree_overlay_if_active(attach_pid).await
    }

    async fn emit_client_detached(&self, session_name: rmux_proto::SessionName, pid: u32) {
        self.emit(LifecycleEvent::ClientDetached {
            session_name,
            client_name: Some(pid.to_string()),
        })
        .await;
    }
}
