use super::HandlerState;
use rmux_proto::{OptionScopeSelector, PaneTarget, ScopeSelector};

impl HandlerState {
    pub(in crate::pane_terminals) fn pane_targets_for_option_scope(
        &self,
        scope: &OptionScopeSelector,
    ) -> Vec<PaneTarget> {
        match scope {
            OptionScopeSelector::ServerGlobal
            | OptionScopeSelector::SessionGlobal
            | OptionScopeSelector::WindowGlobal => self.all_pane_targets(),
            OptionScopeSelector::Session(name) => self.session_pane_targets(name),
            OptionScopeSelector::Window(target) => {
                self.window_pane_targets(target.session_name(), target.window_index())
            }
            OptionScopeSelector::Pane(target) => vec![target.clone()],
        }
    }
    pub(in crate::pane_terminals) fn pane_targets_for_legacy_option_scope(
        &self,
        scope: &ScopeSelector,
    ) -> Vec<PaneTarget> {
        match scope {
            ScopeSelector::Global => self.all_pane_targets(),
            ScopeSelector::Session(name) => self.session_pane_targets(name),
            ScopeSelector::Window(target) => {
                self.window_pane_targets(target.session_name(), target.window_index())
            }
            ScopeSelector::Pane(target) => vec![target.clone()],
        }
    }
}
