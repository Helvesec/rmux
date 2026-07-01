use std::future::{Future, IntoFuture};
use std::pin::Pin;

use crate::handles::session::unexpected_response;
use crate::{Pane, Result};
use rmux_proto::{CapturePaneRequest, Request, Response};

/// Result returned by [`PaneCaptureBuilder`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PaneCapture {
    /// Captured stdout bytes from `capture-pane -p`.
    pub stdout: Vec<u8>,
    /// Buffer name created by a non-printing capture.
    pub buffer_name: Option<String>,
}

/// Awaitable builder for the daemon `capture-pane` command surface.
#[derive(Debug, Clone)]
#[must_use = "pane capture builders do nothing unless awaited"]
pub struct PaneCaptureBuilder<'a> {
    pane: &'a Pane,
    start: Option<i64>,
    end: Option<i64>,
    buffer_name: Option<String>,
    alternate: bool,
    escape_ansi: bool,
    escape_sequences: bool,
    join_wrapped: bool,
    use_mode_screen: bool,
    preserve_trailing_spaces: bool,
    do_not_trim_spaces: bool,
    pending_input: bool,
    quiet: bool,
    start_is_absolute: bool,
    end_is_absolute: bool,
}

impl<'a> PaneCaptureBuilder<'a> {
    pub(crate) const fn new(pane: &'a Pane) -> Self {
        Self {
            pane,
            start: None,
            end: None,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        }
    }

    /// Sets the inclusive start line (`capture-pane -S`).
    pub const fn start(mut self, line: i64) -> Self {
        self.start = Some(line);
        self.start_is_absolute = false;
        self
    }

    /// Sets the absolute inclusive start line (`capture-pane -S -` form).
    ///
    /// The daemon-side absolute form matches tmux's `-S -` sentinel; it does
    /// not carry a numeric line value, so `line` is intentionally ignored and
    /// kept only for builder symmetry/backwards compatibility. Use
    /// [`Self::start`] for numeric bounds.
    pub const fn start_absolute(mut self, _line: i64) -> Self {
        self.start = None;
        self.start_is_absolute = true;
        self
    }

    /// Sets the inclusive end line (`capture-pane -E`).
    pub const fn end(mut self, line: i64) -> Self {
        self.end = Some(line);
        self.end_is_absolute = false;
        self
    }

    /// Sets the absolute inclusive end line (`capture-pane -E -` form).
    ///
    /// The daemon-side absolute form matches tmux's `-E -` sentinel; it does
    /// not carry a numeric line value, so `line` is intentionally ignored and
    /// kept only for builder symmetry/backwards compatibility. Use
    /// [`Self::end`] for numeric bounds.
    pub const fn end_absolute(mut self, _line: i64) -> Self {
        self.end = None;
        self.end_is_absolute = true;
        self
    }

    /// Writes the capture into a daemon buffer instead of stdout.
    pub fn buffer(mut self, name: impl Into<String>) -> Self {
        self.buffer_name = Some(name.into());
        self
    }

    /// Captures the alternate-screen copy (`-a`).
    pub const fn alternate(mut self, enabled: bool) -> Self {
        self.alternate = enabled;
        self
    }

    /// Preserves ANSI SGR and hyperlink sequences (`-e`).
    pub const fn escape_ansi(mut self, enabled: bool) -> Self {
        self.escape_ansi = enabled;
        self
    }

    /// Octal-escapes control sequences (`-C`).
    pub const fn escape_sequences(mut self, enabled: bool) -> Self {
        self.escape_sequences = enabled;
        self
    }

    /// Joins wrapped rows (`-J`).
    pub const fn join_wrapped(mut self, enabled: bool) -> Self {
        self.join_wrapped = enabled;
        self
    }

    /// Captures the copy-mode screen when present (`-M`).
    pub const fn use_mode_screen(mut self, enabled: bool) -> Self {
        self.use_mode_screen = enabled;
        self
    }

    /// Preserves trailing spaces (`-N`).
    pub const fn preserve_trailing_spaces(mut self, enabled: bool) -> Self {
        self.preserve_trailing_spaces = enabled;
        self
    }

    /// Disables trimming of spaces (`-T`).
    pub const fn do_not_trim_spaces(mut self, enabled: bool) -> Self {
        self.do_not_trim_spaces = enabled;
        self
    }

    /// Captures pending parser input bytes (`-P`).
    pub const fn pending_input(mut self, enabled: bool) -> Self {
        self.pending_input = enabled;
        self
    }

    /// Silences missing alternate-screen content (`-q`).
    pub const fn quiet(mut self, enabled: bool) -> Self {
        self.quiet = enabled;
        self
    }

    async fn run(self) -> Result<PaneCapture> {
        let target = self.pane.current_target().await?.to_proto();
        let print = self.buffer_name.is_none();
        match self
            .pane
            .transport()
            .request(Request::CapturePane(Box::new(CapturePaneRequest {
                target,
                start: self.start,
                end: self.end,
                print,
                buffer_name: self.buffer_name,
                alternate: self.alternate,
                escape_ansi: self.escape_ansi,
                escape_sequences: self.escape_sequences,
                join_wrapped: self.join_wrapped,
                use_mode_screen: self.use_mode_screen,
                preserve_trailing_spaces: self.preserve_trailing_spaces,
                do_not_trim_spaces: self.do_not_trim_spaces,
                pending_input: self.pending_input,
                quiet: self.quiet,
                start_is_absolute: self.start_is_absolute,
                end_is_absolute: self.end_is_absolute,
            })))
            .await?
        {
            Response::CapturePane(response) => Ok(PaneCapture {
                stdout: response
                    .output
                    .map(|output| output.stdout)
                    .unwrap_or_default(),
                buffer_name: response.buffer_name,
            }),
            response => Err(unexpected_response("capture-pane", response)),
        }
    }
}

impl<'a> IntoFuture for PaneCaptureBuilder<'a> {
    type Output = Result<PaneCapture>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PaneRef, RmuxEndpoint};
    use rmux_proto::{encode_frame, CapturePaneResponse, CommandOutput, FrameDecoder, PaneTarget};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn alpha() -> rmux_proto::SessionName {
        rmux_proto::SessionName::new("alpha").expect("valid session")
    }

    fn pane(client: crate::transport::TransportClient) -> Pane {
        Pane::new(
            PaneRef::new(alpha(), 1, 3),
            RmuxEndpoint::Default,
            None,
            client,
        )
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

    #[tokio::test]
    async fn pane_capture_builder_sends_capture_pane_options() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let pane = pane(crate::transport::TransportClient::spawn(client_stream));

        let capture = tokio::spawn({
            let pane = pane.clone();
            async move {
                pane.capture_pane()
                    .start(-20)
                    .end(0)
                    .alternate(true)
                    .escape_ansi(true)
                    .join_wrapped(true)
                    .await
            }
        });

        match read_request(&mut server_stream).await {
            Request::CapturePane(request) => {
                assert_eq!(request.target, PaneTarget::with_window(alpha(), 1, 3));
                assert_eq!(request.start, Some(-20));
                assert_eq!(request.end, Some(0));
                assert!(request.print);
                assert!(request.alternate);
                assert!(request.escape_ansi);
                assert!(request.join_wrapped);
                assert!(!request.escape_sequences);
            }
            request => panic!("expected capture-pane, got {request:?}"),
        }
        write_response(
            &mut server_stream,
            Response::CapturePane(CapturePaneResponse::from_output(
                CommandOutput::from_stdout(b"hello\n".to_vec()),
            )),
        )
        .await;

        let capture = capture
            .await
            .expect("capture task")
            .expect("capture succeeds");
        assert_eq!(capture.stdout, b"hello\n");
        assert_eq!(capture.buffer_name, None);
    }

    #[tokio::test]
    async fn pane_capture_builder_sends_absolute_bounds_as_sentinels() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let pane = pane(crate::transport::TransportClient::spawn(client_stream));

        let capture = tokio::spawn({
            let pane = pane.clone();
            async move {
                pane.capture_pane()
                    .start_absolute(20)
                    .end_absolute(30)
                    .await
            }
        });

        match read_request(&mut server_stream).await {
            Request::CapturePane(request) => {
                assert_eq!(request.start, None);
                assert_eq!(request.end, None);
                assert!(request.start_is_absolute);
                assert!(request.end_is_absolute);
            }
            request => panic!("expected capture-pane, got {request:?}"),
        }
        write_response(
            &mut server_stream,
            Response::CapturePane(CapturePaneResponse::from_output(
                CommandOutput::from_stdout(b"hello\n".to_vec()),
            )),
        )
        .await;

        let capture = capture
            .await
            .expect("capture task")
            .expect("capture succeeds");
        assert_eq!(capture.stdout, b"hello\n");
    }

    #[tokio::test]
    async fn pane_capture_builder_can_target_a_buffer() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let pane = pane(crate::transport::TransportClient::spawn(client_stream));

        let capture = tokio::spawn({
            let pane = pane.clone();
            async move { pane.capture_pane().buffer("clip").await }
        });

        match read_request(&mut server_stream).await {
            Request::CapturePane(request) => {
                assert!(!request.print);
                assert_eq!(request.buffer_name.as_deref(), Some("clip"));
            }
            request => panic!("expected capture-pane, got {request:?}"),
        }
        write_response(
            &mut server_stream,
            Response::CapturePane(CapturePaneResponse::from_buffer("clip".to_owned())),
        )
        .await;

        let capture = capture
            .await
            .expect("capture task")
            .expect("capture succeeds");
        assert!(capture.stdout.is_empty());
        assert_eq!(capture.buffer_name.as_deref(), Some("clip"));
    }
}
