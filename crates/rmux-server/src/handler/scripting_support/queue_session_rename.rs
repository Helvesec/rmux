use rmux_proto::{Response, RmuxError, SessionId, SessionName};

use super::super::resolve_existing_session_target;
use super::queue::{QueueExecutionContext, QueueInvocation};
use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QueuedSessionRename {
    session_id: SessionId,
    old_name: SessionName,
    new_name: SessionName,
}

impl QueuedSessionRename {
    pub(super) fn capture(
        invocation: &QueueInvocation,
        state: &HandlerState,
    ) -> Result<Option<Self>, RmuxError> {
        let QueueInvocation::Request(rmux_proto::Request::RenameSession(request)) = invocation
        else {
            return Ok(None);
        };
        let old_name =
            resolve_existing_session_target(&state.sessions, "rename-session", &request.target)?;
        let session_id = state
            .sessions
            .session(&old_name)
            .expect("resolved queued rename target must exist")
            .id();
        Ok(Some(Self {
            session_id,
            old_name,
            new_name: request.new_name.clone(),
        }))
    }

    pub(super) fn commit(self, response: &Response) -> Option<Self> {
        matches!(
            response,
            Response::RenameSession(response) if response.session_name == self.new_name
        )
        .then_some(self)
    }

    pub(super) fn apply(&self, context: &mut QueueExecutionContext) {
        context.rebase_after_session_rename(self.session_id, &self.old_name, &self.new_name);
    }
}
