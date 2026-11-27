use super::*;

/// A private, immutable read projection. It cannot mutate the live store and
/// owns neither children, readers, pipes nor pending lifecycle operations.
pub(crate) struct WebRenderState(HandlerState);

impl WebRenderState {
    pub(crate) fn state(&self) -> &HandlerState {
        &self.0
    }
}

impl HandlerState {
    pub(crate) fn capture_web_render_state(&self) -> WebRenderState {
        // Arbitrary status formats may iterate sessions/windows/panes or refer
        // to user options and buffers. Retain their metadata, not terminal
        // contents; every transcript remains a shared identity until rendering.
        WebRenderState(Self {
            sessions: self.sessions.render_snapshot(),
            options: self.options.clone(),
            environment: self.environment.clone(),
            buffers: self.buffers.clone(),
            status_jobs: self.status_jobs.clone(),
            startup_config_files: self.startup_config_files.clone(),
            terminals: self.terminals.render_snapshot(),
            #[cfg(windows)]
            starting_panes: self
                .starting_panes
                .iter()
                .map(|(name, panes)| {
                    (
                        name.clone(),
                        panes
                            .iter()
                            .map(|(id, pane)| {
                                (
                                    *id,
                                    StartingPane {
                                        profile: pane.profile.clone(),
                                        runtime_window_name: pane.runtime_window_name.clone(),
                                        generation: pane.generation,
                                        queued_input: VecDeque::new(),
                                        queued_input_bytes: 0,
                                    },
                                )
                            })
                            .collect(),
                    )
                })
                .collect(),
            transcripts: self.transcripts.clone(),
            pane_outputs: self.pane_outputs.clone(),
            pane_output_generations: self.pane_output_generations.clone(),
            pane_lifecycle: self.pane_lifecycle.clone(),
            input_disabled_panes: self.input_disabled_panes.clone(),
            dead_panes: self.dead_panes.clone(),
            marked_pane: self.marked_pane.clone(),
            pipes: self.pipes.render_snapshot(),
            auto_named_windows: self.auto_named_windows.clone(),
            window_link_groups: self.window_link_groups.clone(),
            window_link_slots: self.window_link_slots.clone(),
            window_link_occurrences: self.window_link_occurrences.clone(),
            ..Self::default()
        })
    }
}
