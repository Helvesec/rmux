use super::*;

impl RequestHandler {
    pub(super) async fn open_web_session_share(
        &self,
        access: WebShareAccess,
        session_target: WebSessionTarget,
        forwarder_admission: NormalRequestGuard,
    ) -> Result<WebSessionStream, RmuxError> {
        let session_target = self.current_web_session_target(&session_target).await?;
        let (server_transport, client_stream) = pane_io::in_process_attach_pair();
        let attach_pid = self.allocate_web_attach_pid().await?;
        let mut flags = ClientFlags::default();
        let can_write = access.is_operator();
        if !can_write {
            flags = flags.with_read_only();
        }

        let terminal_context = OuterTerminalContext::default();
        let (control_tx, control_rx) = mpsc::unbounded_channel::<AttachControl>();
        let control_backlog = Arc::new(AtomicUsize::new(0));
        let closing = Arc::new(AtomicBool::new(false));
        let persistent_overlay_epoch = Arc::new(AtomicU64::new(0));
        let attached_count = self
            .active_attach
            .lock()
            .await
            .attached_count(session_target.name());
        let (session_target, render_state, scene) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session_by_id(session_target.id())
                .ok_or_else(|| session_not_found_web(session_target.name()))?;
            let current_target = WebSessionTarget::new(session.name().clone(), session.id());
            let render_state = WebInitialCapture::capture(&state, current_target.name(), None)?;
            let scene = WebSessionScene::capture(&state, &current_target, None)?;
            (current_target, render_state, scene)
        };
        let captured_identity = render_state.identity();
        let target =
            render_state.render(attached_count, &terminal_context, &self.socket_path(), true)?;
        let snapshot = scene.render(target.render_frame.clone(), &HashMap::new())?;
        // Bridge registration through the in-process forwarder's final wire
        // drain. Closing the last session removes the active-attach entry
        // before the browser has received its terminal exit frame.
        let attach_forwarder_guard = self.begin_attach_forwarder();
        let attach_identity = self
            .register_virtual_attach_identity_with_access(
                attach_pid,
                session_target.name().clone(),
                Some(session_target.id()),
                AttachRegistration {
                    control_tx,
                    control_backlog: control_backlog.clone(),
                    closing: closing.clone(),
                    persistent_overlay_epoch: persistent_overlay_epoch.clone(),
                    terminal_context: terminal_context.without_theme_reports(),
                    client_title: target
                        .client_title
                        .as_ref()
                        .map(|rendered| rendered.state().clone()),
                    flags,
                    render_stream: true,
                    uid: current_owner_uid(),
                    user: self.server_owner_identity(),
                    can_write,
                    client_size: None,
                },
            )
            .await
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        // Registration establishes notification delivery. Validate after that
        // boundary: changes during the unlocked render must not strand this
        // viewer on a stale window with no corresponding refresh queued.
        let validation = {
            let state = self.state.lock().await;
            let active_attach = self.active_attach.lock().await;
            captured_identity.validate(&state, true).and_then(|()| {
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .filter(|active| {
                        active.id == attach_identity.attach_id()
                            && active.session_id == session_target.id()
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|_| ())
                    .ok_or_else(|| RmuxError::Server("web session attach disappeared".to_owned()))
            })
        };
        if let Err(error) = validation {
            self.finish_attach(attach_pid, attach_identity.attach_id())
                .await;
            return Err(error);
        }
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let mut normal_shutdown = self.normal_request_shutdown_receiver();
        let task_handler = self.clone();
        tokio::spawn(async move {
            let shutdown_relay = tokio::spawn(async move {
                while !*normal_shutdown.borrow() {
                    if normal_shutdown.changed().await.is_err() {
                        return;
                    }
                }
                shutdown_tx.send_replace(());
            });
            let result = pane_io::forward_attach(
                server_transport,
                target,
                Vec::new(),
                shutdown_rx,
                control_rx,
                control_backlog,
                closing,
                persistent_overlay_epoch,
                LiveAttachInputContext::new(Arc::new(task_handler.clone()), attach_identity),
                true,
            )
            .await;
            shutdown_relay.abort();
            let _ = shutdown_relay.await;
            task_handler
                .finish_attach(attach_pid, attach_identity.attach_id())
                .await;
            drop(attach_forwarder_guard);
            drop(forwarder_admission);
            let _ = task_handler.request_shutdown_if_pending();
            if let Err(error) = result {
                tracing::debug!(attach_pid, "web session attach ended: {error}");
            }
        });

        let revoke_rx = access.revoke_receiver();
        let (reader, writer) = tokio::io::split(client_stream);
        Ok(WebSessionStream {
            access,
            attach_pid,
            revoke_rx,
            target: session_target,
            snapshot,
            writer,
            reader: Some(WebSessionAttachReader::new(reader)),
            selected_window: None,
        })
    }

    pub(crate) async fn web_session_snapshot_with_scrolls(
        &self,
        session_target: &WebSessionTarget,
        selected_window: Option<WebSessionWindowSelection>,
        scrolls: &HashMap<PaneId, usize>,
    ) -> Result<WebSessionSnapshot, RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        let attached_count = self
            .active_attach
            .lock()
            .await
            .attached_count(session_target.name());
        let terminal_context = OuterTerminalContext::default();
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        let selected_window_index = selected_web_session_window_index(session, selected_window)?;
        let render_state =
            WebInitialCapture::capture(&state, session.name(), selected_window_index)?;
        let scene = WebSessionScene::capture(&state, &session_target, selected_window)?;
        drop(state);
        let target = render_state.render(
            attached_count,
            &terminal_context,
            &self.socket_path(),
            false,
        )?;
        scene.render(target.render_frame, scrolls)
    }

    pub(crate) async fn validate_web_session_window_selection(
        &self,
        session_target: &WebSessionTarget,
        selection: WebSessionWindowSelection,
    ) -> Result<(), RmuxError> {
        self.with_web_session_window_selection(session_target, Some(selection), || ())
            .await
    }

    /// Validate and publish without allowing a window mutation between them.
    /// The callback only queues owned frames; it must never wait on transport.
    pub(crate) async fn with_web_session_window_selection<T>(
        &self,
        session_target: &WebSessionTarget,
        selection: Option<WebSessionWindowSelection>,
        publish: impl FnOnce() -> T,
    ) -> Result<T, RmuxError> {
        let Some(selection) = selection else {
            return Ok(publish());
        };
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        selected_web_session_window_index(session, Some(selection))?;
        Ok(publish())
    }

    #[cfg(test)]
    pub(crate) async fn web_session_snapshot(
        &self,
        session_target: &WebSessionTarget,
    ) -> Result<WebSessionSnapshot, RmuxError> {
        self.web_session_snapshot_with_scrolls(session_target, None, &HashMap::new())
            .await
    }

    pub(crate) async fn web_session_pane_scroll_frame(
        &self,
        session_target: &WebSessionTarget,
        pane_id: PaneId,
        top_line: usize,
        selected_window: Option<WebSessionWindowSelection>,
    ) -> Result<Option<WebSessionPaneFrame>, RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        let state = self.state.lock().await;
        let scene = WebSessionScene::capture(&state, &session_target, selected_window)?;
        drop(state);
        scene.pane_scroll_frame(pane_id, top_line)
    }

    pub(crate) async fn web_resnapshot(
        &self,
        target: &PaneTargetRef,
    ) -> Result<(WebPaneSnapshot, PaneOutputReceiver), RmuxError> {
        let mut captured = None;
        for _ in 0..crate::handler::pane_stream_support::MAX_SOURCE_CAPTURE_ATTEMPTS {
            let (pane_output, transcript, generation) = {
                let state = self.state.lock().await;
                let target = resolve_pane_target_ref(&state, target)?;
                let pane_output = state.pane_output_for_target(
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )?;
                let transcript = state.transcript_handle(&target)?;
                let generation = pane_output.current_generation();
                (pane_output, transcript, generation)
            };
            let candidate = pane_output.capture_with_observer(|| {
                let transcript = transcript.lock_for_recovery_capture();
                PaneRecoveryDraft::capture(&transcript)
            });
            if candidate.0.generation == generation {
                captured = Some(candidate);
                break;
            };
        }
        let Some((boundary, draft, output)) = captured else {
            return Err(RmuxError::Server(
                "pane process changed repeatedly while capturing web state".to_owned(),
            ));
        };
        // A shared pane link is a read-only *view*: the pane-scoped WebShare
        // protocol has no scrollback at all, so the browser must receive the
        // pane's visible state and never the rows that scrolled out of it
        // before the link was handed out.
        let seed = draft?.materialize_visible_only()?;
        let keyframe = seed.keyframe();
        let screen = seed.screen();
        let size = screen.size();
        debug_assert_eq!((keyframe.cols, keyframe.rows), (size.cols, size.rows));
        debug_assert_eq!(keyframe.alternate, seed.alternate());
        let (cursor_col, cursor_row) = screen.cursor_position();
        let (scroll_top, scroll_bottom) = screen.scroll_region();
        let snapshot = WebPaneSnapshot {
            cols: size.cols,
            rows: size.rows,
            output_sequence: boundary.next_output_sequence,
            // The authoritative bounded keyframe below supersedes the legacy
            // line representation. Avoid rendering and retaining it twice.
            ansi_lines: Vec::new(),
            cursor_row: cursor_row.min(u32::from(size.rows.saturating_sub(1))) as u16,
            cursor_col: cursor_col.min(u32::from(size.cols.saturating_sub(1))) as u16,
            cursor_visible: screen.mode() & mode::MODE_CURSOR != 0,
            mode_bits: screen.mode(),
            cursor_style: screen.cursor_style(),
            alternate: seed.alternate(),
            scroll_top,
            scroll_bottom,
            history_rows_total: keyframe.history_rows_total,
            history_rows_included: keyframe.history_rows_included,
            metadata_complete: keyframe.metadata_complete,
            recovery_keyframe: Some(keyframe.bytes),
        };
        Ok((snapshot, output))
    }
}
