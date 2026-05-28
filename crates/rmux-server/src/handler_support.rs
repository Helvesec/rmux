use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{RmuxError, ScopeSelector, SessionName};

use crate::pane_terminals::{session_not_found, HandlerState};

pub(crate) fn attached_client_required(command_name: &str) -> RmuxError {
    RmuxError::Server(format!("{command_name} requires an attached client"))
}

/// Lists the candidate PIDs and tells the caller how to pick one.
/// Surfaces when running `rmux detach` (or another client-targeted
/// command) from inside an rmux session where two or more clients are
/// simultaneously attached — the server can't infer which one the
/// requester means just from the socket peer's PID. Listing the
/// candidates makes the workaround obvious without forcing the user
/// to consult `list-clients`.
pub(crate) fn ambiguous_attached_client_listing(
    command_name: &str,
    attach_pids: &[u32],
    control_pids: &[u32],
) -> RmuxError {
    // Sort each family so the error message is stable across runs.
    // Source maps are `HashMap`, so caller-side ordering is
    // non-deterministic; without this the message changes between
    // process invocations and tests/asserts/log scrapes get flaky.
    let mut sorted_attach = attach_pids.to_vec();
    sorted_attach.sort_unstable();
    let mut sorted_control = control_pids.to_vec();
    sorted_control.sort_unstable();
    let mut all = Vec::with_capacity(sorted_attach.len() + sorted_control.len());
    all.extend(sorted_attach.iter().map(|pid| format!("{pid}")));
    all.extend(sorted_control.iter().map(|pid| format!("{pid} (control)")));
    let joined = all.join(", ");
    RmuxError::Server(format!(
        "{command_name}: {} clients attached ({joined}); pick one with `-t <client>` \
         or address a whole session with `-s <session>`",
        sorted_attach.len() + sorted_control.len(),
    ))
}

pub(crate) fn ensure_scope_session_exists(
    state: &HandlerState,
    scope: &ScopeSelector,
) -> Result<(), RmuxError> {
    match scope {
        ScopeSelector::Global => Ok(()),
        ScopeSelector::Session(session_name) => {
            if state.sessions.contains_session(session_name) {
                Ok(())
            } else {
                Err(session_not_found(session_name))
            }
        }
        ScopeSelector::Window(target) => {
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            if session.window_at(target.window_index()).is_some() {
                Ok(())
            } else {
                Err(RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                ))
            }
        }
        ScopeSelector::Pane(target) => {
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{}:{}", target.session_name(), target.window_index()),
                    "window index does not exist in session",
                )
            })?;
            if window.pane(target.pane_index()).is_some() {
                Ok(())
            } else {
                Err(RmuxError::invalid_target(
                    target.to_string(),
                    "pane index does not exist in session",
                ))
            }
        }
    }
}

/// Rejects a pane operation when the addressed session is in passthrough
/// mode. Passthrough sessions are single-window/single-pane by contract
/// and any pane split/swap/kill/etc. would break that invariant.
///
/// Returns `Ok(())` if the session is missing — that case is left to the
/// existing not-found error paths in each handler. Passthrough-ness is
/// only enforced when the session is known to exist.
pub(crate) fn reject_pane_op_in_passthrough(
    state: &HandlerState,
    session_name: &SessionName,
    op: &str,
) -> Result<(), RmuxError> {
    if !state.sessions.contains_session(session_name) {
        return Ok(());
    }
    rmux_core::reject_pane_op_if_passthrough(&state.options, session_name, op)
}

pub(crate) fn ensure_option_scope_exists(
    state: &HandlerState,
    scope: &OptionScopeSelector,
) -> Result<(), RmuxError> {
    match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => Ok(()),
        OptionScopeSelector::Session(session_name) => {
            if state.sessions.contains_session(session_name) {
                Ok(())
            } else {
                Err(session_not_found(session_name))
            }
        }
        OptionScopeSelector::Window(target) => {
            ensure_scope_session_exists(state, &ScopeSelector::Window(target.clone()))
        }
        OptionScopeSelector::Pane(target) => {
            ensure_scope_session_exists(state, &ScopeSelector::Pane(target.clone()))
        }
    }
}
