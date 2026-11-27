use super::HandlerState;
use crate::theme::ThemeClient;
use rmux_core::PaneId;

impl HandlerState {
    /// Seed a new reader before its child can query, independently of hooks.
    pub(crate) fn theme_clients_for_pane(&self, pane: PaneId) -> Vec<ThemeClient> {
        self.theme_clients
            .iter()
            .filter(|source| {
                self.sessions.iter().any(|session| {
                    session.id() == source.session
                        && session.window_index_for_pane_id(pane).is_some()
                })
            })
            .map(|source| source.client.clone())
            .collect()
    }
}
