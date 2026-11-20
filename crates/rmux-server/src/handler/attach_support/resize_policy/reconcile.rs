use rmux_core::LifecycleEvent;

use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn reconcile_attached_session_size(
        &self,
        session_name: &SessionName,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let selection = self
                .selected_attached_session_size(session_name, None)
                .await?;
            self.pause_after_attached_size_selection().await;

            let mut state = self.state.lock().await;
            if state.sessions.session(session_name).is_none() {
                return Ok(None);
            }
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
                session_name,
                &selection,
                true,
            ) {
                continue;
            }
            if selection.selected_size.is_none() {
                return Ok(None);
            }
            let session = state
                .sessions
                .session(session_name)
                .expect("stable attached-size selection was revalidated");
            if selection.matches_window(session, selection.active_window_index) {
                return Ok(None);
            }
            self.pause_before_attached_size_apply().await;
            let target =
                WindowTarget::with_window(session_name.clone(), selection.active_window_index);
            state.mutate_attached_window_geometry_and_resize_terminal(&target, |geometry| {
                selection.apply_to_attached_geometry(geometry);
                Ok(())
            })?;
            drop(active_control);
            drop(active_attach);
            return Ok(Some(WindowTarget::with_window(
                session_name.clone(),
                selection.active_window_index,
            )));
        }
        Ok(None)
    }

    /// Publishes what tmux publishes for every window that really did change
    /// size since the last publication.
    ///
    /// tmux 3.7b's `resize_window()` runs `window-layout-changed` and then
    /// `window-resized` for every applied resize, and only the former reaches
    /// control clients (as `%layout-change`). Measured 2026-07-25 on a source
    /// session holding a 101x41 PTY client and a 60x20 control client with
    /// `window-size largest`: after `detach-client` on the PTY client, the
    /// control client receives
    ///     %layout-change @0 a1dd,60x20,0,0,0 a1dd,60x20,0,0,0 *
    /// then `%client-detached /dev/ttys007`; when the PTY client's process dies
    /// instead, the same `%layout-change` arrives after `%client-detached`.
    /// All tickets are reserved under one state lock so they publish in tmux's
    /// order.
    pub(in crate::handler) async fn publish_applied_window_resizes(&self) {
        let prepared = {
            let mut state = self.state.lock().await;
            prepare_applied_window_resize_events(&mut state)
        };
        for event in prepared {
            self.emit_prepared(event).await;
        }
    }

    /// Publishes `target` as an applied resize even when its stored size did not
    /// move, for the commands whose tmux counterpart calls `resize_window()`
    /// unconditionally (`resize-window`).
    pub(in crate::handler) async fn emit_applied_window_resize(&self, target: WindowTarget) {
        let prepared = {
            let mut state = self.state.lock().await;
            state.record_applied_window_resize(target);
            prepare_applied_window_resize_events(&mut state)
        };
        for event in prepared {
            self.emit_prepared(event).await;
        }
    }

    pub(in crate::handler) async fn reconcile_attached_session_size_and_emit(
        &self,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.reconcile_attached_session_size(session_name).await?;
        self.publish_applied_window_resizes().await;
        Ok(())
    }

    pub(in crate::handler) async fn reconcile_attached_window_size(
        &self,
        target: &WindowTarget,
    ) -> Result<Option<WindowTarget>, RmuxError> {
        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let selection = self.selected_attached_window_size(target, None).await?;
            let mut state = self.state.lock().await;
            if state.sessions.session(target.session_name()).is_none() {
                return Ok(None);
            }
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
                target.session_name(),
                &selection,
                false,
            ) {
                continue;
            }
            if selection.selected_size.is_none() {
                return Ok(None);
            }
            let session = state
                .sessions
                .session(target.session_name())
                .expect("stable attached-size session was revalidated");
            if selection.matches_window(session, target.window_index()) {
                return Ok(None);
            }
            self.pause_before_attached_size_apply().await;
            state.mutate_attached_window_geometry_and_resize_terminal(target, |geometry| {
                selection.apply_to_attached_geometry(geometry);
                Ok(())
            })?;
            drop(active_control);
            drop(active_attach);
            return Ok(Some(target.clone()));
        }
        Ok(None)
    }

    pub(in crate::handler) async fn reconcile_attached_window_size_and_emit(
        &self,
        target: &WindowTarget,
    ) -> Result<(), RmuxError> {
        self.reconcile_attached_window_size(target).await?;
        self.publish_applied_window_resizes().await;
        Ok(())
    }
}

/// Reserves the `window-layout-changed` / `window-resized` pair for every
/// window the geometry mutation paths recorded, in tmux's order, under one
/// state lock.
pub(in crate::handler) fn prepare_applied_window_resize_events(
    state: &mut crate::pane_terminals::HandlerState,
) -> Vec<crate::handler::QueuedLifecycleEvent> {
    let targets = state.take_applied_window_resizes();
    let mut prepared = Vec::with_capacity(targets.len().saturating_mul(2));
    for resize in targets {
        let (target, layout_change_prepared, _) = resize.into_parts();
        if !layout_change_prepared {
            prepared.push(crate::handler::prepare_lifecycle_event(
                state,
                &LifecycleEvent::WindowLayoutChanged {
                    target: target.clone(),
                },
            ));
        }
        prepared.push(crate::handler::prepare_lifecycle_event(
            state,
            &LifecycleEvent::WindowResized { target },
        ));
    }
    prepared
}
