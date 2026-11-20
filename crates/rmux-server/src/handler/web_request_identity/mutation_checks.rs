//! Checks run under the state lock before a mutation: the target still matches the expected identity.

use rmux_proto::{PaneId, PaneTarget, RmuxError, SessionName, WindowTarget};

use crate::pane_terminals::HandlerState;

use super::{ExpectedSessionPolicy, EXPECTED_SESSION_IDENTITY};

pub(in crate::handler) fn require_expected_window_identity(
    state: &HandlerState,
    target: &WindowTarget,
) -> Result<(), RmuxError> {
    super::super::require_expected_stable_window_identity(state, target)?;
    require_expected_session_identity(state, target.session_name())?;
    let expected_window = EXPECTED_SESSION_IDENTITY
        .try_with(|expected| expected.window)
        .ok()
        .flatten();
    let Some(expected_window) = expected_window else {
        return Ok(());
    };
    let matches = expected_window.index == target.window_index()
        && state
            .sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .is_some_and(|window| window.id() == expected_window.id)
        && expected_window.occurrence_id.is_none_or(|occurrence_id| {
            state.window_link_occurrence_id(target.session_name(), target.window_index())
                == Some(occurrence_id)
        });
    if matches {
        Ok(())
    } else {
        Err(RmuxError::invalid_target(
            target.to_string(),
            "window identity changed before mutation",
        ))
    }
}

pub(in crate::handler) fn require_expected_pane_identity(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    super::super::require_expected_stable_pane_identity(state, target)?;
    require_expected_window_identity(
        state,
        &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
    )
}

pub(in crate::handler) fn resolve_expected_window_pane_target(
    state: &HandlerState,
    session_name: &SessionName,
    pane_id: PaneId,
) -> Result<Option<PaneTarget>, RmuxError> {
    let expected_window = EXPECTED_SESSION_IDENTITY
        .try_with(|expected| expected.window)
        .ok()
        .flatten();
    let Some(expected_window) = expected_window else {
        return Ok(None);
    };
    let window_target = WindowTarget::with_window(session_name.clone(), expected_window.index);
    require_expected_window_identity(state, &window_target)?;
    let window = state
        .sessions
        .session(session_name)
        .and_then(|session| session.window_at(expected_window.index))
        .expect("expected window identity was already validated");
    let pane_index = window
        .panes()
        .iter()
        .find(|pane| pane.id() == pane_id)
        .map(rmux_core::Pane::index)
        .ok_or_else(|| RmuxError::pane_not_found(session_name.clone(), pane_id))?;
    let target = PaneTarget::with_window(session_name.clone(), expected_window.index, pane_index);
    if let Some((expected_pane_id, expected_generation)) = expected_window.pane_output_generation {
        let generation_matches = expected_pane_id == pane_id
            && state.pane_output_generation_for_target(&target, pane_id) == expected_generation;
        if !generation_matches {
            return Err(RmuxError::invalid_target(
                target.to_string(),
                "pane output generation changed before mutation",
            ));
        }
    }
    Ok(Some(target))
}

pub(in crate::handler) fn require_expected_session_identity(
    state: &HandlerState,
    session_name: &SessionName,
) -> Result<(), RmuxError> {
    super::super::require_expected_stable_session_identity(state, session_name)?;
    let expected = EXPECTED_SESSION_IDENTITY
        .try_with(|expected| (expected.cursor.borrow().clone(), expected.policy))
        .ok();
    let Some((expected, policy)) = expected else {
        return Ok(());
    };
    if policy == ExpectedSessionPolicy::AttachedCommandQueue && expected.name != *session_name {
        return Ok(());
    }
    let matches = expected.name == *session_name
        && state
            .sessions
            .session(session_name)
            .is_some_and(|session| session.id() == expected.id);
    if matches {
        Ok(())
    } else {
        Err(RmuxError::SessionNotFound(session_name.to_string()))
    }
}
