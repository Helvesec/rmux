use super::*;

impl RequestHandler {
    pub(crate) async fn web_send_text(
        &self,
        target: &PaneTargetRef,
        text: String,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        // xterm emits DEL for a Backspace key. Resolve that key through the
        // pane encoder so the configured erase byte matches native attach.
        // Text and paste payloads retain their literal byte representation.
        let (input, literal) = if text == "\x7f" {
            ("BSpace".to_owned(), false)
        } else {
            (text, true)
        };
        let response = self
            .handle_pane_input_ref(PaneInputRequest {
                target: target.clone(),
                keys: vec![input],
                literal,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_send_key(
        &self,
        target: &PaneTargetRef,
        key: String,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let response = self
            .handle_pane_input_ref(PaneInputRequest {
                target: target.clone(),
                keys: vec![key],
                literal: false,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_logout(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_request(
                requester_pid,
                &session_target,
                Request::KillSession(KillSessionRequest {
                    target: session_target.name().clone(),
                    kill_all_except_target: false,
                    clear_alerts: false,
                    kill_group: false,
                }),
            )
            .await;
        self.refresh_hook_identity_aliases().await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_select_pane(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_request(
                requester_pid,
                &session_target,
                Request::PaneSelect(PaneSelectRequest {
                    target: PaneTargetRef::by_id(session_target.name().clone(), pane_id),
                    title: None,
                }),
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_resize_pane(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        pane_id: PaneId,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_request(
                requester_pid,
                &session_target,
                Request::PaneResize(PaneResizeRequest {
                    target: PaneTargetRef::by_id(session_target.name().clone(), pane_id),
                    adjustment,
                }),
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_split_pane(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        direction: SplitDirection,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_request(
                requester_pid,
                &session_target,
                Request::SplitWindow(SplitWindowRequest {
                    target: SplitWindowTarget::Session(session_target.name().clone()),
                    direction,
                    before: false,
                    environment: None,
                }),
            )
            .await;
        self.refresh_hook_identity_aliases().await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_new_window(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_request(
                requester_pid,
                &session_target,
                Request::NewWindow(Box::new(NewWindowRequest {
                    target: session_target.name().clone(),
                    name: None,
                    detached: false,
                    environment: None,
                    command: None,
                    process_command: None,
                    start_directory: None,
                    target_window_index: None,
                    insert_at_target: false,
                })),
            )
            .await;
        self.refresh_hook_identity_aliases().await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_kill_active_pane(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let (session_target, pane_id) = self
            .web_session_active_pane_identity(session_target)
            .await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_request(
                requester_pid,
                &session_target,
                Request::PaneKill(PaneKillRequest {
                    target: PaneTargetRef::by_id(session_target.name().clone(), pane_id),
                    kill_all_except: false,
                }),
            )
            .await;
        self.refresh_hook_identity_aliases().await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_select_window(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let (session_target, window_id) = self
            .current_web_window_identity(session_target, window_index)
            .await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_window_request(
                requester_pid,
                &session_target,
                window_index,
                window_id,
                Request::SelectWindow(SelectWindowRequest {
                    target: WindowTarget::with_window(session_target.name().clone(), window_index),
                }),
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_select_window_for_view(
        &self,
        session_target: &WebSessionTarget,
        attach_pid: u32,
        window_index: u32,
    ) -> Result<Option<WebSessionWindowSelection>, RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let session_target = self.current_web_session_target(session_target).await?;
        let (attached_count, terminal_context, attach_id, render_generation) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("web session attach disappeared".to_owned()))?;
            (
                active_attach.attached_count(session_target.name()),
                active.terminal_context.clone(),
                active.id,
                active.render_generation,
            )
        };
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        let Some(window) = session.window_at(window_index) else {
            return Ok(None);
        };
        let selection = WebSessionWindowSelection::new(window_index, window.id());
        let render_state = WebInitialCapture::capture(&state, session.name(), Some(window_index))?;
        let captured_identity = render_state.identity();
        drop(state);
        let target = render_state.render(
            attached_count,
            &terminal_context,
            &self.socket_path(),
            false,
        )?;
        // Rendering no longer pins global identity. Revalidate before publishing
        // a switch so a deleted/recycled window cannot receive the old capture.
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        selected_web_session_window_index(session, Some(selection))?;
        captured_identity.validate(&state, false)?;
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(RmuxError::Server(
                "web session attach disappeared".to_owned(),
            ));
        };
        if active.id != attach_id
            || active.render_generation != render_generation
            || &active.session_name != session_target.name()
            || active.session_id != session_target.id()
        {
            return Err(RmuxError::Server(
                "web session attach changed sessions".to_owned(),
            ));
        }
        active.render_generation = active.render_generation.saturating_add(1);
        active.render_refresh_pending = false;
        if let Err(error) = active.control_tx.send(AttachControl::switch(target)) {
            if error.is_full() {
                active
                    .closing
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return if error.is_full() {
                Err(RmuxError::Server(
                    "web session attach is not draining updates".to_owned(),
                ))
            } else {
                Err(RmuxError::Server(
                    "web session attach disappeared".to_owned(),
                ))
            };
        }
        Ok(Some(selection))
    }

    pub(crate) async fn web_session_rename_window(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        window_index: u32,
        name: String,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let (session_target, window_id) = self
            .current_web_window_identity(session_target, window_index)
            .await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_window_request(
                requester_pid,
                &session_target,
                window_index,
                window_id,
                Request::RenameWindow(RenameWindowRequest {
                    target: WindowTarget::with_window(session_target.name().clone(), window_index),
                    name,
                }),
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_kill_window(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        let _admission = self.begin_web_mutation()?;
        let (session_target, window_id) = self
            .current_web_window_identity(session_target, window_index)
            .await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .dispatch_web_window_request(
                requester_pid,
                &session_target,
                window_index,
                window_id,
                Request::KillWindow(KillWindowRequest {
                    target: WindowTarget::with_window(session_target.name().clone(), window_index),
                    kill_all_others: false,
                }),
            )
            .await;
        self.refresh_hook_identity_aliases().await;
        response_to_result(response)
    }

    async fn dispatch_web_request(
        &self,
        requester_pid: u32,
        session_target: &WebSessionTarget,
        request: Request,
    ) -> Response {
        crate::handler::dispatch_with_expected_session_identity(
            self,
            requester_pid,
            session_target.name().clone(),
            session_target.id(),
            request,
        )
        .await
    }

    async fn dispatch_web_window_request(
        &self,
        requester_pid: u32,
        session_target: &WebSessionTarget,
        window_index: u32,
        window_id: WindowId,
        request: Request,
    ) -> Response {
        crate::handler::dispatch_with_expected_window_identity(
            self,
            requester_pid,
            session_target.name().clone(),
            session_target.id(),
            window_index,
            window_id,
            request,
        )
        .await
    }
}
