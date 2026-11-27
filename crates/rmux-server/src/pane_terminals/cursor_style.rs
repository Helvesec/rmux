use rmux_core::PaneId;
use rmux_proto::{OptionName, OptionScopeSelector, PaneTarget, ScopeSelector, SessionName};

use super::HandlerState;

impl HandlerState {
    pub(in crate::pane_terminals) fn refresh_all_transcript_cursor_styles(&mut self) {
        let targets = self.all_pane_targets();
        self.refresh_transcript_cursor_style_for_targets(targets);
    }

    pub(crate) fn refresh_transcript_cursor_style_for_option_scope(
        &mut self,
        scope: &OptionScopeSelector,
    ) {
        let targets = self.pane_targets_for_option_scope(scope);
        self.refresh_transcript_cursor_style_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn refresh_transcript_cursor_style_for_legacy_scope(
        &mut self,
        scope: &ScopeSelector,
    ) {
        let targets = self.pane_targets_for_legacy_option_scope(scope);
        self.refresh_transcript_cursor_style_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn cursor_default_style_for_pane_id(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> u32 {
        let Some(session) = self.sessions.session(session_name) else {
            return 0;
        };
        let Some(window_index) = session.window_index_for_pane_id(pane_id) else {
            return 0;
        };
        let Some(pane_index) = session
            .window_at(window_index)
            .and_then(|window| window.panes().iter().find(|pane| pane.id() == pane_id))
            .map(|pane| pane.index())
        else {
            return 0;
        };
        self.cursor_default_style_for_pane(session_name, window_index, pane_index)
    }

    fn refresh_transcript_cursor_style_for_targets(&mut self, targets: Vec<PaneTarget>) {
        for target in targets {
            let style = self.cursor_default_style_for_pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            );
            if let Ok(transcript) = self.transcript_handle(&target) {
                if let Ok(output) = self.pane_output_for_target(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                ) {
                    // The option names a scope, not this pane: a poisoned
                    // output state skips it instead of failing the request.
                    output.mutate_transcript_in_sweep(
                        &transcript,
                        crate::pane_io::PaneInvalidationReason::TranscriptMutation,
                        |transcript| {
                            let changed = transcript.set_cursor_default_style(style);
                            ((), changed)
                        },
                    );
                } else if let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() {
                    transcript.set_cursor_default_style(style);
                }
            }
        }
    }

    fn cursor_default_style_for_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> u32 {
        rmux_core::cursor_style_from_option(self.options.resolve_for_pane(
            session_name,
            window_index,
            pane_index,
            OptionName::CursorStyle,
        ))
    }
}
