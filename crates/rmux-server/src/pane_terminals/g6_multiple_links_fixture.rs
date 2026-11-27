//! A process-free in-memory state for the G6 multiple-links observation.
//!
//! Built only from ordinary product constructors: `HandlerState::default`, the model's own
//! `create_session`, and a pane terminal made by `PaneTerminal::empty`, which the type documents as
//! the form a `split -I` pane owns - a profile and output, no OS process and no PTY. Nothing is
//! forged, no guard is doubled and no runtime scene is created. It lives here because the pane
//! terminal store is private to this module.

use super::HandlerState;
use crate::pane_terminal_process::PaneTerminal;
use crate::pane_transcript::PaneTranscript;
use crate::terminal::TerminalProfile;
use rmux_proto::{PaneId, SessionName, TerminalSize};
use std::path::Path;

impl HandlerState {
    /// A second session of a different size, so a window linked into it resolves its own
    /// rectangle. Built with the same ordinary constructors as the first one.
    pub(crate) fn g6_add_sized_session(
        &mut self,
        session: &SessionName,
        size: TerminalSize,
    ) -> PaneId {
        self.sessions
            .create_session(session.clone(), size)
            .expect("the in-memory model creates its session");
        let (session_id, pane_id) = {
            let created = self.sessions.session(session).expect("session exists");
            let window = created.window_at(0).expect("window 0 exists");
            (
                created.id(),
                window.panes().first().expect("pane exists").id(),
            )
        };
        let profile = TerminalProfile::for_session(
            &self.environment,
            &self.options,
            session,
            session_id.as_u32(),
            Path::new("rmux-g6-multiple-links.sock"),
            None,
            false,
            None,
            Some(pane_id),
            None,
        )
        .expect("an ordinary in-memory terminal profile");
        self.terminals
            .insert_session(session.clone(), pane_id, PaneTerminal::empty(profile))
            .expect("a process-free pane terminal entry");
        pane_id
    }

    /// The session, its single window's identity and its pane, with a process-free terminal entry.
    pub(crate) fn g6_multiple_links_fixture(
        session: &SessionName,
    ) -> (Self, rmux_core::WindowId, PaneId) {
        let mut state = Self::default();
        let size = TerminalSize { cols: 80, rows: 24 };
        state
            .sessions
            .create_session(session.clone(), size)
            .expect("the in-memory model creates its session");
        let (session_id, window_id, pane_id) = {
            let created = state.sessions.session(session).expect("session exists");
            let window = created.window_at(0).expect("window 0 exists");
            (
                created.id(),
                window.id(),
                window.panes().first().expect("pane exists").id(),
            )
        };
        let profile = TerminalProfile::for_session(
            &state.environment,
            &state.options,
            session,
            session_id.as_u32(),
            Path::new("rmux-g6-multiple-links.sock"),
            None,
            false,
            None,
            Some(pane_id),
            None,
        )
        .expect("an ordinary in-memory terminal profile");
        state
            .terminals
            .insert_session(session.clone(), pane_id, PaneTerminal::empty(profile))
            .expect("a process-free pane terminal entry");
        (state, window_id, pane_id)
    }

    /// The ordinary transcript the live input path consults before it decodes anything.
    ///
    /// `transcript_handle` reads `HandlerState::transcripts` under the *canonical runtime* session
    /// name of the window plus the `PaneId`. That registry is distinct from `terminals`, is empty by
    /// default, and is private to this module, which is why installing it belongs here. The key is
    /// taken from the product's own `runtime_session_name_for_window`, so a window that belongs to a
    /// link group is keyed by that group exactly as the runtime keys it - no identity is fabricated
    /// to satisfy the lookup. A fresh transcript carries `mode: None`, so no mode is invented and no
    /// guard is neutralised. Returns the canonical name it used.
    pub(crate) fn g6_install_pane_transcript(
        &mut self,
        session: &SessionName,
        window_index: u32,
        pane_id: PaneId,
        size: TerminalSize,
    ) -> SessionName {
        /// An ordinary scrollback capacity. The observation never fills it.
        const TRANSCRIPT_LIMIT: usize = 2_000;

        let runtime_session_name = self.runtime_session_name_for_window(session, window_index);
        self.transcripts
            .entry(runtime_session_name.clone())
            .or_default()
            .insert(
                pane_id,
                PaneTranscript::shared_for_pane(pane_id, TRANSCRIPT_LIMIT, size),
            );
        runtime_session_name
    }

    /// Establish this pane's mouse tracking mode the ordinary way: by parsing application output.
    ///
    /// `PaneTranscript::mode()` is the screen's own bitmask, which starts `MODE_CURSOR|MODE_WRAP` and
    /// carries no mouse mode - that is why `encode_mouse_event` refuses to encode until an
    /// application asks. DECSET 1000 then 1006 are fed as ordinary output bytes through
    /// `append_bytes` -> `TerminalScreen::feed_with_query_colours` -> the CSI private-mode dispatch,
    /// exactly as a real application would emit them. No bit is written directly, no state is forged
    /// and no guard is touched. Returns the mode before and after the parse so the effect is
    /// observed rather than assumed.
    pub(crate) fn g6_enable_pane_mouse_tracking(
        &self,
        session: &SessionName,
        window_index: u32,
        pane_id: PaneId,
    ) -> (u32, u32) {
        let runtime_session_name = self.runtime_session_name_for_window(session, window_index);
        let transcript = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .expect("the canonical transcript installed for this pane")
            .clone();
        let mut transcript = transcript.lock();
        let before = transcript.mode();
        transcript.append_bytes(b"\x1b[?1000h");
        transcript.append_bytes(b"\x1b[?1006h");
        (before, transcript.mode())
    }
}
