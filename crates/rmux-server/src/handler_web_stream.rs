use std::io;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, PaneTargetRef,
    SessionName, TerminalSize, WebTerminalPalette,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

use crate::pane_io::PaneOutputReceiver;
use crate::web::{WebShareAccess, WebShareRevokeReason};

use super::WebPaneSnapshot;

const ATTACH_READ_BUFFER_SIZE: usize = 8192;

pub(crate) struct WebPaneStream {
    pub(crate) access: WebShareAccess,
    pub(crate) output: PaneOutputReceiver,
    pub(crate) snapshot: WebPaneSnapshot,
    pub(crate) revoke_rx: tokio::sync::watch::Receiver<Option<WebShareRevokeReason>>,
    pub(crate) target: PaneTargetRef,
}

pub(crate) enum WebShareStream {
    Pane(Box<WebPaneStream>),
    Session(Box<WebSessionStream>),
}

pub(crate) struct WebSessionStream {
    pub(crate) access: WebShareAccess,
    pub(crate) revoke_rx: tokio::sync::watch::Receiver<Option<WebShareRevokeReason>>,
    pub(crate) session_name: SessionName,
    pub(crate) initial_size: TerminalSize,
    pub(crate) writer: WriteHalf<DuplexStream>,
    pub(crate) reader: Option<WebSessionAttachReader>,
}

pub(crate) struct WebSessionAttachReader {
    reader: ReadHalf<DuplexStream>,
    decoder: AttachFrameDecoder,
    read_buffer: [u8; ATTACH_READ_BUFFER_SIZE],
}

impl WebPaneStream {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        self.access.origin_allowed(received)
    }

    pub(crate) fn is_operator(&self) -> bool {
        self.access.is_operator()
    }

    pub(crate) fn share_id(&self) -> &str {
        self.access.share_id()
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        self.access.expires_at()
    }

    pub(crate) fn release_operator(&mut self) -> bool {
        self.access.release_operator()
    }

    pub(crate) fn target(&self) -> &PaneTargetRef {
        &self.target
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self.access.terminal_palette()
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

    pub(crate) fn share_id(&self) -> &str {
        match self {
            Self::Pane(stream) => stream.share_id(),
            Self::Session(stream) => stream.share_id(),
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

    pub(crate) fn role(&self) -> &'static str {
        if self.is_operator() {
            "operator"
        } else {
            "read"
        }
    }
}

impl WebSessionStream {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        self.access.origin_allowed(received)
    }

    pub(crate) fn is_operator(&self) -> bool {
        self.access.is_operator()
    }

    pub(crate) fn share_id(&self) -> &str {
        self.access.share_id()
    }

    pub(crate) fn controls(&self) -> bool {
        self.access.controls()
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        self.access.expires_at()
    }

    pub(crate) fn release_operator(&mut self) -> bool {
        self.access.release_operator()
    }

    pub(crate) fn session_name(&self) -> &SessionName {
        &self.session_name
    }

    pub(crate) const fn initial_size(&self) -> TerminalSize {
        self.initial_size
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self.access.terminal_palette()
    }

    pub(crate) fn take_attach_reader(&mut self) -> WebSessionAttachReader {
        self.reader
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
    pub(crate) fn new(reader: ReadHalf<DuplexStream>) -> Self {
        Self {
            reader,
            decoder: AttachFrameDecoder::new(),
            read_buffer: [0; ATTACH_READ_BUFFER_SIZE],
        }
    }

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
