use super::*;
use crate::pane_io::{AttachTarget, PaneOutputReceiver};
use crate::pane_terminals::{HandlerState, WebRenderState};

/// Owns the selection metadata and reserves output before the global lock is
/// released. Recovery, formats, screen painting and native use happen later.
pub(super) struct WebInitialCapture {
    state: WebRenderState,
    identity: WebInitialIdentity,
    session_name: SessionName,
    window_index: Option<u32>,
    output: PaneOutputReceiver,
    sequence: u64,
}

#[derive(Clone)]
pub(super) struct WebInitialIdentity {
    session_id: SessionId,
    window_index: u32,
    window_id: WindowId,
    pane_id: PaneId,
    output_generation: u64,
}

impl WebInitialIdentity {
    pub(super) fn validate(
        &self,
        state: &HandlerState,
        follow_active: bool,
    ) -> Result<(), RmuxError> {
        let changed = || RmuxError::Server("Web target changed while rendering".to_owned());
        let session = state
            .sessions
            .session_by_id(self.session_id)
            .ok_or_else(changed)?;
        let window = session.window_at(self.window_index).ok_or_else(changed)?;
        let pane = window.active_pane().ok_or_else(changed)?;
        if window.id() != self.window_id
            || pane.id() != self.pane_id
            || (follow_active && session.active_window_index() != self.window_index)
            || state
                .pane_output_for_target(session.name(), self.window_index, pane.index())?
                .current_generation()
                != self.output_generation
        {
            return Err(changed());
        }
        Ok(())
    }
}

impl WebInitialCapture {
    pub(super) fn capture(
        state: &HandlerState,
        session_name: &SessionName,
        window_index: Option<u32>,
    ) -> Result<Self, RmuxError> {
        let session = state
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found_web(session_name))?;
        let index = window_index.unwrap_or_else(|| session.active_window_index());
        let window = session
            .window_at(index)
            .ok_or_else(|| RmuxError::Server(format!("Web window {index} disappeared")))?;
        let output =
            state.pane_output_for_target(session_name, index, window.active_pane_index())?;
        let identity = WebInitialIdentity {
            session_id: session.id(),
            window_index: index,
            window_id: window.id(),
            pane_id: window
                .active_pane()
                .expect("output target has an active pane")
                .id(),
            output_generation: output.current_generation(),
        };
        let (sequence, output) = output.subscribe_live_from_now();
        Ok(Self {
            state: state.capture_web_render_state(),
            identity,
            session_name: session_name.clone(),
            window_index,
            output,
            sequence,
        })
    }

    pub(super) fn identity(&self) -> WebInitialIdentity {
        self.identity.clone()
    }

    pub(super) fn render(
        self,
        attached_count: usize,
        terminal_context: &OuterTerminalContext,
        socket_path: &std::path::Path,
        opening: bool,
    ) -> Result<AttachTarget, RmuxError> {
        let mut target = if opening {
            attach_target_for_web_session(
                self.state.state(),
                &self.session_name,
                attached_count,
                terminal_context,
                socket_path,
            )?
        } else {
            attach_render_target_for_session_window(
                self.state.state(),
                &self.session_name,
                self.window_index,
                attached_count,
                terminal_context,
                socket_path,
            )?
        };
        target.pane_output = self.output;
        target.pane_output_start_sequence = self.sequence;
        Ok(target)
    }
}
