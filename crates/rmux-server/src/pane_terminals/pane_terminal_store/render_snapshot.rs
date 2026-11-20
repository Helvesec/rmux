use super::*;

impl PaneTerminalStore {
    pub(in crate::pane_terminals) fn render_snapshot(&self) -> Self {
        Self {
            sessions: self
                .sessions
                .iter()
                .map(|(name, panes)| {
                    (
                        name.clone(),
                        panes
                            .iter()
                            .map(|(id, terminal)| (*id, terminal.render_snapshot()))
                            .collect(),
                    )
                })
                .collect(),
            session_environments: self.session_environments.clone(),
            #[cfg(test)]
            fail_next_resizes: 0,
            #[cfg(test)]
            resize_calls: HashMap::new(),
            #[cfg(all(test, target_os = "macos"))]
            resize_probe: None,
        }
    }
}
