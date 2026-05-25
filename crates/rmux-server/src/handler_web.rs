use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;

use rmux_ipc::LocalStream;
use rmux_os::identity::UserIdentity;
use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke,
    CreateWebShareRequest, ErrorResponse, KillSessionRequest, PaneInputRequest, PaneResizeRequest,
    PaneTarget, PaneTargetRef, ResizePaneAdjustment, Response, RmuxError, SessionName,
    TerminalSize, WebShareRequest, WebShareScope, WebTerminalPalette,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, watch};

use super::attach_support::{attach_target_for_session, AttachRegistration, ClientFlags};
use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{self, AttachControl, LiveAttachInputContext, PaneOutputReceiver};
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::server_access::current_owner_uid;
use crate::web::{WebShareAccess, WebShareConnectRole, WebShareRevokeReason};
use rmux_core::{input::mode, GridRenderOptions, ScreenCaptureRange};

const WEB_ATTACH_PID_BASE: u32 = 0x8000_0000;
const ATTACH_READ_BUFFER_SIZE: usize = 8192;

pub(crate) struct WebPaneStream {
    _access: WebShareAccess,
    pub(crate) output: PaneOutputReceiver,
    pub(crate) snapshot: WebPaneSnapshot,
    pub(crate) revoke_rx: tokio::sync::watch::Receiver<Option<WebShareRevokeReason>>,
    target: PaneTargetRef,
}

pub(crate) enum WebShareStream {
    Pane(WebPaneStream),
    Session(WebSessionStream),
}

pub(crate) struct WebSessionStream {
    _access: WebShareAccess,
    pub(crate) revoke_rx: tokio::sync::watch::Receiver<Option<WebShareRevokeReason>>,
    session_name: SessionName,
    initial_size: TerminalSize,
    writer: WriteHalf<LocalStream>,
    reader: Option<Box<WebSessionAttachReader>>,
}

pub(crate) struct WebSessionAttachReader {
    reader: ReadHalf<LocalStream>,
    decoder: AttachFrameDecoder,
    read_buffer: [u8; ATTACH_READ_BUFFER_SIZE],
}

impl WebPaneStream {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        self._access.origin_allowed(received)
    }

    pub(crate) fn is_operator(&self) -> bool {
        self._access.is_operator()
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        self._access.expires_at()
    }

    pub(crate) fn release_operator(&mut self) -> bool {
        self._access.release_operator()
    }

    pub(crate) fn target(&self) -> &PaneTargetRef {
        &self.target
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self._access.terminal_palette()
    }
}

impl WebShareStream {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        match self {
            Self::Pane(stream) => stream.origin_allowed(received),
            Self::Session(stream) => stream.origin_allowed(received),
        }
    }

    pub(crate) fn is_operator(&self) -> bool {
        match self {
            Self::Pane(stream) => stream.is_operator(),
            Self::Session(stream) => stream.is_operator(),
        }
    }

    pub(crate) fn controls(&self) -> bool {
        match self {
            Self::Pane(_) => false,
            Self::Session(stream) => stream.controls(),
        }
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        match self {
            Self::Pane(stream) => stream.terminal_palette(),
            Self::Session(stream) => stream.terminal_palette(),
        }
    }
}

impl WebSessionStream {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        self._access.origin_allowed(received)
    }

    pub(crate) fn is_operator(&self) -> bool {
        self._access.is_operator()
    }

    pub(crate) fn controls(&self) -> bool {
        self._access.controls()
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        self._access.expires_at()
    }

    pub(crate) fn release_operator(&mut self) -> bool {
        self._access.release_operator()
    }

    pub(crate) fn session_name(&self) -> &SessionName {
        &self.session_name
    }

    pub(crate) const fn initial_size(&self) -> TerminalSize {
        self.initial_size
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self._access.terminal_palette()
    }

    pub(crate) fn take_attach_reader(&mut self) -> WebSessionAttachReader {
        *self
            .reader
            .take()
            .expect("web session attach reader is taken exactly once")
    }

    pub(crate) async fn send_attach_keystroke(&mut self, bytes: Vec<u8>) -> io::Result<()> {
        self.write_attach_message(AttachMessage::Keystroke(AttachedKeystroke::new(bytes)))
            .await
    }

    pub(crate) async fn send_resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.write_attach_message(AttachMessage::Resize(TerminalSize { cols, rows }))
            .await
    }

    async fn write_attach_message(&mut self, message: AttachMessage) -> io::Result<()> {
        let frame =
            encode_attach_message(&message).map_err(|error| io::Error::other(error.to_string()))?;
        self.writer.write_all(&frame).await
    }
}

impl WebSessionAttachReader {
    pub(crate) async fn read_attach_bytes(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(message) = self
                .decoder
                .next_message()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
            {
                match message {
                    AttachMessage::Data(bytes) => return Ok(Some(bytes)),
                    AttachMessage::KeyDispatched(_) => continue,
                    AttachMessage::Lock(_)
                    | AttachMessage::LockShellCommand(_)
                    | AttachMessage::Unlock
                    | AttachMessage::Suspend
                    | AttachMessage::DetachKill
                    | AttachMessage::DetachExec(_)
                    | AttachMessage::DetachExecShellCommand(_)
                    | AttachMessage::Resize(_)
                    | AttachMessage::ResizeGeometry(_)
                    | AttachMessage::Keystroke(_) => continue,
                }
            }

            let read = self.reader.read(&mut self.read_buffer).await?;
            if read == 0 {
                return Ok(None);
            }
            self.decoder.push_bytes(&self.read_buffer[..read]);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WebPaneSnapshot {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) output_sequence: u64,
    pub(crate) ansi_lines: Vec<Vec<u8>>,
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) cursor_visible: bool,
}

impl RequestHandler {
    pub(crate) fn web_listener(&self) -> rmux_proto::WebShareListener {
        self.web_shares.listener()
    }

    pub(in crate::handler) async fn handle_web_share(&self, request: WebShareRequest) -> Response {
        let request = match self.resolve_web_share_targets(request).await {
            Ok(request) => request,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        match self.web_shares.handle(request) {
            Ok(response) => Response::WebShare(response),
            Err(error) => Response::Error(ErrorResponse { error }),
        }
    }

    pub(crate) async fn open_web_share(
        &self,
        share_id: &str,
        key: &str,
        pin: Option<&str>,
        role: WebShareConnectRole,
    ) -> Result<WebShareStream, RmuxError> {
        let access = self.web_shares.connect(share_id, key, pin, role)?;
        match access.scope().clone() {
            WebShareScope::Pane(target) => {
                let target = self.stable_web_target(&target).await?;
                let (snapshot, output) = self.web_resnapshot(&target).await?;
                let revoke_rx = access.revoke_receiver();
                Ok(WebShareStream::Pane(WebPaneStream {
                    _access: access,
                    output,
                    revoke_rx,
                    snapshot,
                    target,
                }))
            }
            WebShareScope::Session(session_name) => {
                let stream = self.open_web_session_share(access, session_name).await?;
                Ok(WebShareStream::Session(stream))
            }
        }
    }

    async fn open_web_session_share(
        &self,
        access: WebShareAccess,
        session_name: SessionName,
    ) -> Result<WebSessionStream, RmuxError> {
        let (client_stream, server_stream) =
            LocalStream::pair().map_err(|error| RmuxError::Server(error.to_string()))?;
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
            .attached_count(&session_name);
        let (target, initial_size) = {
            let state = self.state.lock().await;
            let size = state
                .sessions
                .session(&session_name)
                .map(|session| session.window().size())
                .ok_or_else(|| session_not_found_web(&session_name))?;
            (
                attach_target_for_session(
                    &state,
                    &session_name,
                    attached_count,
                    &terminal_context,
                )?,
                size,
            )
        };
        let attach_id = self
            .register_attach_with_access(
                attach_pid,
                session_name.clone(),
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
                server_stream,
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
            _access: access,
            revoke_rx,
            session_name,
            initial_size,
            writer,
            reader: Some(Box::new(WebSessionAttachReader {
                reader,
                decoder: AttachFrameDecoder::new(),
                read_buffer: [0; ATTACH_READ_BUFFER_SIZE],
            })),
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
        session_name: &SessionName,
        text: String,
    ) -> Result<(), RmuxError> {
        let target = self.web_session_active_pane(session_name).await?;
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
        session_name: &SessionName,
        key: String,
    ) -> Result<(), RmuxError> {
        let target = self.web_session_active_pane(session_name).await?;
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
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let response = self
            .handle_kill_session(KillSessionRequest {
                target: session_name.clone(),
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

    async fn resolve_web_share_targets(
        &self,
        request: WebShareRequest,
    ) -> Result<WebShareRequest, rmux_proto::RmuxError> {
        match request {
            WebShareRequest::Create(request) => self.resolve_create_web_share(request).await,
            other => Ok(other),
        }
    }

    async fn resolve_create_web_share(
        &self,
        mut request: CreateWebShareRequest,
    ) -> Result<WebShareRequest, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        match &request.scope {
            WebShareScope::Pane(raw_target) => {
                let target = resolve_pane_target_ref(&state, raw_target)?;
                let pane_id = pane_id_for_target(
                    &state.sessions,
                    target.session_name(),
                    target.window_index(),
                    target.pane_index(),
                )?;
                request.scope = WebShareScope::Pane(PaneTargetRef::by_id(
                    target.session_name().clone(),
                    pane_id,
                ));
            }
            WebShareScope::Session(session_name) => {
                if state.sessions.session(session_name).is_none() {
                    return Err(session_not_found_web(session_name));
                }
            }
        }
        Ok(WebShareRequest::Create(request))
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

    pub(crate) async fn web_session_alive(&self, session_name: &SessionName) -> bool {
        let state = self.state.lock().await;
        state.sessions.session(session_name).is_some()
    }

    async fn web_session_active_pane(
        &self,
        session_name: &SessionName,
    ) -> Result<PaneTarget, RmuxError> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found_web(session_name))?;
        Ok(PaneTarget::with_window(
            session_name.clone(),
            session.active_window_index(),
            session.active_pane_index(),
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

fn session_not_found_web(session_name: &SessionName) -> RmuxError {
    RmuxError::Server(format!("can't find session: {session_name}"))
}

impl WebPaneSnapshot {
    pub(crate) fn ansi_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"\x1bc\x1b[?25l\x1b[H");
        for (index, line) in self.ansi_lines.iter().enumerate() {
            if index > 0 {
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(b"\x1b[0m");
            out.extend_from_slice(line);
        }
        let cursor_row = self.cursor_row.min(self.rows.saturating_sub(1)) + 1;
        let cursor_col = self.cursor_col.min(self.cols.saturating_sub(1)) + 1;
        out.extend_from_slice(format!("\x1b[0m\x1b[{cursor_row};{cursor_col}H").as_bytes());
        out.extend_from_slice(if self.cursor_visible {
            b"\x1b[?25h"
        } else {
            b"\x1b[?25l"
        });
        out
    }
}

fn snapshot_ansi_lines(screen: &rmux_core::Screen) -> Vec<Vec<u8>> {
    screen.capture_transcript_lines_independent(
        ScreenCaptureRange::default(),
        GridRenderOptions {
            with_sequences: true,
            trim_spaces: false,
            ..GridRenderOptions::default()
        },
    )
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
    use rmux_core::{input::InputParser, Screen};
    use rmux_proto::{
        CreateWebShareRequest, NewSessionRequest, Request, Response, SessionName, TerminalSize,
        WebShareScope,
    };

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
        assert!(created.read_url.contains("&key="));
    }

    #[test]
    fn web_snapshot_bytes_preserve_ansi_style_and_cursor() {
        let snapshot = WebPaneSnapshot {
            cols: 80,
            rows: 24,
            output_sequence: 7,
            ansi_lines: vec![b"\x1b[32mpingu@host\x1b[0m".to_vec()],
            cursor_row: 3,
            cursor_col: 7,
            cursor_visible: true,
        };

        let bytes = snapshot.ansi_bytes();
        let rendered = String::from_utf8(bytes).expect("snapshot bytes are utf8");

        assert!(rendered.contains("\x1b[32mpingu@host"));
        assert!(rendered.contains("\x1b[4;8H\x1b[?25h"));
    }

    #[test]
    fn web_snapshot_capture_preserves_screen_sequences() {
        let mut screen = Screen::new(TerminalSize { cols: 12, rows: 3 }, 100);
        let mut parser = InputParser::new();
        parser.parse(b"\x1b[32mpingu\x1b[0m@host", &mut screen);

        let lines = snapshot_ansi_lines(&screen);
        let joined = String::from_utf8(lines.concat()).expect("snapshot lines are utf8");

        assert!(joined.contains("\x1b[32m"));
        assert!(joined.contains("pingu"));
    }
}
