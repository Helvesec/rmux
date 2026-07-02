use std::io;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke,
    AttachedWindowsConsoleKey, PaneTargetRef, TerminalSize, WebTerminalPalette,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};

use crate::pane_io::PaneOutputReceiver;
use crate::web::{
    WebSessionTarget, WebShareAccess, WebShareConnectionCounts, WebShareRevokeReason,
};

use super::{WebPaneSnapshot, WebSessionSnapshot};

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
    pub(crate) attach_pid: u32,
    pub(crate) revoke_rx: tokio::sync::watch::Receiver<Option<WebShareRevokeReason>>,
    pub(crate) target: WebSessionTarget,
    pub(crate) snapshot: WebSessionSnapshot,
    pub(crate) writer: WriteHalf<DuplexStream>,
    pub(crate) reader: Option<WebSessionAttachReader>,
    pub(crate) selected_window_index: Option<u32>,
}

pub(crate) struct WebSessionAttachReader {
    reader: ReadHalf<DuplexStream>,
    decoder: AttachFrameDecoder,
    read_buffer: [u8; ATTACH_READ_BUFFER_SIZE],
}

pub(crate) enum WebSessionAttachEvent {
    Data(Vec<u8>),
    Resize,
}

impl WebPaneStream {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        self.access.origin_allowed(received)
    }

    pub(crate) fn is_operator(&self) -> bool {
        self.access.is_operator()
    }

    pub(crate) fn has_operator_access(&self) -> bool {
        self.access.has_operator_access()
    }

    pub(crate) fn has_spectator_access(&self) -> bool {
        self.access.has_spectator_access()
    }

    pub(crate) fn share_id(&self) -> &str {
        self.access.share_id()
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        self.access.expires_at()
    }

    pub(crate) fn connection_counts(&self) -> WebShareConnectionCounts {
        self.access.connection_counts()
    }

    pub(crate) fn target(&self) -> &PaneTargetRef {
        &self.target
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self.access.terminal_palette()
    }

    pub(crate) fn show_viewers(&self) -> bool {
        self.access.show_viewers()
    }

    pub(crate) fn operator_visible_spectator_pairing_code(&self) -> Option<&str> {
        self.access.operator_visible_spectator_pairing_code()
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

    pub(crate) fn has_operator_access(&self) -> bool {
        match self {
            Self::Pane(stream) => stream.has_operator_access(),
            Self::Session(stream) => stream.has_operator_access(),
        }
    }

    pub(crate) fn has_spectator_access(&self) -> bool {
        match self {
            Self::Pane(stream) => stream.has_spectator_access(),
            Self::Session(stream) => stream.has_spectator_access(),
        }
    }

    pub(crate) fn share_id(&self) -> &str {
        match self {
            Self::Pane(stream) => stream.share_id(),
            Self::Session(stream) => stream.share_id(),
        }
    }

    pub(crate) fn session_name(&self) -> Option<&str> {
        match self {
            Self::Pane(_) => None,
            Self::Session(stream) => Some(stream.target().name().as_str()),
        }
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        match self {
            Self::Pane(stream) => stream.expires_at(),
            Self::Session(stream) => stream.expires_at(),
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

    pub(crate) fn connection_counts(&self) -> WebShareConnectionCounts {
        match self {
            Self::Pane(stream) => stream.connection_counts(),
            Self::Session(stream) => stream.connection_counts(),
        }
    }

    pub(crate) fn show_viewers(&self) -> bool {
        match self {
            Self::Pane(stream) => stream.show_viewers(),
            Self::Session(stream) => stream.show_viewers(),
        }
    }

    pub(crate) fn operator_visible_spectator_pairing_code(&self) -> Option<&str> {
        match self {
            Self::Pane(stream) => stream.operator_visible_spectator_pairing_code(),
            Self::Session(stream) => stream.operator_visible_spectator_pairing_code(),
        }
    }

    pub(crate) fn role(&self) -> &'static str {
        if self.is_operator() {
            "operator"
        } else {
            "spectator"
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

    pub(crate) fn has_operator_access(&self) -> bool {
        self.access.has_operator_access()
    }

    pub(crate) fn has_spectator_access(&self) -> bool {
        self.access.has_spectator_access()
    }

    pub(crate) fn share_id(&self) -> &str {
        self.access.share_id()
    }

    pub(crate) fn controls(&self) -> bool {
        self.access.controls()
    }

    pub(crate) fn is_resize_authority(&self) -> bool {
        self.access.is_resize_authority()
    }

    pub(crate) fn expires_at(&self) -> Option<std::time::SystemTime> {
        self.access.expires_at()
    }

    pub(crate) fn connection_counts(&self) -> WebShareConnectionCounts {
        self.access.connection_counts()
    }

    pub(crate) fn target(&self) -> &WebSessionTarget {
        &self.target
    }

    pub(crate) const fn attach_pid(&self) -> u32 {
        self.attach_pid
    }

    pub(crate) const fn size(&self) -> TerminalSize {
        self.snapshot.size
    }

    pub(crate) const fn selected_window_index(&self) -> Option<u32> {
        self.selected_window_index
    }

    pub(crate) fn select_window_for_view(&mut self, window_index: u32) {
        self.selected_window_index = Some(window_index);
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self.access.terminal_palette()
    }

    pub(crate) fn show_viewers(&self) -> bool {
        self.access.show_viewers()
    }

    pub(crate) fn operator_visible_spectator_pairing_code(&self) -> Option<&str> {
        self.access.operator_visible_spectator_pairing_code()
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

    pub(crate) async fn send_attach_windows_console_key(
        &mut self,
        bytes: Vec<u8>,
        key: AttachedWindowsConsoleKey,
    ) -> io::Result<()> {
        self.write_attach_message(AttachMessage::Keystroke(
            AttachedKeystroke::new(bytes).with_windows_console_key(key),
        ))
        .await
    }

    pub(crate) async fn send_attach_resize(&mut self, size: TerminalSize) -> io::Result<()> {
        self.write_attach_message(AttachMessage::Resize(size)).await
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

    pub(crate) async fn read_event(&mut self) -> io::Result<Option<WebSessionAttachEvent>> {
        loop {
            if let Some(message) = self
                .decoder
                .next_message()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
            {
                match message {
                    AttachMessage::Data(bytes) | AttachMessage::Render(bytes) => {
                        if !bytes.is_empty() {
                            return Ok(Some(WebSessionAttachEvent::Data(bytes)));
                        }
                    }
                    AttachMessage::Resize(_size) => {
                        return Ok(Some(WebSessionAttachEvent::Resize));
                    }
                    AttachMessage::ResizeGeometry(_geometry) => {
                        return Ok(Some(WebSessionAttachEvent::Resize));
                    }
                    AttachMessage::KeyDispatched(_) => continue,
                    AttachMessage::Lock(_)
                    | AttachMessage::LockShellCommand(_)
                    | AttachMessage::Unlock
                    | AttachMessage::Suspend
                    | AttachMessage::DetachKill
                    | AttachMessage::DetachExec(_)
                    | AttachMessage::DetachExecShellCommand(_)
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
