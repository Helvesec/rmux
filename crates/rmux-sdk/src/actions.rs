//! High-level terminal actions built on existing pane input primitives.

use crate::{Input, Locator, Pane, PaneSet, Result};

/// Keyboard actions for one pane.
#[derive(Debug, Clone)]
pub struct PaneKeyboard {
    pane: Pane,
}

impl PaneKeyboard {
    pub(crate) const fn new(pane: Pane) -> Self {
        Self { pane }
    }

    /// Sends literal text to the pane. No newline is appended.
    pub async fn type_text(&self, text: impl AsRef<str>) -> Result<()> {
        self.pane.send_text(text.as_ref()).await
    }

    /// Sends one tmux-compatible key token to the pane.
    ///
    /// Common Playwright-style spellings such as `Control+C` and `Ctrl+C`
    /// are normalized to tmux-style `C-c` key tokens.
    pub async fn press(&self, key: impl AsRef<str>) -> Result<()> {
        self.pane.send_key(normalize_key_token(key.as_ref())).await
    }
}

impl Pane {
    /// Returns high-level keyboard actions for this pane.
    #[must_use]
    pub fn keyboard(&self) -> PaneKeyboard {
        PaneKeyboard::new(self.clone())
    }

    /// Returns high-level mouse actions for this pane.
    #[must_use]
    pub fn mouse(&self) -> PaneMouse {
        PaneMouse::new(self.clone())
    }
}

/// Keyboard actions broadcast to a pane set.
#[derive(Debug, Clone)]
pub struct PaneSetKeyboard {
    panes: PaneSet,
}

impl PaneSetKeyboard {
    pub(crate) fn new(panes: PaneSet) -> Self {
        Self { panes }
    }

    /// Sends literal text to every pane in this set.
    pub async fn type_text(&self, text: impl AsRef<str>) -> Result<()> {
        self.panes.broadcast(Input::text(text.as_ref())).await?;
        Ok(())
    }

    /// Sends one key token to every pane in this set.
    pub async fn press(&self, key: impl AsRef<str>) -> Result<()> {
        let key = normalize_key_token(key.as_ref());
        self.panes.broadcast(Input::key(&key)).await?;
        Ok(())
    }
}

impl PaneSet {
    /// Returns high-level keyboard actions broadcast to this pane set.
    #[must_use]
    pub fn keyboard(&self) -> PaneSetKeyboard {
        PaneSetKeyboard::new(self.clone())
    }
}

/// Mouse actions for one pane.
///
/// These helpers inject terminal mouse-report escape sequences into the pane's
/// foreground process. They do not operate below the PTY like a real terminal
/// emulator would. Applications that understand SGR mouse reports can react to
/// them; shells and programs that do not may treat the bytes as literal input.
#[derive(Debug, Clone)]
pub struct PaneMouse {
    pane: Pane,
}

impl PaneMouse {
    pub(crate) const fn new(pane: Pane) -> Self {
        Self { pane }
    }

    /// Moves the terminal mouse cursor to a zero-based row and column.
    ///
    /// This sends an SGR mouse-mode motion sequence as input bytes. Callers
    /// should only use it with applications that are prepared to parse SGR
    /// mouse reports; other applications may render or buffer the bytes.
    pub async fn move_to(&self, row: u16, col: u16) -> Result<()> {
        self.pane
            .send_text(sgr_mouse_sequence(35, row, col, true))
            .await
    }

    /// Sends a primary-button click at a zero-based row and column.
    ///
    /// This sends SGR mouse press and release sequences as input bytes. It is
    /// intentionally minimal: terminals have no DOM hit target, so higher-level
    /// semantics belong to the application under test.
    pub async fn click(&self, row: u16, col: u16) -> Result<()> {
        let pane = self.pane.begin_operation_handle();
        pane.send_text(sgr_mouse_sequence(0, row, col, true))
            .await?;
        pane.send_text(sgr_mouse_sequence(0, row, col, false)).await
    }
}

/// Clearing strategy used by [`Locator::fill_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FillStrategy {
    /// Send `C-u` before typing. This matches common readline-like prompts.
    ControlU,
    /// Send `Backspace` `n` times before typing.
    Backspace(usize),
    /// Do not attempt to clear existing terminal input before typing.
    None,
}

impl Locator {
    /// Clicks the strict visible text match for this locator.
    pub async fn click(self) -> Result<()> {
        let locator = self.begin_operation_handle();
        let (_snapshot, item) = locator.resolve_strict_with_wait().await?;
        locator
            .pane()
            .mouse()
            .click(item.text_match.start_row, item.text_match.start_col)
            .await
    }

    /// Moves the mouse to the strict visible text match for this locator.
    pub async fn hover(self) -> Result<()> {
        let locator = self.begin_operation_handle();
        let (_snapshot, item) = locator.resolve_strict_with_wait().await?;
        locator
            .pane()
            .mouse()
            .move_to(item.text_match.start_row, item.text_match.start_col)
            .await
    }

    /// Best-effort terminal fill for the current prompt line.
    ///
    /// A terminal has no input element to focus. The locator is used for
    /// strictness and synchronization only; text is still typed at the current
    /// terminal cursor. The default clearing strategy sends `C-u`, which is
    /// intended for common readline-like shells and REPLs.
    pub async fn fill(self, text: impl AsRef<str>) -> Result<()> {
        self.fill_with(text, FillStrategy::ControlU).await
    }

    /// Best-effort terminal fill with an explicit clearing strategy.
    ///
    /// This method still types at the terminal cursor. It does not move focus
    /// to the matched text because terminals do not expose DOM-like controls.
    pub async fn fill_with(self, text: impl AsRef<str>, strategy: FillStrategy) -> Result<()> {
        let locator = self.begin_operation_handle();
        let pane = locator.pane().clone();
        let (_snapshot, _item) = locator.resolve_strict_with_wait().await?;
        let keyboard = pane.keyboard();
        match strategy {
            FillStrategy::ControlU => keyboard.press("C-u").await?,
            FillStrategy::Backspace(count) => {
                for _ in 0..count {
                    keyboard.press("Backspace").await?;
                }
            }
            FillStrategy::None => {}
        }
        keyboard.type_text(text.as_ref()).await
    }
}

fn normalize_key_token(key: &str) -> String {
    let Some((modifiers, key_name)) = key.rsplit_once('+') else {
        return key.to_owned();
    };
    if key_name.is_empty() {
        return key.to_owned();
    }

    let mut normalized = Vec::new();
    for modifier in modifiers.split('+') {
        match modifier.to_ascii_lowercase().as_str() {
            "control" | "ctrl" => normalized.push("C"),
            "alt" | "meta" | "option" => normalized.push("M"),
            "shift" => normalized.push("S"),
            _ => return key.to_owned(),
        }
    }
    if normalized.is_empty() {
        return key.to_owned();
    }

    let has_shift = normalized.contains(&"S");
    let control_only = normalized.len() == 1 && normalized[0] == "C";
    let key_name = if control_only || (normalized.contains(&"C") && !has_shift) {
        key_name.to_ascii_lowercase()
    } else {
        key_name.to_owned()
    };
    format!("{}-{key_name}", normalized.join("-"))
}

#[cfg(test)]
fn control_key(rest: &str) -> String {
    let lowered = rest.to_ascii_lowercase();
    format!("C-{lowered}")
}

fn sgr_mouse_sequence(button: u16, row: u16, col: u16, press: bool) -> String {
    let suffix = if press { 'M' } else { 'm' };
    let row = row.saturating_add(1);
    let col = col.saturating_add(1);
    format!("\x1b[<{button};{col};{row}{suffix}")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::Instant;

    use super::{control_key, normalize_key_token, sgr_mouse_sequence};
    use crate::transport::TransportClient;
    use crate::{Pane, PaneRef, RmuxEndpoint, RmuxError, SessionName};
    use rmux_proto::{
        encode_frame, CommandOutput, FrameDecoder, ListPanesResponse, PaneInputRequest, Request,
        Response, SendKeysResponse, CAPABILITY_HANDSHAKE, CAPABILITY_SDK_PANE_BY_ID,
    };

    #[test]
    fn keyboard_tokens_preserve_plain_keys_and_normalize_control_spellings() {
        assert_eq!(normalize_key_token("Enter"), "Enter");
        assert_eq!(normalize_key_token("Backspace"), "Backspace");
        assert_eq!(normalize_key_token("Control+C"), "C-c");
        assert_eq!(normalize_key_token("Control+["), "C-[");
        assert_eq!(normalize_key_token("Ctrl+Z"), "C-z");
        assert_eq!(normalize_key_token("ctrl+c"), "C-c");
        assert_eq!(normalize_key_token("Alt+x"), "M-x");
        assert_eq!(normalize_key_token("Meta+x"), "M-x");
        assert_eq!(normalize_key_token("Option+x"), "M-x");
        assert_eq!(normalize_key_token("Shift+Tab"), "S-Tab");
        assert_eq!(normalize_key_token("Control+Shift+T"), "C-S-T");
        assert_eq!(normalize_key_token("Hyper+X"), "Hyper+X");
    }

    #[test]
    fn control_key_lowercases_only_the_control_suffix() {
        assert_eq!(control_key("C"), "C-c");
        assert_eq!(control_key("Break"), "C-break");
    }

    #[test]
    fn mouse_sequences_use_zero_based_input_and_sgr_coordinates() {
        assert_eq!(sgr_mouse_sequence(35, 0, 0, true), "\x1b[<35;1;1M");
        assert_eq!(sgr_mouse_sequence(0, 2, 4, true), "\x1b[<0;5;3M");
        assert_eq!(sgr_mouse_sequence(0, 2, 4, false), "\x1b[<0;5;3m");
    }

    #[test]
    fn mouse_sequences_saturate_at_terminal_protocol_bounds() {
        assert_eq!(
            sgr_mouse_sequence(0, u16::MAX, u16::MAX, true),
            "\x1b[<0;65535;65535M"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn mouse_click_shares_one_fifty_millisecond_press_release_deadline() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client_stream);
        transport
            .cache_capabilities(vec![
                CAPABILITY_HANDSHAKE.to_owned(),
                CAPABILITY_SDK_PANE_BY_ID.to_owned(),
            ])
            .await;
        let pane = Pane::new(
            PaneRef::new(session_name(), 0, 0),
            RmuxEndpoint::Default,
            Some(Duration::from_millis(50)),
            transport,
        );
        let server = tokio::spawn(async move {
            serve_mouse_half(&mut server_stream, true).await;
            serve_mouse_half(&mut server_stream, false).await;
        });

        let started = Instant::now();
        let error = pane
            .mouse()
            .click(2, 4)
            .await
            .expect_err("release must not receive a fresh timeout budget");

        assert_eq!(Instant::now() - started, Duration::from_millis(50));
        assert_timed_out(error);
        server.abort();
    }

    async fn serve_mouse_half(stream: &mut tokio::io::DuplexStream, press: bool) {
        assert!(matches!(read_request(stream).await, Request::ListPanes(_)));
        write_response(
            stream,
            Response::ListPanes(ListPanesResponse {
                output: CommandOutput::from_stdout("0:0:%1\n"),
            }),
        )
        .await;

        match read_request(stream).await {
            Request::PaneInput(PaneInputRequest { keys, literal, .. }) => {
                assert!(literal);
                assert_eq!(keys, [sgr_mouse_sequence(0, 2, 4, press)]);
            }
            request => panic!("expected pane input, got {request:?}"),
        }
        tokio::time::sleep(Duration::from_millis(35)).await;
        write_response(
            stream,
            Response::SendKeys(SendKeysResponse { key_count: 1 }),
        )
        .await;
    }

    async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 256];
        loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return request;
            }
            let read = stream.read(&mut buffer).await.expect("read request");
            assert_ne!(read, 0, "client closed before request");
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        stream.write_all(&frame).await.expect("write response");
        stream.flush().await.expect("flush response");
    }

    fn session_name() -> SessionName {
        SessionName::new("mouse-deadline").expect("valid session name")
    }

    fn assert_timed_out(error: RmuxError) {
        match error {
            RmuxError::Transport { source, .. } => {
                assert_eq!(source.kind(), io::ErrorKind::TimedOut);
            }
            error => panic!("expected transport timeout, got {error:?}"),
        }
    }
}
