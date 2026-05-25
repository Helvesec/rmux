use rmux_proto::{
    CreateWebShareRequest, ErrorResponse, PaneInputRequest, PaneResizeRequest, PaneTargetRef,
    ResizePaneAdjustment, Response, RmuxError, WebShareRequest,
};

use super::pane_support::resolve_pane_target_ref;
use super::RequestHandler;
use crate::pane_io::PaneOutputReceiver;
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::web::{WebShareAccess, WebShareConnectRole, WebShareRevokeReason};
use rmux_core::{input::mode, GridRenderOptions, ScreenCaptureRange};

pub(crate) struct WebPaneStream {
    _access: WebShareAccess,
    pub(crate) output: PaneOutputReceiver,
    pub(crate) snapshot: WebPaneSnapshot,
    pub(crate) revoke_rx: tokio::sync::watch::Receiver<Option<WebShareRevokeReason>>,
    target: PaneTargetRef,
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
        role: WebShareConnectRole,
    ) -> Result<WebPaneStream, RmuxError> {
        let access = self.web_shares.connect(share_id, key, role)?;
        let target = self.stable_web_target(access.target()).await?;
        let (snapshot, output) = self.web_resnapshot(&target).await?;
        let revoke_rx = access.revoke_receiver();
        Ok(WebPaneStream {
            _access: access,
            output,
            revoke_rx,
            snapshot,
            target,
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
        let target = resolve_pane_target_ref(&state, &request.target)?;
        let pane_id = pane_id_for_target(
            &state.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        request.target = PaneTargetRef::by_id(target.session_name().clone(), pane_id);
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
                    target: rmux_proto::PaneTarget::new(session_name.clone(), 0).into(),
                    public_base_url: Some("https://share.example".to_owned()),
                    frontend_url: None,
                    ttl_seconds: None,
                    max_viewers: Some(1),
                    writable: false,
                },
            )))
            .await;

        let Response::WebShare(rmux_proto::WebShareResponse::Created(created)) = response else {
            panic!("expected created web-share response");
        };
        assert!(matches!(
            created.target,
            PaneTargetRef::Id {
                session_name: ref actual,
                ..
            } if actual == &session_name
        ));
        assert!(created.viewer_url.contains("&key="));
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
