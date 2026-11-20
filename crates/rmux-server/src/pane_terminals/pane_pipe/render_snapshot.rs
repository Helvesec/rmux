use super::*;

impl PanePipeStore {
    pub(crate) fn render_snapshot(&self) -> Self {
        Self {
            sessions: HashMap::new(),
            render_memberships: self
                .sessions
                .iter()
                .flat_map(|(name, panes)| panes.keys().map(|id| (name.clone(), *id)))
                .collect(),
        }
    }
}
