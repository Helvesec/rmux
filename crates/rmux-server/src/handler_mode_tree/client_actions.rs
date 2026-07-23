use rmux_core::LifecycleEvent;
use rmux_proto::RmuxError;

use crate::pane_io::AttachControl;

use super::super::RequestHandler;
use super::mode_tree_model::{ModeTreeAction, ModeTreeActionIdentity};
use super::mode_tree_selection::selected_items;

impl RequestHandler {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn perform_client_detach(&self, attach_pid: u32) -> Result<(), RmuxError> {
        let identity = self.current_mode_tree_action_identity(attach_pid).await?;
        self.perform_client_detach_for_identity(identity).await
    }

    pub(super) async fn perform_client_detach_for_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<(), RmuxError> {
        let attach_pid = identity.attach_pid();
        let mut mode = self.mode_tree_for_action_identity(identity).await?;
        let _access = self.require_requester_origin_write(&mode.origin).await?;
        let had_tagged_items_before_rebuild = !mode.tagged.is_empty();
        let selected_id_before_rebuild = mode.selected_id.clone();
        let build = self.build_mode_tree(&mut mode, attach_pid).await?;
        if had_tagged_items_before_rebuild && mode.tagged.is_empty() {
            return self
                .refresh_mode_tree_overlay_for_action_identity(identity)
                .await;
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
        self.perform_client_detach_actions_for_identity(identity, actions)
            .await
    }

    pub(super) async fn perform_client_detach_actions_for_identity(
        &self,
        identity: ModeTreeActionIdentity,
        actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        let origin = self.mode_tree_origin_for_action_identity(identity).await?;
        let _access = self.require_requester_origin_write(&origin).await?;
        self.perform_client_detach_actions_inner(identity, actions)
            .await
    }

    async fn perform_client_detach_actions_inner(
        &self,
        expected_requester: ModeTreeActionIdentity,
        actions: Vec<ModeTreeAction>,
    ) -> Result<(), RmuxError> {
        let attach_pid = expected_requester.attach_pid();
        #[cfg(test)]
        super::mode_tree_test_support::pause_mode_tree_identity(
            super::mode_tree_test_support::ModeTreeIdentityPausePoint::Mutation(attach_pid),
        )
        .await;
        // Detach self last so other detaches complete while we still have state.
        let mut self_detach = None;
        for action in actions {
            let _ = self
                .mode_tree_for_action_identity(expected_requester)
                .await?;
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
                let outcome = self
                    .exit_control_client_for_identity_from_mode_tree(
                        expected_requester,
                        pid,
                        attach_id,
                        None,
                    )
                    .await?;
                if let Some(event) = outcome.lifecycle_event {
                    self.emit_prepared(event).await;
                }
            } else if let Ok(session_name) = self
                .send_attach_control_for_client_identity_from_mode_tree(
                    expected_requester,
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
                .send_attach_control_for_client_identity_from_mode_tree(
                    expected_requester,
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
        self.refresh_mode_tree_overlay_for_action_identity(expected_requester)
            .await
    }

    async fn emit_client_detached(&self, session_name: rmux_proto::SessionName, pid: u32) {
        self.emit(LifecycleEvent::ClientDetached {
            session_name,
            client_name: Some(pid.to_string()),
        })
        .await;
    }
}
