use crate::{PaneId, PaneRef, RmuxError, SessionName};

pub(super) fn parse_error(message: impl Into<String>) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(message.into()))
}

pub(super) fn stale_slot_error(target: &PaneRef) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::invalid_target(
        target.to_proto().to_string(),
        "pane index does not exist in session",
    ))
}

pub(super) fn is_already_closed_error<T: TargetSelector>(error: &RmuxError, target: &T) -> bool {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::SessionNotFound(session),
        } => session == target.session_name().as_str(),
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::InvalidTarget { value, reason },
        } => target.matches_invalid_target(value, reason),
        _ => false,
    }
}

pub(crate) fn is_already_closed_pane_error(error: &RmuxError, target: &PaneRef) -> bool {
    is_already_closed_error(error, target)
}

pub(super) fn is_already_closed_pane_id_error(
    error: &RmuxError,
    session_name: &SessionName,
    pane_id: PaneId,
) -> bool {
    matches!(
        error,
        RmuxError::PaneNotFound {
            session_name: missing_session,
            pane_id: missing_pane,
        } if missing_session == session_name && *missing_pane == pane_id
    )
}

pub(super) trait TargetSelector {
    fn session_name(&self) -> &rmux_proto::SessionName;
    fn matches_invalid_target(&self, value: &str, reason: &str) -> bool;
}

impl TargetSelector for PaneRef {
    fn session_name(&self) -> &rmux_proto::SessionName {
        &self.session_name
    }

    fn matches_invalid_target(&self, value: &str, reason: &str) -> bool {
        let pane_target = self.to_proto().to_string();
        let window_target = format!("{}:{}", self.session_name, self.window_index);
        let mismatched_index_reason = matches!(
            reason,
            "window index does not exist in session" | "pane index does not exist in session"
        );
        mismatched_index_reason && (value == pane_target || value == window_target)
    }
}
