use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn selected_attached_session_size(
        &self,
        session_name: &SessionName,
        incoming_client: Option<IncomingSizeClient>,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        let (
            policy,
            status,
            aggressive_resize,
            linked_sessions,
            session_id,
            active_window_index,
            active_window_id,
            manual_size,
        ) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Err(crate::pane_terminals::session_not_found(session_name));
            };
            let active_window_index = session.active_window_index();
            let active_window_id = session.window().id();
            let policy = policy_from_option_value(state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                session_name,
                active_window_index,
                OptionName::AggressiveResize,
            ) == Some("on");
            (
                policy,
                state
                    .options
                    .resolve(Some(session_name), OptionName::Status)
                    .map(str::to_owned),
                aggressive_resize,
                linked_session_statuses(
                    &state,
                    session_name,
                    active_window_index,
                    aggressive_resize,
                ),
                session.id(),
                active_window_index,
                active_window_id,
                session.window().manual_size(),
            )
        };

        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates = self.incoming_client_candidates(
                &active_attach,
                &linked_sessions,
                incoming_client,
                status.as_deref(),
            );
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None, active_window_id);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_client_size(
                policy,
                candidates,
                &control_candidates,
                manual_size,
            ),
            session_id,
            active_window_index,
            active_window_id,
            policy,
            manual_size,
            status,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client,
            control_candidates,
        })
    }

    /// The attached candidates, with the incoming client counted exactly once.
    ///
    /// `status` is the target session's own resolved value: the incoming client
    /// is joining that session, so it is the status that converts its outer
    /// terminal rows. Under `aggressive-resize` the target need not appear in
    /// its own linked set when it is not currently showing the window, so this
    /// value is resolved directly rather than looked up there.
    fn incoming_client_candidates(
        &self,
        active_attach: &super::super::ActiveAttachState,
        linked_sessions: &LinkedSessions,
        incoming_client: Option<IncomingSizeClient>,
        status: Option<&str>,
    ) -> Vec<AttachedSizeCandidate> {
        let incoming_candidate = incoming_client.and_then(|client| {
            client.size.map(|size| {
                AttachedSizeCandidate::from_terminal(size, client.size_sequence, status)
            })
        });
        attached_size_candidates(
            active_attach,
            linked_sessions,
            incoming_candidate,
            incoming_client.and_then(|client| client.displaced),
        )
    }

    pub(in crate::handler) async fn selected_attached_window_size(
        &self,
        target: &WindowTarget,
        incoming_client: Option<IncomingSizeClient>,
    ) -> Result<AttachedSizeSelection, RmuxError> {
        let (
            policy,
            status,
            aggressive_resize,
            linked_sessions,
            session_id,
            window_id,
            manual_size,
        ) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| crate::pane_terminals::session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
            let policy = policy_from_option_value(state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::WindowSize,
            ));
            let aggressive_resize = state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                OptionName::AggressiveResize,
            ) == Some("on");
            (
                policy,
                state
                    .options
                    .resolve(Some(target.session_name()), OptionName::Status)
                    .map(str::to_owned),
                aggressive_resize,
                linked_session_statuses(
                    &state,
                    target.session_name(),
                    target.window_index(),
                    aggressive_resize,
                ),
                session.id(),
                window.id(),
                window.manual_size(),
            )
        };
        let (candidates, control_candidates, active_attach_epoch) = {
            let active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let candidates = self.incoming_client_candidates(
                &active_attach,
                &linked_sessions,
                incoming_client,
                status.as_deref(),
            );
            let control_candidates =
                control_size_candidates(&active_control, &linked_sessions, None, window_id);
            (
                candidates,
                control_candidates,
                self.active_attach_epoch.load(Ordering::Acquire),
            )
        };
        Ok(AttachedSizeSelection {
            selected_size: selected_client_size(
                policy,
                candidates,
                &control_candidates,
                manual_size,
            ),
            session_id,
            active_window_index: target.window_index(),
            active_window_id: window_id,
            policy,
            manual_size,
            status,
            aggressive_resize,
            linked_sessions,
            active_attach_epoch,
            incoming_client,
            control_candidates,
        })
    }

    pub(in crate::handler) fn attached_size_selection_is_current(
        &self,
        state: &crate::pane_terminals::HandlerState,
        active_attach: &super::super::ActiveAttachState,
        active_control: &super::super::super::ActiveControlState,
        session_name: &SessionName,
        selection: &AttachedSizeSelection,
        require_active_window: bool,
    ) -> bool {
        if self.active_attach_epoch.load(Ordering::Acquire) != selection.active_attach_epoch {
            return false;
        }
        let Some(session) = state.sessions.session(session_name) else {
            return false;
        };
        if !selection.still_exists(session)
            || (require_active_window
                && session.active_window_index() != selection.active_window_index)
        {
            return false;
        }
        let policy = policy_from_option_value(state.options.resolve_for_window(
            session_name,
            selection.active_window_index,
            OptionName::WindowSize,
        ));
        let status = state
            .options
            .resolve(Some(session_name), OptionName::Status);
        let aggressive_resize = state.options.resolve_for_window(
            session_name,
            selection.active_window_index,
            OptionName::AggressiveResize,
        ) == Some("on");
        let current_control_candidates = control_size_candidates(
            active_control,
            &selection.linked_sessions,
            None,
            selection.active_window_id,
        );
        session
            .window_at(selection.active_window_index)
            .is_some_and(|window| window.manual_size() == selection.manual_size)
            && policy == selection.policy
            && status == selection.status.as_deref()
            && aggressive_resize == selection.aggressive_resize
            && linked_session_statuses(
                state,
                session_name,
                selection.active_window_index,
                aggressive_resize,
            ) == selection.linked_sessions
            && current_control_candidates == selection.control_candidates
            && selected_client_size(
                policy,
                self.incoming_client_candidates(
                    active_attach,
                    &selection.linked_sessions,
                    selection.incoming_client,
                    status,
                ),
                &current_control_candidates,
                selection.manual_size,
            ) == selection.selected_size
    }
}
