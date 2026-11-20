//! Propagation of the `scroll-on-clear` window/pane option to pane screens.
//!
//! The option decides whether a whole-screen erase moves the visible rows into
//! scrollback. It is resolved per pane, so every mutation entry point expands
//! its scope to concrete pane targets and re-resolves `pane -> window ->
//! window-global` for each of them, exactly like the other window/pane screen
//! policies.

use rmux_core::PaneId;
use rmux_proto::{OptionName, OptionScopeSelector, PaneTarget, ScopeSelector, SessionName};

use super::HandlerState;

impl HandlerState {
    /// Re-resolves the policy for every pane after a structural mutation that
    /// can change which window a live pane belongs to.
    ///
    /// A transfer moves the pane runtime rather than recreating it, so the
    /// cached policy survives the move and would otherwise keep the value the
    /// pane resolved in its previous window until the next option mutation or
    /// respawn. Every pane is re-resolved because a transfer also renumbers
    /// pane indices in the source and destination windows.
    pub(in crate::pane_terminals) fn refresh_all_transcript_scroll_on_clear(&mut self) {
        let targets = self.all_pane_targets();
        self.refresh_transcript_scroll_on_clear_for_targets(targets);
    }

    pub(crate) fn refresh_transcript_scroll_on_clear_for_option_scope(
        &mut self,
        scope: &OptionScopeSelector,
    ) {
        let targets = self.pane_targets_for_option_scope(scope);
        self.refresh_transcript_scroll_on_clear_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn refresh_transcript_scroll_on_clear_for_legacy_scope(
        &mut self,
        scope: &ScopeSelector,
    ) {
        let targets = self.pane_targets_for_legacy_option_scope(scope);
        self.refresh_transcript_scroll_on_clear_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn scroll_on_clear_for_pane_id(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> bool {
        let Some(session) = self.sessions.session(session_name) else {
            return true;
        };
        let Some(window_index) = session.window_index_for_pane_id(pane_id) else {
            return true;
        };
        let Some(pane_index) = session
            .window_at(window_index)
            .and_then(|window| window.panes().iter().find(|pane| pane.id() == pane_id))
            .map(|pane| pane.index())
        else {
            return true;
        };
        self.scroll_on_clear_for_pane(session_name, window_index, pane_index)
    }

    fn refresh_transcript_scroll_on_clear_for_targets(&mut self, targets: Vec<PaneTarget>) {
        for target in targets {
            let enabled = self.scroll_on_clear_for_pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            );
            let Ok(transcript) = self.transcript_handle(&target) else {
                continue;
            };
            // A poisoned pane is skipped: the option names a scope, not it.
            let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() else {
                continue;
            };
            // The policy only changes what a later erase does, so nothing
            // already rendered is invalidated here. Attached clients are
            // refreshed by the option's own `EFFECT_RENDER` class.
            transcript.set_scroll_on_clear(enabled);
        }
    }

    fn scroll_on_clear_for_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> bool {
        self.options
            .resolve_for_pane(
                session_name,
                window_index,
                pane_index,
                OptionName::ScrollOnClear,
            )
            .is_none_or(|value| value != "off")
    }
}
