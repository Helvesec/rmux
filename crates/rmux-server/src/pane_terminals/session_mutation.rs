use rmux_core::Session;
use rmux_proto::{RmuxError, SessionName};

use super::{session_not_found, HandlerState};

impl HandlerState {
    pub(crate) fn mutate_session_and_resize_terminals<T, F>(
        &mut self,
        session_name: &SessionName,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(&mut Session) -> Result<T, RmuxError>,
    {
        let previous_session = self
            .sessions
            .session(session_name)
            .cloned()
            .ok_or_else(|| session_not_found(session_name))?;
        let result = {
            let session = self
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            mutate(session)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.replace_session(session_name, previous_session)?;
                return Err(error);
            }
        };

        if let Err(error) = self.resize_terminals(session_name) {
            self.restore_session_after_resize_error(session_name, previous_session, &error)?;
            return Err(error);
        }

        self.synchronize_session_group_from(session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(session_name);

        Ok(result)
    }
}
