use rmux_proto::{
    LinkWindowRequest, MoveWindowRequest, MoveWindowTarget, RmuxError, SwapWindowRequest,
};

use crate::pane_terminals::HandlerState;

pub(super) fn require_expected_link_window_identity(
    state: &HandlerState,
    request: &LinkWindowRequest,
) -> Result<(), RmuxError> {
    super::super::require_expected_window_identity(state, &request.source)?;
    super::super::require_expected_window_identity(state, &request.target)
}

pub(super) fn require_expected_move_window_identity(
    state: &HandlerState,
    request: &MoveWindowRequest,
) -> Result<(), RmuxError> {
    if let Some(source) = request.source.as_ref() {
        super::super::require_expected_window_identity(state, source)?;
    }
    match &request.target {
        MoveWindowTarget::Session(session_name) => {
            super::super::require_expected_session_identity(state, session_name)
        }
        MoveWindowTarget::Window(target) => {
            super::super::require_expected_window_identity(state, target)
        }
    }
}

pub(super) fn require_expected_swap_window_identity(
    state: &HandlerState,
    request: &SwapWindowRequest,
) -> Result<(), RmuxError> {
    super::super::require_expected_window_identity(state, &request.source)?;
    super::super::require_expected_window_identity(state, &request.target)
}
