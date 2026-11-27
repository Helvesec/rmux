use rmux_core::WindowLayoutContext;
use rmux_proto::{OptionName, RmuxError, SessionName, WindowTarget};

use super::HandlerState;

impl HandlerState {
    pub(crate) fn mutate_window_layout_and_synchronize_family<T, F>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(WindowLayoutContext<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_store_and_synchronize_window_family_with_snapshot(
            target.session_name(),
            target.window_index(),
            |store| mutate(store.window_layout_context_mut(target)?),
        )
        .map(|(result, sessions, _)| (result, sessions))
    }

    pub(crate) fn mutate_window_layout_and_resize_terminal_with_family<T, F>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(WindowLayoutContext<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_store_and_resize_window_terminal_with_family_if(
            target.session_name(),
            target.window_index(),
            |store| mutate(store.window_layout_context_mut(target)?),
            |_| true,
        )
        .map_err(RmuxError::from)
    }

    /// Runs one `select-layout` family mutation, then refits a
    /// `window-size manual` window inside the same layout transaction.
    ///
    /// tmux 3.7b's `cmd_select_layout_exec` ends every branch that changed the
    /// layout with `recalculate_sizes()`, which puts a manual window back at
    /// its explicit size, held no smaller than the new layout's minimum. When
    /// that refit moved the size, tmux's `resize_window()` fired
    /// `window-resized` even if the window ends where it started, as a custom
    /// layout string of another size does, so the refit records an applied
    /// resize of its own.
    pub(crate) fn mutate_select_layout_and_resize_terminal_with_family<T, F>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(&mut WindowLayoutContext<'_>) -> Result<T, RmuxError>,
    {
        let manual_size = self.window_size_is_manual(target);
        let mut refitted = false;
        let mutated =
            self.mutate_window_layout_and_resize_terminal_with_family(target, |mut layout| {
                let result = mutate(&mut layout)?;
                refitted = manual_size && layout.refit_to_manual_size();
                Ok(result)
            })?;
        if refitted {
            self.record_applied_window_resize(target.clone());
        }
        Ok(mutated)
    }

    /// Whether `target` resolves `window-size manual`, the policy under which
    /// tmux 3.7b's `recalculate_size()` puts the explicit `resize-window` size
    /// back after every layout change.
    pub(crate) fn window_size_is_manual(&self, target: &WindowTarget) -> bool {
        self.options.resolve_for_window(
            target.session_name(),
            target.window_index(),
            OptionName::WindowSize,
        ) == Some("manual")
    }

    /// Refits a surviving `window-size manual` window to its explicit size
    /// after panes left it, as tmux's `recalculate_sizes()` does once the
    /// command that removed them completes. Returns whether the size moved;
    /// the caller still owns the linked-family synchronization and pane
    /// terminal resize that follow, and records the applied resize once they
    /// committed.
    pub(crate) fn refit_manual_window_size(
        &mut self,
        target: &WindowTarget,
    ) -> Result<bool, RmuxError> {
        if !self.window_size_is_manual(target) {
            return Ok(false);
        }
        Ok(self
            .sessions
            .window_layout_context_mut(target)?
            .refit_to_manual_size())
    }
}
