use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use rmux_os::identity::UserIdentity;
use rmux_proto::{
    CreateWebShareRequest, ErrorResponse, KillSessionRequest, PaneInputRequest, PaneResizeRequest,
    PaneTarget, PaneTargetRef, ResizePaneAdjustment, Response, RmuxError, SessionName,
    WebShareRequest, WebShareScope,
};
use tokio::sync::{mpsc, watch};

use super::attach_support::{attach_target_for_session, AttachRegistration, ClientFlags};
use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{self, AttachControl, LiveAttachInputContext, PaneOutputReceiver};
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::server_access::current_owner_uid;
use crate::web::{ResolvedCreateWebShareRequest, WebSessionTarget, WebShareAccess, WebShareTarget};
use rmux_core::input::mode;

const WEB_ATTACH_PID_BASE: u32 = 0x8000_0000;

#[path = "handler_web_snapshot.rs"]
mod snapshot;
#[path = "handler_web_stream.rs"]
mod stream;

use snapshot::snapshot_ansi_lines;
pub(crate) use snapshot::WebPaneSnapshot;
pub(crate) use stream::{WebPaneStream, WebSessionAttachReader, WebSessionStream, WebShareStream};

impl RequestHandler {
    pub(crate) fn web_listener(&self) -> rmux_proto::WebShareListener {
        self.web_shares.listener()
    }

    pub(crate) fn mark_web_listener_available(&self) {
        self.web_shares.mark_listener_available();
    }

    pub(crate) fn mark_web_listener_unavailable(&self, reason: impl Into<String>) {
        self.web_shares.mark_listener_unavailable(reason);
    }

    pub(in crate::handler) async fn handle_web_share(&self, request: WebShareRequest) -> Response {
        let response = match request {
            WebShareRequest::Create(request) => {
                let request = match self.resolve_create_web_share(request).await {
                    Ok(request) => request,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
                self.web_shares
                    .create(request)
                    .map(rmux_proto::WebShareResponse::Created)
            }
            other => self.web_shares.handle(other),
        };
        match response {
            Ok(response) => Response::WebShare(response),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(crate) async fn open_web_share(
        &self,
        token: &str,
        pin: Option<&str>,
    ) -> Result<WebShareStream, RmuxError> {
        let access = self.web_shares.connect(token, pin).await?;
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

    pub(crate) fn known_web_share_origin_allowed(&self, token: &str, origin: &str) -> Option<bool> {
        self.web_shares.known_token_origin_allowed(token, origin)
    }

    async fn open_web_session_share(
        &self,
        access: WebShareAccess,
        session_target: WebSessionTarget,
    ) -> Result<WebSessionStream, RmuxError> {
        let (server_transport, client_stream) = pane_io::in_process_attach_pair();
        let attach_pid = self.allocate_web_attach_pid().await?;
        let controls = access.controls();
        let mut flags = ClientFlags::default();
        let can_write = controls;
        if !controls {
            flags = flags.with_read_only();
        } else {
            flags.insert(ClientFlags::WEB_CONTROLS);
        }

        let terminal_context = OuterTerminalContext::default();
        let (control_tx, control_rx) = mpsc::unbounded_channel::<AttachControl>();
        let closing = Arc::new(AtomicBool::new(false));
        let persistent_overlay_epoch = Arc::new(AtomicU64::new(0));
        let attached_count = self
            .active_attach
            .lock()
            .await
            .attached_count(session_target.name());
        let (target, initial_size) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_target.name())
                .filter(|session| session.id() == session_target.id())
                .ok_or_else(|| session_not_found_web(session_target.name()))?;
            let size = session.window().size();
            (
                attach_target_for_session(
                    &state,
                    session_target.name(),
                    attached_count,
                    &terminal_context,
                )?,
                size,
            )
        };
        let attach_id = self
            .register_attach_with_access(
                attach_pid,
                session_target.name().clone(),
                AttachRegistration {
                    control_tx,
                    closing: closing.clone(),
                    persistent_overlay_epoch: persistent_overlay_epoch.clone(),
                    terminal_context,
                    flags,
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
                closing,
                persistent_overlay_epoch,
                LiveAttachInputContext {
                    handler: Arc::new(task_handler.clone()),
                    attach_pid,
                },
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
            revoke_rx,
            target: session_target,
            initial_size,
            writer,
            reader: Some(WebSessionAttachReader::new(reader)),
        })
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
            let transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            let screen = transcript.clone_screen();
            let size = screen.size();
            let (cursor_col, cursor_row) = screen.cursor_position();
            WebPaneSnapshot {
                cols: size.cols,
                rows: size.rows,
                output_sequence: 0,
                ansi_lines: snapshot_ansi_lines(&screen),
                cursor_row: cursor_row.min(u32::from(size.rows.saturating_sub(1))) as u16,
                cursor_col: cursor_col.min(u32::from(size.cols.saturating_sub(1))) as u16,
                cursor_visible: screen.mode() & mode::MODE_CURSOR != 0,
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

    pub(crate) async fn web_session_send_text(
        &self,
        session_target: &WebSessionTarget,
        text: String,
    ) -> Result<(), RmuxError> {
        let target = self.web_session_active_pane(session_target).await?;
        let response = self
            .handle_pane_input_ref(PaneInputRequest {
                target: PaneTargetRef::slot(target),
                keys: vec![text],
                literal: true,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_session_send_key(
        &self,
        session_target: &WebSessionTarget,
        key: String,
    ) -> Result<(), RmuxError> {
        let target = self.web_session_active_pane(session_target).await?;
        let response = self
            .handle_pane_input_ref(PaneInputRequest {
                target: PaneTargetRef::slot(target),
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
        self.require_web_session(session_target).await?;
        let response = self
            .handle_kill_session(KillSessionRequest {
                target: session_target.name().clone(),
                kill_all_except_target: false,
                clear_alerts: false,
            })
            .await;
        response_to_result(response)
    }

    pub(crate) async fn web_resize(
        &self,
        target: &PaneTargetRef,
        cols: u16,
        rows: u16,
    ) -> Result<(), RmuxError> {
        let response = self
            .handle_pane_resize_ref(PaneResizeRequest {
                target: target.clone(),
                adjustment: ResizePaneAdjustment::AbsoluteSize {
                    columns: cols,
                    rows,
                },
            })
            .await;
        response_to_result(response)
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
        self.require_web_session(session_target).await.is_ok()
    }

    async fn web_session_active_pane(
        &self,
        session_target: &WebSessionTarget,
    ) -> Result<PaneTarget, RmuxError> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session(session_target.name())
            .filter(|session| session.id() == session_target.id())
            .ok_or_else(|| session_not_found_web(session_target.name()))?;
        Ok(PaneTarget::with_window(
            session_target.name().clone(),
            session.active_window_index(),
            session.active_pane_index(),
        ))
    }

    async fn require_web_session(
        &self,
        session_target: &WebSessionTarget,
    ) -> Result<(), RmuxError> {
        let state = self.state.lock().await;
        state
            .sessions
            .session(session_target.name())
            .filter(|session| session.id() == session_target.id())
            .map(|_| ())
            .ok_or_else(|| session_not_found_web(session_target.name()))
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

fn session_not_found_web(session_name: &SessionName) -> RmuxError {
    RmuxError::Server(format!("can't find session: {session_name}"))
}

fn response_to_result(response: Response) -> Result<(), RmuxError> {
    match response {
        Response::Error(error) => Err(error.error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{
        CreateWebShareRequest, KillSessionRequest, NewSessionRequest, Request, Response,
        SessionName, TerminalSize, WebShareScope,
    };
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn web_share_create_resolves_slot_target_to_stable_pane_id() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("alpha").expect("valid session");
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));

        let response = handler
            .handle(Request::WebShare(WebShareRequest::Create(
                CreateWebShareRequest {
                    scope: WebShareScope::Pane(
                        rmux_proto::PaneTarget::new(session_name.clone(), 0).into(),
                    ),
                    public_base_url: Some("https://share.example".to_owned()),
                    frontend_url: None,
                    ttl_seconds: None,
                    max_readers: Some(1),
                    url_options: Default::default(),
                    require_pin: false,
                    terminal_palette: None,
                    writable: false,
                    controls: false,
                },
            )))
            .await;

        let Response::WebShare(rmux_proto::WebShareResponse::Created(created)) = response else {
            panic!("expected created web-share response");
        };
        assert!(matches!(
            created.scope,
            WebShareScope::Pane(PaneTargetRef::Id {
                session_name: ref actual,
                ..
            }) if actual == &session_name
        ));
        assert!(created
            .read_url
            .contains("#endpoint=wss://share.example/share&token="));
    }

    #[tokio::test]
    async fn web_session_share_opens_portable_attach_transport() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("websession").expect("valid session");
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));

        let response = handler
            .handle(Request::WebShare(WebShareRequest::Create(
                CreateWebShareRequest {
                    scope: WebShareScope::Session(session_name),
                    public_base_url: Some("https://share.example".to_owned()),
                    frontend_url: None,
                    ttl_seconds: None,
                    max_readers: Some(1),
                    url_options: Default::default(),
                    require_pin: false,
                    terminal_palette: None,
                    writable: true,
                    controls: true,
                },
            )))
            .await;

        let Response::WebShare(rmux_proto::WebShareResponse::Created(created)) = response else {
            panic!("expected created web-share response");
        };
        let operator_url = created.operator_url.as_deref().expect("operator URL");
        let operator_token = token_from_url(operator_url);
        let stream = handler
            .open_web_share(&operator_token, None)
            .await
            .expect("session web share opens");
        let WebShareStream::Session(mut session_stream) = stream else {
            panic!("expected session web share stream");
        };
        let mut reader = session_stream.take_attach_reader();
        let bytes = timeout(Duration::from_secs(2), reader.read_attach_bytes())
            .await
            .expect("attach stream should produce initial bytes")
            .expect("attach read succeeds")
            .expect("initial attach bytes are present");

        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn web_session_share_rejects_recreated_session_with_same_name() {
        let handler = RequestHandler::new();
        let session_name = SessionName::new("websession").expect("valid session");
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));

        let response = handler
            .handle(Request::WebShare(WebShareRequest::Create(
                CreateWebShareRequest {
                    scope: WebShareScope::Session(session_name.clone()),
                    public_base_url: Some("https://share.example".to_owned()),
                    frontend_url: None,
                    ttl_seconds: None,
                    max_readers: Some(1),
                    url_options: Default::default(),
                    require_pin: false,
                    terminal_palette: None,
                    writable: true,
                    controls: true,
                },
            )))
            .await;
        let Response::WebShare(rmux_proto::WebShareResponse::Created(created)) = response else {
            panic!("expected created web-share response");
        };
        let operator_token = token_from_url(created.operator_url.as_deref().expect("operator URL"));

        let killed = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: session_name.clone(),
                kill_all_except_target: false,
                clear_alerts: false,
            }))
            .await;
        assert!(matches!(killed, Response::KillSession(_)));

        let recreated = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(recreated, Response::NewSession(_)));

        let error = handler
            .open_web_share(&operator_token, None)
            .await
            .err()
            .expect("old share must not attach to a recreated session");
        assert!(error.to_string().contains("can't find session"));
    }

    fn token_from_url(url: &str) -> String {
        url.split_once("token=")
            .map(|(_, token)| token.split('&').next().unwrap_or(token).to_owned())
            .expect("URL contains access token")
    }
}
