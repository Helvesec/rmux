use rmux_core::PaneId;
use rmux_proto::SessionName;

use super::HandlerState;

impl HandlerState {
    /// Formats read this for every pane a listing or session JSON reaches, so
    /// a poisoned transcript reports no status instead of failing the request.
    // Compile-time regression check: non-Unix builds only read the transcript.
    #[cfg_attr(test, deny(unused_mut))]
    pub(crate) fn pane_program_status(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<rmux_core::ProgramStatus> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        #[cfg(unix)]
        let mut transcript = transcript.lock_for_cross_pane_sweep()?;
        #[cfg(not(unix))]
        let transcript = transcript.lock_for_cross_pane_sweep()?;
        #[cfg(unix)]
        {
            let pane_index = self
                .sessions
                .session(session_name)?
                .window_at(window_index)?
                .panes()
                .iter()
                .find(|pane| pane.id() == pane_id)?
                .index();
            let group = self
                .pane_master_fd(session_name, window_index, pane_index)
                .ok()
                .and_then(rmux_os::process::unix::foreground_pid);
            transcript.observe_foreground_group(group);
        }
        Some(transcript.program_status().clone())
    }
}
