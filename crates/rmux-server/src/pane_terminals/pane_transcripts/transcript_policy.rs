//! Option-driven transcript policy: history limits, UTF-8, input,
//! alternate-screen and title-rename settings per pane.

use rmux_core::{PaneId, Utf8Config};
use rmux_proto::{OptionName, OptionScopeSelector, PaneTarget, ScopeSelector, SessionName};

use crate::pane_io::PaneInvalidationReason;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::PaneTranscript;

impl HandlerState {
    pub(crate) fn refresh_transcript_limits_for_scope(
        &mut self,
        scope: &ScopeSelector,
        option: OptionName,
    ) {
        match option {
            OptionName::HistoryLimit => match scope {
                ScopeSelector::Global | ScopeSelector::Session(_) => {
                    // tmux options_push_changes updates every session after a
                    // history-limit mutation, including masked and sibling ones.
                    for session_name in self.transcripts.keys().cloned().collect::<Vec<_>>() {
                        self.refresh_transcript_limits(&session_name);
                    }
                }
                ScopeSelector::Window(_) | ScopeSelector::Pane(_) => {}
            },
            OptionName::CodepointWidths | OptionName::VariationSelectorAlwaysWide => {
                if matches!(scope, ScopeSelector::Global) {
                    self.refresh_transcript_utf8_config();
                }
            }
            OptionName::InputBufferSize | OptionName::ExtendedKeys => {
                if matches!(scope, ScopeSelector::Global) {
                    self.refresh_transcript_input_policies();
                }
            }
            OptionName::AlternateScreen => {
                self.refresh_transcript_alternate_screen_for_legacy_scope(scope);
            }
            OptionName::ScrollOnClear => {
                self.refresh_transcript_scroll_on_clear_for_legacy_scope(scope);
            }
            OptionName::AllowSetTitle => {
                self.refresh_transcript_title_rename_for_legacy_scope(scope);
            }
            OptionName::AllowRename => {
                self.refresh_transcript_allow_rename_for_legacy_scope(scope);
            }
            OptionName::CursorStyle => {
                self.refresh_transcript_cursor_style_for_legacy_scope(scope);
            }
            _ => {}
        }
    }

    pub(crate) fn refresh_transcript_alternate_screen_for_option_scope(
        &mut self,
        scope: &OptionScopeSelector,
    ) {
        let targets = self.pane_targets_for_option_scope(scope);
        self.refresh_transcript_alternate_screen_for_targets(targets);
    }

    pub(crate) fn refresh_transcript_title_rename_for_option_scope(
        &mut self,
        scope: &OptionScopeSelector,
    ) {
        let targets = self.pane_targets_for_option_scope(scope);
        self.refresh_transcript_title_rename_for_targets(targets);
    }

    fn refresh_transcript_alternate_screen_for_legacy_scope(&mut self, scope: &ScopeSelector) {
        let targets = self.pane_targets_for_legacy_option_scope(scope);
        self.refresh_transcript_alternate_screen_for_targets(targets);
    }

    fn refresh_transcript_title_rename_for_legacy_scope(&mut self, scope: &ScopeSelector) {
        let targets = self.pane_targets_for_legacy_option_scope(scope);
        self.refresh_transcript_title_rename_for_targets(targets);
    }

    pub(in crate::pane_terminals) fn history_limit_for_session(
        &self,
        session_name: &SessionName,
    ) -> usize {
        self.options
            .resolve(Some(session_name), OptionName::HistoryLimit)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2000)
    }

    /// Applies the session's history limit to its transcripts after a real
    /// `history-limit` mutation, like tmux `session_update_history`.
    ///
    /// The setter runs even when the effective value is unchanged: tmux
    /// collects one row at equality for positive limits too. Nothing else
    /// applies a session's limit to an existing pane, so a pane moved to
    /// another session keeps the limit it was born with until the next
    /// mutation, as in tmux 3.7b.
    fn refresh_transcript_limits(&mut self, session_name: &SessionName) {
        let limit = self.history_limit_for_session(session_name);
        let runtime_session_name = self.runtime_session_name(session_name);
        let transcripts = self
            .transcripts
            .get(&runtime_session_name)
            .map(|transcripts| {
                transcripts
                    .iter()
                    .map(|(pane_id, transcript)| (*pane_id, transcript.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (pane_id, transcript) in transcripts {
            if let Some(output) = self
                .pane_outputs
                .get(&runtime_session_name)
                .and_then(|panes| panes.get(&pane_id))
                .cloned()
            {
                let set_limit = |transcript: &mut PaneTranscript| {
                    let changed_limit = transcript.history_limit() != limit;
                    // A repeated limit still runs the collector, but only
                    // a real limit or history change is worth publishing.
                    //
                    // The setter clears the synchronized-output snapshot as
                    // well as the active grid, and the two can hold
                    // different histories: under DEC 2026 a presented row
                    // survives an `ED3` that already emptied the active
                    // grid. Comparing only the active size would drop that
                    // row from every subscriber without ever waking one, so
                    // both views are compared under the same lock.
                    let before = (
                        transcript.history_size(),
                        transcript.presentation_history_size(),
                    );
                    transcript.set_limit(limit);
                    let after = (
                        transcript.history_size(),
                        transcript.presentation_history_size(),
                    );
                    ((), changed_limit || after != before)
                };
                // The option sweep reaches every pane of every session; one
                // whose output state is poisoned is skipped.
                output.mutate_transcript_in_sweep(
                    &transcript,
                    PaneInvalidationReason::TranscriptMutation,
                    set_limit,
                );
            } else {
                transcript.lock().set_limit(limit);
            }
        }
    }

    fn refresh_transcript_utf8_config(&mut self) {
        let utf8_config = Utf8Config::from_options(&self.options);
        for transcripts in self.transcripts.values_mut() {
            for transcript in transcripts.values() {
                if let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() {
                    transcript.set_utf8_config(utf8_config.clone());
                }
            }
        }
    }

    fn refresh_transcript_input_policies(&mut self) {
        let limit = self.input_buffer_limit();
        let extended = self.options.resolve(None, OptionName::ExtendedKeys);
        for transcripts in self.transcripts.values_mut() {
            for transcript in transcripts.values() {
                let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() else {
                    continue;
                };
                transcript.set_input_buffer_limit(limit);
                transcript.set_extended_key_policy(extended);
            }
        }
    }

    fn refresh_transcript_alternate_screen_for_targets(&mut self, targets: Vec<PaneTarget>) {
        for target in targets {
            let enabled = self.alternate_screen_enabled_for_pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            );
            let Ok(transcript) = self.transcript_handle(&target) else {
                continue;
            };
            let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() else {
                continue;
            };
            transcript.set_alternate_screen_enabled(enabled);
        }
    }

    fn refresh_transcript_title_rename_for_targets(&mut self, targets: Vec<PaneTarget>) {
        for target in targets {
            let enabled = self.title_rename_enabled_for_pane(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            );
            let Ok(transcript) = self.transcript_handle(&target) else {
                continue;
            };
            let Some(mut transcript) = transcript.lock_for_cross_pane_sweep() else {
                continue;
            };
            transcript.set_title_rename_enabled(enabled);
        }
    }

    pub(in crate::pane_terminals) fn alternate_screen_enabled_for_pane_id(
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
        self.alternate_screen_enabled_for_pane(session_name, window_index, pane_index)
    }

    pub(in crate::pane_terminals) fn title_rename_enabled_for_pane_id(
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
        self.title_rename_enabled_for_pane(session_name, window_index, pane_index)
    }

    fn alternate_screen_enabled_for_pane(
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
                OptionName::AlternateScreen,
            )
            .is_none_or(|value| value != "off")
    }

    fn title_rename_enabled_for_pane(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> bool {
        self.options.resolve_for_pane(
            session_name,
            window_index,
            pane_index,
            OptionName::AllowSetTitle,
        ) == Some("on")
    }
}
