use rmux_core::Session;
use rmux_proto::{RmuxError, SessionName};

use super::HandlerState;

impl HandlerState {
    pub(in crate::pane_terminals) fn replace_session(
        &mut self,
        session_name: &SessionName,
        previous_session: Session,
    ) -> Result<(), RmuxError> {
        let session = self.sessions.session_mut(session_name).ok_or_else(|| {
            RmuxError::Server(format!(
                "failed to roll back session {session_name}: session disappeared"
            ))
        })?;
        *session = previous_session;
        Ok(())
    }

    pub(in crate::pane_terminals) fn restore_session_after_resize_error(
        &mut self,
        session_name: &SessionName,
        previous_session: Session,
        source_error: &RmuxError,
    ) -> Result<(), RmuxError> {
        self.replace_session(session_name, previous_session)?;
        self.resize_terminals(session_name)
            .map_err(|rollback_error| {
                RmuxError::Server(format!(
                "failed to roll back session {session_name} after {source_error}: {rollback_error}"
            ))
            })
    }
}
