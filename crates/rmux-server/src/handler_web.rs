use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rmux_os::identity::UserIdentity;
use rmux_proto::{
    CreateWebShareRequest, ErrorResponse, KillPaneRequest, KillSessionRequest, KillWindowRequest,
    NewWindowRequest, OptionName, PaneInputRequest, PaneResizeRequest, PaneSelectRequest,
    PaneTarget, PaneTargetRef, RenameWindowRequest, ResizePaneAdjustment, Response, RmuxError,
    SelectWindowRequest, SessionId, SessionName, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, WebShareRequest, WebShareScope, WindowTarget,
};
use tokio::sync::{mpsc, watch};

use super::attach_support::{
    attach_render_target_for_session_window, attach_target_for_session, AttachRegistration,
    ClientFlags, ATTACH_CONTROL_BACKLOG_LIMIT,
};
use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{self, AttachControl, LiveAttachInputContext, PaneOutputReceiver};
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::server_access::current_owner_uid;
use crate::web::{
    ResolvedCreateWebShareRequest, WebSessionTarget, WebShareAccess, WebShareExpiryPoll,
    WebShareTarget,
};
use rmux_core::{input::mode, PaneId};

const WEB_ATTACH_PID_BASE: u32 = 0x8000_0000;

#[path = "handler_web_snapshot.rs"]
mod snapshot;
#[path = "handler_web_stream.rs"]
mod stream;

#[cfg(test)]
pub(crate) use snapshot::WebSessionView as TestWebSessionView;
use snapshot::{overlay_pane_lines, session_content_geometry, snapshot_ansi_lines, WebSessionView};
pub(crate) use snapshot::{
    WebPaneSnapshot, WebSessionPaneFrame, WebSessionPaneView, WebSessionSnapshot,
};
pub(crate) use stream::{
    WebPaneStream, WebSessionAttachEvent, WebSessionAttachReader, WebSessionStream, WebShareStream,
};

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn open_web_share(
        &self,
        token: &str,
        pin: Option<&str>,
    ) -> Result<WebShareStream, RmuxError> {
        let token_id = crate::web::SecretHashForCrypto::from_secret(token).token_id();
        self.open_web_share_token_id(&token_id, pin).await
    }

    pub(crate) fn web_settings(&self) -> crate::web::WebShareSettings {
        self.web_shares.settings()
    }

    pub(crate) fn update_web_listener_port(&self, port: u16) {
        self.web_shares.update_listener_port(port);
    }

    pub(crate) fn mark_web_listener_available(&self) {
        self.web_shares.mark_listener_available();
    }

    pub(crate) fn mark_web_listener_unavailable(&self, reason: impl Into<String>) {
        self.web_shares.mark_listener_unavailable(reason);
    }

    pub(crate) async fn ensure_web_share_listener_running(&self) -> Result<(), RmuxError> {
        if self.web_shares.listener_available() {
            return Ok(());
        }
        let _start_guard = self.web_listener_start.lock().await;
        if self.web_shares.listener_available() {
            return Ok(());
        }
        if let Err(error) = crate::web::spawn(Arc::new(self.clone())).await {
            return Err(RmuxError::Server(format!(
                "web-share listener unavailable: {error}"
            )));
        }
        Ok(())
    }

    pub(in crate::handler) async fn handle_web_share(&self, request: WebShareRequest) -> Response {
        let response = match request {
            WebShareRequest::Create(request) => {
                if let Err(error) = self.ensure_web_share_listener_running().await {
                    return Response::Error(ErrorResponse { error });
                }
                let request = match self.resolve_create_web_share(request).await {
                    Ok(request) => request,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
                if let Err(error) = self.web_shares.validate_create_options(request.request()) {
                    return Response::Error(ErrorResponse { error });
                }
                let request = match self.start_web_share_tunnel(request).await {
                    Ok(request) => request,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
                let expiry_kill_target = request.expiry_kill_target();
                match self.web_shares.create(request) {
                    Ok(created) => {
                        self.spawn_web_share_expiry_task(
                            created.share_id.clone(),
                            created.expires_at_unix,
                            expiry_kill_target,
                        );
                        Ok(rmux_proto::WebShareResponse::Created(created))
                    }
                    Err(error) => Err(error),
                }
            }
            WebShareRequest::Config(request) => {
                if let Err(error) = self.ensure_web_share_listener_running().await {
                    return Response::Error(ErrorResponse { error });
                }
                self.web_shares.handle(WebShareRequest::Config(request))
            }
            other => self.web_shares.handle(other),
        };
        match response {
            Ok(response) => Response::WebShare(Box::new(response)),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(crate) async fn open_web_share_token_id(
        &self,
        token_id: &str,
        pin: Option<&str>,
    ) -> Result<WebShareStream, RmuxError> {
        let access = self.web_shares.connect_token_id(token_id, pin).await?;
        self.open_web_share_access(access).await
    }

    pub(crate) fn web_share_pre_auth_token(
        &self,
        token_id: &str,
        origin: &str,
    ) -> Option<(crate::web::SecretHashForCrypto, bool)> {
        self.web_shares.pre_auth_token(token_id, origin)
    }

    pub(in crate::handler) fn prune_web_session(&self, removed: Option<(SessionName, SessionId)>) {
        if let Some((name, id)) = removed {
            self.web_shares.remove_targets_for_sessions(&[(name, id)]);
        }
    }

    async fn open_web_share_access(
        &self,
        access: WebShareAccess,
    ) -> Result<WebShareStream, RmuxError> {
        match access.target().clone() {
            WebShareTarget::Pane(target) => {
                let target = self.stable_web_target(&target).await?;
                let (snapshot, output) = self.web_resnapshot(&target).await?;
                let revoke_rx = access.revoke_receiver();
                Ok(WebShareStream::Pane(Box::new(WebPaneStream {
                    access,
                    output,
                    revoke_rx,
                    snapshot,
                    target,
                })))
            }
            WebShareTarget::Session(session_target) => {
                let stream = self.open_web_session_share(access, session_target).await?;
                Ok(WebShareStream::Session(Box::new(stream)))
            }
        }
    }

    async fn open_web_session_share(
        &self,
        access: WebShareAccess,
        session_target: WebSessionTarget,
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
        let (session_target, target, snapshot) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session_by_id(session_target.id())
                .ok_or_else(|| session_not_found_web(session_target.name()))?;
            let current_target = WebSessionTarget::new(session.name().clone(), session.id());
            let target = attach_target_for_session(
                &state,
                current_target.name(),
                attached_count,
                &terminal_context,
                &self.socket_path(),
            )?;
            let snapshot = web_session_snapshot_from_state(
                &state,
                &current_target,
                target.render_frame.clone(),
                None,
                &HashMap::new(),
            )?;
            (current_target.clone(), target, snapshot)
        };
        let attach_id = self
            .register_attach_with_access(
                attach_pid,
                session_target.name().clone(),
                AttachRegistration {
                    control_tx,
                    control_backlog: control_backlog.clone(),
                    closing: closing.clone(),
                    persistent_overlay_epoch: persistent_overlay_epoch.clone(),
                    terminal_context,
                    flags,
                    render_stream: true,
                    uid: current_owner_uid(),
                    user: UserIdentity::Uid(current_owner_uid()),
                    can_write,
                    client_size: None,
                },
            )
            .await;
        let (_shutdown_tx, shutdown_rx) = watch::channel(());
        let task_handler = self.clone();
        tokio::spawn(async move {
            let _keep_shutdown_open = _shutdown_tx;
            let result = pane_io::forward_attach(
                server_transport,
                target,
                Vec::new(),
                shutdown_rx,
                control_rx,
                control_backlog,
                closing,
                persistent_overlay_epoch,
                LiveAttachInputContext {
                    handler: Arc::new(task_handler.clone()),
                    attach_pid,
                },
                true,
            )
            .await;
            task_handler.finish_attach(attach_pid, attach_id).await;
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
            selected_window_index: None,
        })
    }

    pub(crate) async fn web_session_snapshot_with_scrolls(
        &self,
        session_target: &WebSessionTarget,
        selected_window_index: Option<u32>,
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
        let _lock_span = crate::perf_instrument::span("state_lock_hold")
            .with_str("site", "web_session_snapshot");
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        let selected_window_index =
            selected_web_session_window_index(session, selected_window_index);
        let target = attach_render_target_for_session_window(
            &state,
            session.name(),
            selected_window_index,
            attached_count,
            &terminal_context,
            &self.socket_path(),
        )?;
        let _render_span = crate::perf_instrument::span("render_compose")
            .with_str("site", "web_session_snapshot")
            .with_str("session", session.name().as_str())
            .with_usize("scroll_count", scrolls.len());
        web_session_snapshot_from_state(
            &state,
            &session_target,
            target.render_frame,
            selected_window_index,
            scrolls,
        )
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
        selected_window_index: Option<u32>,
    ) -> Result<Option<WebSessionPaneFrame>, RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        let session = web_session_view_session(session, selected_window_index);
        let session = session.as_ref();
        let window = session.window();
        let active_pane = window.active_pane_index();
        let panes = if window.is_zoomed() {
            window.active_pane().into_iter().collect::<Vec<_>>()
        } else {
            window.panes().iter().collect::<Vec<_>>()
        };
        let Some(pane) = panes.into_iter().find(|pane| pane.id() == pane_id) else {
            return Ok(None);
        };
        let status = state
            .options
            .resolve(Some(session.name()), OptionName::Status);
        let Some(geometry) = session_content_geometry(pane.geometry(), window.size(), status)
        else {
            return Ok(None);
        };
        let Some(scrollback) =
            state.pane_scrollback_view_from_top_line(session.name(), pane.id(), top_line)
        else {
            return Err(RmuxError::Server(format!(
                "missing pane transcript: {}",
                pane.id()
            )));
        };
        if scrollback.scroll_offset == 0 {
            return Ok(None);
        }
        let mouse_on = state
            .with_pane_screen(session.name(), pane.id(), |screen| {
                screen.mode() & mode::ALL_MOUSE_MODES != 0
            })
            .unwrap_or(false);
        let mut frame = Vec::new();
        overlay_pane_lines(&mut frame, geometry, &scrollback.ansi_lines);
        let pane = WebSessionPaneView::new(
            pane.id(),
            geometry,
            pane.index() == active_pane,
            scrollback.history_size,
            scrollback.scroll_offset,
            scrollback.alternate_on,
            mouse_on,
        );
        Ok(Some(WebSessionPaneFrame::new(window.size(), pane, frame)))
    }

    pub(crate) async fn web_resnapshot(
        &self,
        target: &PaneTargetRef,
    ) -> Result<(WebPaneSnapshot, PaneOutputReceiver), RmuxError> {
        let (pane_output, transcript) = {
            let state = self.state.lock().await;
            let target = resolve_pane_target_ref(&state, target)?;
            let pane_output = state.pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )?;
            let transcript = state.transcript_handle(&target)?;
            (pane_output, transcript)
        };
        let (output_sequence, snapshot) = pane_output.capture_with_next_sequence(|| {
            let transcript = match transcript.lock() {
                Ok(transcript) => transcript,
                Err(poisoned) => poisoned.into_inner(),
            };
            let screen = transcript.screen();
            let size = screen.size();
            let (cursor_col, cursor_row) = screen.cursor_position();
            let (scroll_top, scroll_bottom) = screen.scroll_region();
            WebPaneSnapshot {
                cols: size.cols,
                rows: size.rows,
                output_sequence: 0,
                ansi_lines: snapshot_ansi_lines(screen),
                cursor_row: cursor_row.min(u32::from(size.rows.saturating_sub(1))) as u16,
                cursor_col: cursor_col.min(u32::from(size.cols.saturating_sub(1))) as u16,
                cursor_visible: screen.mode() & mode::MODE_CURSOR != 0,
                mode_bits: screen.mode(),
                cursor_style: screen.cursor_style(),
                alternate: screen.is_alternate(),
                scroll_top,
                scroll_bottom,
            }
        });
        let snapshot = WebPaneSnapshot {
            output_sequence,
            ..snapshot
        };
        let output = pane_output.subscribe_from_sequence(output_sequence);
        Ok((snapshot, output))
    }

    pub(crate) async fn web_send_text(
        &self,
        target: &PaneTargetRef,
        text: String,
    ) -> Result<(), RmuxError> {
        let response = self
            .handle_pane_input_ref(PaneInputRequest {
                target: target.clone(),
                keys: vec![text],
                literal: true,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_send_key(
        &self,
        target: &PaneTargetRef,
        key: String,
    ) -> Result<(), RmuxError> {
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
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_kill_session(KillSessionRequest {
                target: session_target.name().clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_select_pane(
        &self,
        session_target: &WebSessionTarget,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_pane_select_ref(PaneSelectRequest {
                target: PaneTargetRef::by_id(session_target.name().clone(), pane_id),
                title: None,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_resize_pane(
        &self,
        session_target: &WebSessionTarget,
        pane_id: PaneId,
        adjustment: ResizePaneAdjustment,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_pane_resize_ref(PaneResizeRequest {
                target: PaneTargetRef::by_id(session_target.name().clone(), pane_id),
                adjustment,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_split_pane(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
        direction: SplitDirection,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_split_window(
                requester_pid,
                SplitWindowRequest {
                    target: SplitWindowTarget::Session(session_target.name().clone()),
                    direction,
                    before: false,
                    environment: None,
                },
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_new_window(
        &self,
        session_target: &WebSessionTarget,
        requester_pid: u32,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_new_window(
                requester_pid,
                NewWindowRequest {
                    target: session_target.name().clone(),
                    name: None,
                    detached: false,
                    environment: None,
                    command: None,
                    process_command: None,
                    start_directory: None,
                    target_window_index: None,
                    insert_at_target: false,
                },
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_kill_active_pane(
        &self,
        session_target: &WebSessionTarget,
    ) -> Result<(), RmuxError> {
        let target = self.web_session_active_pane_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_kill_pane(KillPaneRequest {
                target,
                kill_all_except: false,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_select_window(
        &self,
        session_target: &WebSessionTarget,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_select_window(
                None,
                SelectWindowRequest {
                    target: WindowTarget::with_window(session_target.name().clone(), window_index),
                },
            )
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_select_window_for_view(
        &self,
        session_target: &WebSessionTarget,
        attach_pid: u32,
        window_index: u32,
    ) -> Result<bool, RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        let (attached_count, terminal_context) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("web session attach disappeared".to_owned()))?;
            (
                active_attach.attached_count(session_target.name()),
                active.terminal_context.clone(),
            )
        };
        let target = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session_by_id(session_target.id())
                .ok_or_else(|| session_not_found_web(session_target.name()))?;
            if !session.windows().contains_key(&window_index) {
                return Ok(false);
            }
            attach_render_target_for_session_window(
                &state,
                session.name(),
                Some(window_index),
                attached_count,
                &terminal_context,
                &self.socket_path(),
            )?
        };

        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(RmuxError::Server(
                "web session attach disappeared".to_owned(),
            ));
        };
        if &active.session_name != session_target.name() {
            return Err(RmuxError::Server(
                "web session attach changed sessions".to_owned(),
            ));
        }
        if active.control_backlog.load(Ordering::Acquire) >= ATTACH_CONTROL_BACKLOG_LIMIT {
            let _ = active.control_tx.send(AttachControl::Detach);
            active.closing.store(true, Ordering::SeqCst);
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return Err(RmuxError::Server(
                "web session attach is not draining updates".to_owned(),
            ));
        }
        active.render_generation = active.render_generation.saturating_add(1);
        active.render_refresh_pending = false;
        active.control_backlog.fetch_add(1, Ordering::AcqRel);
        if active
            .control_tx
            .send(AttachControl::switch(target))
            .is_err()
        {
            let _ =
                active
                    .control_backlog
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                        value.checked_sub(1)
                    });
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return Err(RmuxError::Server(
                "web session attach disappeared".to_owned(),
            ));
        }
        Ok(true)
    }

    pub(crate) async fn web_session_rename_window(
        &self,
        session_target: &WebSessionTarget,
        window_index: u32,
        name: String,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_rename_window(RenameWindowRequest {
                target: WindowTarget::with_window(session_target.name().clone(), window_index),
                name,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_kill_window(
        &self,
        session_target: &WebSessionTarget,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let response = self
            .handle_kill_window(KillWindowRequest {
                target: WindowTarget::with_window(session_target.name().clone(), window_index),
                kill_all_others: false,
            })
            .await;
        response_to_result(response)
    }

    fn spawn_web_share_expiry_task(
        &self,
        share_id: String,
        expires_at_unix: Option<u64>,
        kill_target: Option<WebSessionTarget>,
    ) {
        if expires_at_unix.is_none() {
            return;
        }
        let handler = self.clone();
        tokio::spawn(async move {
            // The wire response exposes whole Unix seconds, but the registry
            // retains the exact SystemTime deadline. Poll that exact deadline
            // atomically with record presence so rounding cannot leave an
            // expired share unenforced and stopping a share cannot leak a task.
            let Some(expired) = handler.wait_for_web_share_expiry(&share_id).await else {
                return;
            };
            tracing::info!(share_id = %expired.share_id, "web_share_expired");
            let target = kill_target.or(expired.kill_session);
            if let Some(target) = target {
                if let Err(error) = handler.web_session_logout(&target).await {
                    tracing::debug!(
                        share_id = %expired.share_id,
                        session = %target.name(),
                        "web-share expiry session kill skipped: {error}"
                    );
                }
            }
        });
    }

    async fn wait_for_web_share_expiry(
        &self,
        share_id: &str,
    ) -> Option<crate::web::ExpiredWebShare> {
        loop {
            match self.web_shares.poll_expiry(share_id) {
                WebShareExpiryPoll::Expired(expired) => return Some(expired),
                WebShareExpiryPoll::Pending(deadline) => {
                    tokio::time::sleep(duration_until(deadline)).await;
                }
                WebShareExpiryPoll::Gone => return None,
            }
        }
    }

    async fn resolve_create_web_share(
        &self,
        request: CreateWebShareRequest,
    ) -> Result<ResolvedCreateWebShareRequest, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        let target = match &request.scope {
            WebShareScope::Pane(raw_target) => {
                let target = resolve_pane_target_ref(&state, raw_target)?;
                let pane_id = pane_id_for_target(
                    &state.sessions,
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )?;
                WebShareTarget::pane(PaneTargetRef::by_id(target.session_name().clone(), pane_id))
            }
            WebShareScope::Session(session_name) => {
                let session = state
                    .sessions
                    .session(session_name)
                    .ok_or_else(|| session_not_found_web(session_name))?;
                WebShareTarget::session(session.name().clone(), session.id())
            }
        };
        Ok(ResolvedCreateWebShareRequest::new(request, target))
    }

    async fn start_web_share_tunnel(
        &self,
        request: ResolvedCreateWebShareRequest,
    ) -> Result<ResolvedCreateWebShareRequest, rmux_proto::RmuxError> {
        let provider = match request.tunnel_provider() {
            Some(provider) => provider.to_owned(),
            None => return Ok(request),
        };
        if request.public_base_url().is_some() {
            return Err(RmuxError::Server(
                "web-share --tunnel-url and --tunnel-provider are mutually exclusive".to_owned(),
            ));
        }
        let settings = self.web_shares.settings();
        let tunnel = crate::web::start_tunnel_provider(&provider, &settings).await?;
        Ok(request.with_tunnel(tunnel))
    }

    async fn stable_web_target(&self, target: &PaneTargetRef) -> Result<PaneTargetRef, RmuxError> {
        let state = self.state.lock().await;
        let target = resolve_pane_target_ref(&state, target)?;
        let pane_id = pane_id_for_target(
            &state.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        Ok(PaneTargetRef::by_id(target.session_name().clone(), pane_id))
    }

    pub(crate) async fn web_target_alive(&self, target: &PaneTargetRef) -> bool {
        let state = self.state.lock().await;
        resolve_pane_target_ref(&state, target).is_ok()
    }

    pub(crate) async fn web_session_alive(&self, session_target: &WebSessionTarget) -> bool {
        self.current_web_session_target(session_target)
            .await
            .is_ok()
    }

    async fn current_web_session_target(
        &self,
        session_target: &WebSessionTarget,
    ) -> Result<WebSessionTarget, RmuxError> {
        let state = self.state.lock().await;
        state
            .sessions
            .session_by_id(session_target.id())
            .map(|session| WebSessionTarget::new(session.name().clone(), session.id()))
            .ok_or_else(|| session_not_found_web(session_target.name()))
    }

    async fn web_session_active_pane_target(
        &self,
        session_target: &WebSessionTarget,
    ) -> Result<PaneTarget, RmuxError> {
        let session_target = self.current_web_session_target(session_target).await?;
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session_by_id(session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        let window_index = session.active_window_index();
        let pane_index = session.active_pane_index();
        Ok(PaneTarget::with_window(
            session_target.name().clone(),
            window_index,
            pane_index,
        ))
    }

    async fn allocate_web_attach_pid(&self) -> Result<u32, RmuxError> {
        for _ in 0..1024 {
            let id = self.allocate_connection_id();
            let candidate = WEB_ATTACH_PID_BASE | (id as u32 & !WEB_ATTACH_PID_BASE);
            if !self
                .active_attach
                .lock()
                .await
                .by_pid
                .contains_key(&candidate)
            {
                return Ok(candidate);
            }
        }
        Err(RmuxError::Server(
            "failed to allocate web attach client id".to_owned(),
        ))
    }
}

fn duration_until(deadline: SystemTime) -> Duration {
    deadline
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO)
}

fn session_not_found_web(session_name: &SessionName) -> RmuxError {
    RmuxError::Server(format!("can't find session: {session_name}"))
}

fn response_to_result(response: Response) -> Result<(), RmuxError> {
    match response {
        Response::Error(error) => Err(error.error),
        _ => Ok(()),
    }
}

fn web_session_snapshot_from_state(
    state: &crate::pane_terminals::HandlerState,
    session_target: &WebSessionTarget,
    mut frame: Vec<u8>,
    selected_window_index: Option<u32>,
    scrolls: &HashMap<PaneId, usize>,
) -> Result<WebSessionSnapshot, RmuxError> {
    let session = state
        .sessions
        .session_by_id(session_target.id())
        .ok_or_else(|| session_not_found_web(session_target.name()))?;
    let session = web_session_view_session(session, selected_window_index);
    let session = session.as_ref();
    let window = session.window();
    let mut view = WebSessionView::new(window.size());
    let active_window = session.active_window_index();
    let active_pane = window.active_pane_index();
    for (index, window) in session.windows() {
        view.add_window(*index, window.name(), *index == active_window);
    }
    let panes = if window.is_zoomed() {
        window.active_pane().into_iter().collect::<Vec<_>>()
    } else {
        window.panes().iter().collect::<Vec<_>>()
    };

    // Default to post-reset values so render_dec_modes emits nothing if the
    // active pane's screen is unavailable.
    let mut active_mode_bits = mode::MODE_CURSOR | mode::MODE_WRAP;
    let mut active_cursor_style = 0u32;

    for pane in panes {
        let screen_state = state.pane_screen_state(session.name(), pane.id());
        let mode_bits = screen_state.as_ref().map(|screen| {
            if pane.index() == active_pane {
                active_mode_bits = screen.mode;
                active_cursor_style = screen.cursor_style;
            }
            screen.mode
        });
        let status = state
            .options
            .resolve(Some(session.name()), OptionName::Status);
        let Some(geometry) = session_content_geometry(pane.geometry(), window.size(), status)
        else {
            continue;
        };
        let scrollback = match scrolls.get(&pane.id()).copied() {
            Some(top_line) => {
                state.pane_scrollback_view_from_top_line(session.name(), pane.id(), top_line)
            }
            None => state.pane_scrollback_view(session.name(), pane.id(), 0),
        }
        .ok_or_else(|| RmuxError::Server(format!("missing pane transcript: {}", pane.id())))?;
        if scrollback.scroll_offset > 0 {
            overlay_pane_lines(&mut frame, geometry, &scrollback.ansi_lines);
        }
        view.push_pane(WebSessionPaneView::new(
            pane.id(),
            geometry,
            pane.index() == active_pane,
            scrollback.history_size,
            scrollback.scroll_offset,
            scrollback.alternate_on,
            mode_bits.is_some_and(|bits| bits & mode::ALL_MOUSE_MODES != 0),
        ));
    }

    Ok(WebSessionSnapshot::new(
        window.size(),
        frame,
        view,
        active_mode_bits,
        active_cursor_style,
    ))
}

fn web_session_view_session(
    session: &rmux_core::Session,
    selected_window_index: Option<u32>,
) -> Cow<'_, rmux_core::Session> {
    let Some(window_index) = selected_web_session_window_index(session, selected_window_index)
    else {
        return Cow::Borrowed(session);
    };
    if session.active_window_index() == window_index {
        return Cow::Borrowed(session);
    }

    let mut selected = session.clone();
    selected
        .select_window(window_index)
        .expect("selected web session window was validated above");
    Cow::Owned(selected)
}

fn selected_web_session_window_index(
    session: &rmux_core::Session,
    selected_window_index: Option<u32>,
) -> Option<u32> {
    selected_window_index.filter(|window_index| session.windows().contains_key(window_index))
}

#[cfg(test)]
#[path = "handler_web_tests.rs"]
mod tests;
