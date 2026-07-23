use rmux_proto::RmuxError;

use super::super::{RequestHandler, RequesterOrigin};
use super::mode_tree_model::ModeTreeActionIdentity;

impl RequestHandler {
    #[cfg(test)]
    pub(super) async fn mode_tree_origin(
        &self,
        attach_pid: u32,
    ) -> Result<RequesterOrigin, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .and_then(|active| active.mode_tree.as_ref())
            .map(|mode| mode.origin.clone())
            .ok_or_else(|| RmuxError::Server("mode-tree is not active".to_owned()))
    }

    #[cfg(test)]
    pub(super) async fn mode_tree_action_origin(
        &self,
        attach_pid: u32,
    ) -> Result<RequesterOrigin, RmuxError> {
        match self.mode_tree_origin(attach_pid).await {
            Ok(origin) => Ok(origin),
            #[cfg(test)]
            Err(_) => Ok(self.capture_requester_origin(attach_pid).await),
            #[cfg(not(test))]
            Err(error) => Err(error),
        }
    }

    pub(super) async fn mode_tree_origin_for_action_identity(
        &self,
        identity: ModeTreeActionIdentity,
    ) -> Result<RequesterOrigin, RmuxError> {
        Ok(self.mode_tree_for_action_identity(identity).await?.origin)
    }
}
