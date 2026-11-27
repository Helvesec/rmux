use rmux_core::{ManagedSessionStore, PaneGeometry, PaneId};
use rmux_proto::{RmuxError, SessionName, TerminalSize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionPane {
    pub(crate) id: PaneId,
    pub(crate) window_index: u32,
    pub(crate) index: u32,
    pub(crate) geometry: PaneGeometry,
    pub(crate) window_size: TerminalSize,
}

pub(crate) fn initial_pane(
    sessions: &ManagedSessionStore,
    session_name: &SessionName,
) -> Result<SessionPane, RmuxError> {
    let session = sessions
        .session(session_name)
        .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?;

    session
        .window()
        .pane(0)
        .map(|pane| SessionPane {
            id: pane.id(),
            window_index: session.active_window_index(),
            index: pane.index(),
            geometry: pane.geometry(),
            window_size: session.window().size(),
        })
        .ok_or_else(|| {
            RmuxError::Server(format!("initial pane missing for session {session_name}"))
        })
}

pub(crate) fn pane_id_for_target(
    sessions: &ManagedSessionStore,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) -> Result<PaneId, RmuxError> {
    let target = sessions
        .session(session_name)
        .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?
        .window_at(window_index)
        .ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{session_name}:{window_index}"),
                "window index does not exist in session",
            )
        })?;

    target
        .pane(pane_index)
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{session_name}:{window_index}.{pane_index}"),
                "pane index does not exist in session",
            )
        })
}

pub(crate) fn missing_pane_terminal(
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) -> RmuxError {
    RmuxError::Server(format!(
        "missing pane terminal for {}:{window_index}.{pane_index}",
        session_name
    ))
}
