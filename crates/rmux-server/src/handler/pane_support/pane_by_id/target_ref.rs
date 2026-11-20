//! Resolution of a slot- or id-addressed pane reference to a concrete pane target.

use rmux_proto::{PaneTarget, PaneTargetRef, RmuxError};

use crate::pane_terminals::{session_not_found, HandlerState};

pub(crate) fn resolve_pane_target_ref(
    state: &HandlerState,
    target: &PaneTargetRef,
) -> Result<PaneTarget, RmuxError> {
    match target {
        PaneTargetRef::Slot(target) => Ok(target.clone()),
        PaneTargetRef::Id {
            session_name,
            pane_id,
        } => {
            match super::super::super::resolve_expected_window_pane_target(
                state,
                session_name,
                *pane_id,
            )? {
                Some(target) => Ok(target),
                None => resolve_pane_id(state, session_name, *pane_id),
            }
        }
    }
}

fn resolve_pane_id(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    pane_id: rmux_proto::PaneId,
) -> Result<PaneTarget, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let window_index = session
        .window_index_for_pane_id(pane_id)
        .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), pane_id))?;
    let pane_index = session
        .window_at(window_index)
        .and_then(|window| {
            window
                .panes()
                .iter()
                .find(|pane| pane.id() == pane_id)
                .map(|pane| pane.index())
        })
        .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), pane_id))?;
    Ok(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane_index,
    ))
}
