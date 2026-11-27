use super::*;

impl RequestHandler {
    /// Commits the recency of one accepted attached-client interaction.
    ///
    /// This is the single admission boundary for live client input, reached
    /// before the server decides whether the bytes become a prefix, a key
    /// binding, prompt or overlay editing, a copy/clock/mode-tree key,
    /// display-panes selection, a bracketed paste, a clipboard or terminal
    /// response, or a pane write. Committing here is what makes locally
    /// consumed interaction count as session use, matching tmux, which updates
    /// session activity immediately after admitting a key and before any
    /// dispatch decision. Committing after pane I/O instead would silently
    /// drop every key the server handles itself.
    ///
    /// Both counters are advanced under the same two guards, so client order
    /// and session order share one policy and one linearization point:
    /// concurrent clients are ordered by which one acquires these locks first,
    /// not by which pane write finishes first. One logical input advances the
    /// session once here regardless of how many panes it later reaches, so
    /// `synchronize-panes` cannot inflate recency.
    ///
    /// Rejected input never reaches the commit: empty frames are filtered by
    /// the caller, a vanished or closing attach fails, and a client that
    /// cannot write or is read-only returns without advancing anything. That
    /// read-only exclusion is a deliberate RMUX divergence — tmux does count
    /// read-only keys — and is the existing client-activity policy this
    /// boundary reuses rather than a new rule.
    pub(super) async fn record_attached_input_activity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> io::Result<()> {
        // Handler lock order is state before active_attach.
        let mut state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| io_other("attached client disappeared"))?;
        if active.can_write && !active.flags.contains(ClientFlags::READONLY) {
            // The attach owns its current session, which a switch may have
            // changed since `identity` was captured. Requiring the session id
            // to still match keeps a same-name session recreated underneath
            // this client from inheriting the older session's interaction.
            if let Some(session) = state
                .sessions
                .session(&active.session_name)
                .filter(|session| session.id() == active.session_id)
            {
                let name = session.name().clone();
                let _ = state.sessions.touch_activity(&name);
            }
            let sequence = self.next_client_activity_sequence();
            let _ = active_attach.record_client_activity(identity.attach_pid(), sequence);
        }
        drop(active_attach);
        drop(state);
        self.reset_all_attached_lock_timers().await;
        Ok(())
    }

    pub(super) async fn attached_key_table_active(&self, identity: ActiveAttachIdentity) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.key_table_name.is_some()
            })
    }

    pub(in crate::handler::pane_support::pane_attached_input) async fn emit_attached_client_active_if_changed(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) {
        let attach_pid = identity.attach_pid();
        let window_target =
            WindowTarget::with_window(target.session_name().clone(), target.window_index());
        let epoch = self.active_attach_epoch.load(Ordering::Acquire);
        if active_emit_cache
            .as_ref()
            .is_some_and(|(cached_epoch, cached)| {
                *cached_epoch == epoch && cached == &window_target
            })
        {
            return;
        }

        let (should_emit, cache_epoch, client_name) = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(client_name) = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    identity.matches_active(active)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|active| active.client_name.clone())
            else {
                return;
            };
            let changed = active_attach.record_active_client_for_window(attach_pid, target);
            let cache_epoch = if changed {
                self.active_attach_epoch
                    .fetch_add(1, Ordering::AcqRel)
                    .saturating_add(1)
            } else {
                self.active_attach_epoch.load(Ordering::Acquire)
            };
            (changed, cache_epoch, client_name)
        };
        *active_emit_cache = Some((cache_epoch, window_target));
        if should_emit {
            self.emit(LifecycleEvent::ClientActive {
                session_name: target.session_name().clone(),
                client_name: Some(client_name),
            })
            .await;
        }
    }

    pub(super) async fn handle_attached_terminal_control_event(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        event: TerminalControlEvent,
    ) -> io::Result<bool> {
        match event {
            TerminalControlEvent::FocusIn => {
                return self
                    .apply_attached_focus_report(identity, AttachFocusReport::In)
                    .await;
            }
            TerminalControlEvent::FocusOut => {
                return self
                    .apply_attached_focus_report(identity, AttachFocusReport::Out)
                    .await;
            }
            TerminalControlEvent::ClientLightTheme
            | TerminalControlEvent::ClientDarkTheme
            | TerminalControlEvent::ClientBackground(_) => {
                if !self.record_client_theme(identity, event).await {
                    return Ok(false);
                }
                self.refresh_theme_sources().await;
            }
        }
        let Ok((session_name, session_id, client_name)) = self
            .attached_session_identity_and_client_name_for_identity(identity)
            .await
        else {
            return Ok(false);
        };
        if target.session_name() != &session_name {
            return Ok(false);
        }
        let session_name = target.session_name().clone();
        let client_name = Some(client_name);
        let events = match event {
            TerminalControlEvent::ClientLightTheme => {
                vec![LifecycleEvent::ClientLightTheme {
                    session_name,
                    client_name,
                }]
            }
            TerminalControlEvent::ClientDarkTheme => {
                vec![LifecycleEvent::ClientDarkTheme {
                    session_name,
                    client_name,
                }]
            }
            TerminalControlEvent::ClientBackground(_) => Vec::new(),
            TerminalControlEvent::FocusIn | TerminalControlEvent::FocusOut => {
                unreachable!("focus reports returned before theme handling")
            }
        };
        let prepared = {
            let mut state = self.state.lock().await;
            if super::super::ensure_target_session_identity(&state, target, session_id).is_err() {
                return Ok(false);
            }
            events
                .iter()
                .map(|event| prepare_lifecycle_event(&mut state, event))
                .collect::<Vec<_>>()
        };
        for event in prepared {
            self.emit_prepared_and_wait(event).await;
        }
        Ok(false)
    }

    pub(in crate::handler::pane_support::pane_attached_input) async fn clear_attached_focus_alerts(
        &self,
        identity: ActiveAttachIdentity,
    ) {
        let focused_window = {
            let session_name = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&identity.attach_pid())
                    .filter(|active| {
                        identity.matches_active(active)
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|active| active.session_name.clone())
            };
            match session_name {
                Some(session_name) => {
                    let window_index = {
                        let state = self.state.lock().await;
                        state
                            .sessions
                            .session(&session_name)
                            .map(|session| session.active_window_index())
                    };
                    window_index.map(|window_index| (session_name, window_index))
                }
                None => None,
            }
        };
        if let Some((session_name, window_index)) = focused_window {
            let _ = self
                .clear_session_alerts_on_focus(&session_name, window_index)
                .await;
        }
    }
}
