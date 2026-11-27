//! Propagation of the `allow-rename` window/pane option to pane screens.
//!
//! The option decides whether a pane's own output may rename its window with
//! `ESC k`. It is resolved per pane, so every mutation entry point expands its
//! scope to concrete pane targets and re-resolves `pane -> window ->
//! window-global` for each of them, exactly like the other window/pane screen
//! policies. It is deliberately independent of `allow-set-title`, which stays
//! the only master of OSC 0/2 and APC titles.

use rmux_core::{PaneId, WindowId};
use rmux_proto::{OptionName, OptionScopeSelector, PaneTarget, ScopeSelector, SessionName};

use super::HandlerState;

impl HandlerState {
    /// Re-resolves the policy for every pane after a structural mutation that
    /// can change which window a live pane belongs to.
    ///
    /// A transfer moves the pane runtime rather than recreating it, so the
    /// cached policy survives the move and would otherwise keep the value the
    /// pane resolved in its previous window. Every pane is re-resolved because
    /// a transfer also renumbers pane indices in both windows.
    pub(in crate::pane_terminals) fn refresh_all_transcript_allow_rename(&mut self) {
        let targets = self.all_pane_targets();
        self.refresh_transcript_allow_rename_for_targets(targets);
    }

    pub(crate) fn refresh_transcript_allow_rename_for_option_scope(
        &mut self,
        scope: &OptionScopeSelector,
    ) {
        let targets = self.pane_targets_for_option_scope(scope);
        self.refresh_transcript_allow_rename_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn refresh_transcript_allow_rename_for_legacy_scope(
        &mut self,
        scope: &ScopeSelector,
    ) {
        let targets = self.pane_targets_for_legacy_option_scope(scope);
        self.refresh_transcript_allow_rename_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn allow_rename_for_pane_id(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> bool {
        let Some(session) = self.sessions.session(session_name) else {
            return false;
        };
        let Some(window_index) = session.window_index_for_pane_id(pane_id) else {
            return false;
        };
        let Some(pane_index) = session
            .window_at(window_index)
            .and_then(|window| window.panes().iter().find(|pane| pane.id() == pane_id))
            .map(|pane| pane.index())
        else {
            return false;
        };
        self.allow_rename_for_pane(session_name, window_index, pane_index)
    }

    fn refresh_transcript_allow_rename_for_targets(&mut self, targets: Vec<PaneTarget>) {
        for target in targets {
            let enabled = self.allow_rename_for_pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            );
            let owner = self.rename_owner_for_pane(&target);
            let Ok(transcript) = self.transcript_handle(&target) else {
                continue;
            };
            // A poisoned pane is skipped: the option names a scope, not it.
            let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() else {
                continue;
            };
            // The policy only decides whether a later `ESC k` is accepted,
            // so nothing already rendered is invalidated here and no name
            // is revisited: turning the option off does not undo a rename
            // the pane obtained while it was on. The owning window travels
            // with it, because a transfer changes both at once.
            transcript.set_rename_policy(enabled, owner);
        }
    }

    /// The window a pane belongs to right now, stamped on every request that
    /// pane's output accepts until the next refresh.
    pub(in crate::pane_terminals) fn rename_owner_for_pane(
        &self,
        target: &PaneTarget,
    ) -> Option<WindowId> {
        self.sessions
            .session(target.session_name())
            .and_then(|session| session.window_at(target.window_index()))
            .map(rmux_core::Window::id)
    }

    pub(in crate::pane_terminals) fn rename_owner_for_pane_id(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<WindowId> {
        let session = self.sessions.session(session_name)?;
        let window_index = session.window_index_for_pane_id(pane_id)?;
        session.window_at(window_index).map(rmux_core::Window::id)
    }

    fn allow_rename_for_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> bool {
        self.options.resolve_for_pane(
            session_name,
            window_index,
            pane_index,
            OptionName::AllowRename,
        ) == Some("on")
    }
}
