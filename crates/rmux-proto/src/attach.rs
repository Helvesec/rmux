//! Attach-stream message codec shared by client and server.

use crate::{RmuxError, TerminalGeometry, TerminalSize, DEFAULT_MAX_FRAME_LENGTH};
use serde::{Deserialize, Serialize};

const DATA_TAG: u8 = 1;
const RESIZE_TAG: u8 = 2;
const LOCK_TAG: u8 = 3;
const UNLOCK_TAG: u8 = 4;
const SUSPEND_TAG: u8 = 5;
const DETACH_KILL_TAG: u8 = 6;
const DETACH_EXEC_TAG: u8 = 7;
const KEYSTROKE_TAG: u8 = 8;
const KEY_DISPATCHED_TAG: u8 = 9;
const LOCK_SHELL_COMMAND_TAG: u8 = 10;
const DETACH_EXEC_SHELL_COMMAND_TAG: u8 = 11;
const RESIZE_GEOMETRY_TAG: u8 = 12;
const RENDER_TAG: u8 = 13;
const DATA_HEADER_LEN: usize = 5;
const RESIZE_FRAME_LEN: usize = 5;
const SINGLE_TAG_FRAME_LEN: usize = 1;

/// Encoded byte length of a raw-data attach frame header.
pub const ATTACH_DATA_HEADER_LEN: usize = DATA_HEADER_LEN;

/// Typed attach-stream input captured from an attached client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedKeystroke {
    bytes: Vec<u8>,
}

impl AttachedKeystroke {
    /// Creates a typed keystroke from the terminal byte sequence read by the client.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    /// Returns the terminal byte sequence carried by this typed keystroke.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Structured acknowledgement returned after the server receives a typed keystroke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyDispatched {
    byte_len: u32,
    consumed: bool,
}

impl KeyDispatched {
    /// Creates a consumed dispatch acknowledgement for the received keystroke byte length.
    #[must_use]
    pub fn new(byte_len: u32) -> Self {
        Self {
            byte_len,
            consumed: true,
        }
    }

    /// Creates a dispatch acknowledgement for key bytes forwarded to the pane.
    #[must_use]
    pub fn forwarded(byte_len: u32) -> Self {
        Self {
            byte_len,
            consumed: false,
        }
    }

    /// Returns the number of key bytes acknowledged by the server.
    #[must_use]
    pub fn byte_len(&self) -> u32 {
        self.byte_len
    }

    /// Returns whether the server consumed the key before it reached the pane.
    #[must_use]
    pub fn consumed(&self) -> bool {
        self.consumed
    }
}

/// A local command request with the server-resolved shell context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachShellCommand {
    command: String,
    shell: String,
    cwd: String,
}

impl AttachShellCommand {
    /// Creates a local command request that must run through `shell` in `cwd`.
    #[must_use]
    pub fn new(command: String, shell: String, cwd: String) -> Self {
        Self {
            command,
            shell,
            cwd,
        }
    }

    /// Returns the tmux command payload to pass to the shell.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Returns the server-resolved shell executable path.
    #[must_use]
    pub fn shell(&self) -> &str {
        &self.shell
    }

    /// Returns the server-resolved command working directory.
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
}

/// All message types supported after the attach upgrade boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachMessage {
    /// Raw pane I/O bytes.
    Data(Vec<u8>),
    /// Rendered terminal frame bytes that supersede older render frames.
    Render(Vec<u8>),
    /// Typed key input from an attached client.
    Keystroke(AttachedKeystroke),
    /// Structured acknowledgement for a typed key input message.
    KeyDispatched(KeyDispatched),
    /// A terminal resize event.
    Resize(TerminalSize),
    /// A terminal resize event that includes optional pixel geometry.
    ResizeGeometry(TerminalGeometry),
    /// A request for the client to run the configured lock command locally.
    Lock(String),
    /// A request for the client to run the configured lock command through the
    /// server-resolved shell profile.
    LockShellCommand(AttachShellCommand),
    /// A notification that the local lock command has completed.
    Unlock,
    /// A request for the client to suspend itself and later resume in raw mode.
    Suspend,
    /// A request for the client to terminate itself after detaching.
    DetachKill,
    /// A request for the client to run a shell command locally before detaching.
    DetachExec(String),
    /// A request for the client to run a shell command locally before detaching
    /// through the server-resolved shell profile.
    DetachExecShellCommand(AttachShellCommand),
}

/// Encodes a single attach-stream message.
pub fn encode_attach_message(message: &AttachMessage) -> Result<Vec<u8>, RmuxError> {
    match message {
        AttachMessage::Data(bytes) => encode_data_message(bytes),
        AttachMessage::Render(bytes) => encode_data_like_message(RENDER_TAG, bytes),
        AttachMessage::Keystroke(keystroke) => encode_structured_message(KEYSTROKE_TAG, keystroke),
        AttachMessage::KeyDispatched(response) => {
            encode_structured_message(KEY_DISPATCHED_TAG, response)
        }
        AttachMessage::Resize(size) => Ok(encode_resize_message(*size)),
        AttachMessage::ResizeGeometry(geometry) => {
            encode_structured_message(RESIZE_GEOMETRY_TAG, geometry)
        }
        AttachMessage::Lock(command) => encode_data_like_message(LOCK_TAG, command.as_bytes()),
        AttachMessage::LockShellCommand(command) => {
            encode_structured_message(LOCK_SHELL_COMMAND_TAG, command)
        }
        AttachMessage::Unlock => Ok(vec![UNLOCK_TAG]),
        AttachMessage::Suspend => Ok(vec![SUSPEND_TAG]),
        AttachMessage::DetachKill => Ok(vec![DETACH_KILL_TAG]),
        AttachMessage::DetachExec(command) => {
            encode_data_like_message(DETACH_EXEC_TAG, command.as_bytes())
        }
        AttachMessage::DetachExecShellCommand(command) => {
            encode_structured_message(DETACH_EXEC_SHELL_COMMAND_TAG, command)
        }
    }
}

/// Encodes raw attach data bytes without first materializing an [`AttachMessage`].
pub fn encode_attach_data(bytes: &[u8]) -> Result<Vec<u8>, RmuxError> {
    encode_data_message(bytes)
}

/// Encodes raw attach data bytes into a caller-provided buffer.
///
/// Returns the number of initialized bytes in `frame`. This avoids a heap
/// allocation for small hot-path attach data frames.
pub fn encode_attach_data_into_slice(bytes: &[u8], frame: &mut [u8]) -> Result<usize, RmuxError> {
    encode_data_like_message_into_slice(DATA_TAG, bytes, frame)
}

/// A borrowed raw-data attach frame decoded from an input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachDataFrame<'a> {
    payload: &'a [u8],
    frame_len: usize,
}

impl<'a> AttachDataFrame<'a> {
    /// Returns the raw pane bytes carried by this data frame.
    #[must_use]
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }

    /// Returns the total encoded frame length, including tag and length header.
    #[must_use]
    pub const fn frame_len(self) -> usize {
        self.frame_len
    }
}

/// Attempts to decode a complete raw-data frame without allocating.
///
/// Returns `Ok(None)` when the buffer is empty, starts with another attach
/// message type, or does not yet contain the full data payload.
pub fn decode_attach_data_frame(input: &[u8]) -> Result<Option<AttachDataFrame<'_>>, RmuxError> {
    decode_attach_data_frame_with_limit(input, DEFAULT_MAX_FRAME_LENGTH)
}

/// Attempts to decode a complete raw-data frame with an explicit payload limit.
pub fn decode_attach_data_frame_with_limit(
    input: &[u8],
    max_data_length: usize,
) -> Result<Option<AttachDataFrame<'_>>, RmuxError> {
    if input.first().copied() != Some(DATA_TAG) {
        return Ok(None);
    }
    if input.len() < DATA_HEADER_LEN {
        return Ok(None);
    }

    let length = u32::from_le_bytes(
        input[1..DATA_HEADER_LEN]
            .try_into()
            .map_err(|_| RmuxError::Decode("invalid attach data header".to_owned()))?,
    ) as usize;
    if length > max_data_length {
        return Err(RmuxError::FrameTooLarge {
            length,
            maximum: max_data_length,
        });
    }

    let frame_len = DATA_HEADER_LEN + length;
    if input.len() < frame_len {
        return Ok(None);
    }

    Ok(Some(AttachDataFrame {
        payload: &input[DATA_HEADER_LEN..frame_len],
        frame_len,
    }))
}

/// Incremental decoder for attach-stream messages.
#[derive(Debug, Clone)]
pub struct AttachFrameDecoder {
    max_data_length: usize,
    buffer: Vec<u8>,
}

impl AttachFrameDecoder {
    /// Creates a decoder with the default maximum attach payload length.
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_data_length(DEFAULT_MAX_FRAME_LENGTH)
    }

    /// Creates a decoder with a custom maximum attach payload length.
    #[must_use]
    pub fn with_max_data_length(max_data_length: usize) -> Self {
        Self {
            max_data_length,
            buffer: Vec::new(),
        }
    }

    /// Appends raw attach-stream bytes to the decoder buffer.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Returns whether the decoder has no buffered partial frame.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Attempts to decode the next full attach-stream message.
    pub fn next_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        let Some(&tag) = self.buffer.first() else {
            return Ok(None);
        };

        match tag {
            DATA_TAG => self.next_data_message(),
            RESIZE_TAG => self.next_resize_message(),
            LOCK_TAG => self.next_lock_message(),
            UNLOCK_TAG => self.next_unlock_message(),
            SUSPEND_TAG => self.next_suspend_message(),
            DETACH_KILL_TAG => self.next_detach_kill_message(),
            DETACH_EXEC_TAG => self.next_detach_exec_message(),
            KEYSTROKE_TAG => self.next_keystroke_message(),
            KEY_DISPATCHED_TAG => self.next_key_dispatched_message(),
            LOCK_SHELL_COMMAND_TAG => self.next_lock_shell_command_message(),
            DETACH_EXEC_SHELL_COMMAND_TAG => self.next_detach_exec_shell_command_message(),
            RESIZE_GEOMETRY_TAG => self.next_resize_geometry_message(),
            RENDER_TAG => self.next_render_message(),
            other => {
                self.buffer.clear();
                Err(RmuxError::Decode(format!(
                    "unknown attach-stream message tag {other}"
                )))
            }
        }
    }

    /// Decodes the next raw-data message into `scratch` without allocating.
    ///
    /// Returns `Ok(None)` when the decoder is empty, the next message is not
    /// raw data, the frame is incomplete, or the payload is larger than
    /// `scratch`. In those cases the buffered bytes are left untouched so the
    /// caller can fall back to [`Self::next_message`].
    pub fn next_data_payload_into<'a>(
        &mut self,
        scratch: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, RmuxError> {
        let Some(&DATA_TAG) = self.buffer.first() else {
            return Ok(None);
        };
        if self.buffer.len() < DATA_HEADER_LEN {
            return Ok(None);
        }

        let length = u32::from_le_bytes(
            self.buffer[1..DATA_HEADER_LEN]
                .try_into()
                .map_err(|_| RmuxError::Decode("invalid attach data header".to_owned()))?,
        ) as usize;
        if length > self.max_data_length {
            self.buffer.clear();
            return Err(RmuxError::FrameTooLarge {
                length,
                maximum: self.max_data_length,
            });
        }

        let required = DATA_HEADER_LEN + length;
        if self.buffer.len() < required || length > scratch.len() {
            return Ok(None);
        }

        scratch[..length].copy_from_slice(&self.buffer[DATA_HEADER_LEN..required]);
        self.buffer.drain(..required);
        Ok(Some(&scratch[..length]))
    }

    fn next_data_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        if self.buffer.len() < DATA_HEADER_LEN {
            return Ok(None);
        }

        let length = u32::from_le_bytes(
            self.buffer[1..DATA_HEADER_LEN]
                .try_into()
                .map_err(|_| RmuxError::Decode("invalid attach data header".to_owned()))?,
        ) as usize;

        if length > self.max_data_length {
            self.buffer.clear();
            return Err(RmuxError::FrameTooLarge {
                length,
                maximum: self.max_data_length,
            });
        }

        let required = DATA_HEADER_LEN + length;
        if self.buffer.len() < required {
            return Ok(None);
        }

        self.buffer.drain(..DATA_HEADER_LEN);
        let bytes = self.buffer.drain(..length).collect();
        Ok(Some(AttachMessage::Data(bytes)))
    }

    fn next_resize_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        if self.buffer.len() < RESIZE_FRAME_LEN {
            return Ok(None);
        }

        let frame: Vec<u8> = self.buffer.drain(..RESIZE_FRAME_LEN).collect();
        let cols = u16::from_le_bytes(
            frame[1..3]
                .try_into()
                .map_err(|_| RmuxError::Decode("invalid attach resize columns".to_owned()))?,
        );
        let rows = u16::from_le_bytes(
            frame[3..5]
                .try_into()
                .map_err(|_| RmuxError::Decode("invalid attach resize rows".to_owned()))?,
        );

        Ok(Some(AttachMessage::Resize(TerminalSize { cols, rows })))
    }

    fn next_render_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_data_like_message(RENDER_TAG)
            .map(|message| message.map(AttachMessage::Render))
    }

    fn next_resize_geometry_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_structured_message(RESIZE_GEOMETRY_TAG)
            .map(|message| message.map(AttachMessage::ResizeGeometry))
    }

    fn next_lock_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_data_like_message(LOCK_TAG).map(|message| {
            message.map(|bytes| AttachMessage::Lock(String::from_utf8_lossy(&bytes).into_owned()))
        })
    }

    fn next_lock_shell_command_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_structured_message(LOCK_SHELL_COMMAND_TAG)
            .map(|message| message.map(AttachMessage::LockShellCommand))
    }

    fn next_unlock_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        if self.buffer.len() < SINGLE_TAG_FRAME_LEN {
            return Ok(None);
        }

        let frame: Vec<u8> = self.buffer.drain(..SINGLE_TAG_FRAME_LEN).collect();
        if frame[0] != UNLOCK_TAG {
            self.buffer.clear();
            return Err(RmuxError::Decode("invalid attach unlock frame".to_owned()));
        }

        Ok(Some(AttachMessage::Unlock))
    }

    fn next_suspend_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        if self.buffer.len() < SINGLE_TAG_FRAME_LEN {
            return Ok(None);
        }

        let frame: Vec<u8> = self.buffer.drain(..SINGLE_TAG_FRAME_LEN).collect();
        if frame[0] != SUSPEND_TAG {
            self.buffer.clear();
            return Err(RmuxError::Decode("invalid attach suspend frame".to_owned()));
        }

        Ok(Some(AttachMessage::Suspend))
    }

    fn next_detach_kill_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        if self.buffer.len() < SINGLE_TAG_FRAME_LEN {
            return Ok(None);
        }

        let frame: Vec<u8> = self.buffer.drain(..SINGLE_TAG_FRAME_LEN).collect();
        if frame[0] != DETACH_KILL_TAG {
            self.buffer.clear();
            return Err(RmuxError::Decode(
                "invalid attach detach-kill frame".to_owned(),
            ));
        }

        Ok(Some(AttachMessage::DetachKill))
    }

    fn next_detach_exec_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_data_like_message(DETACH_EXEC_TAG).map(|message| {
            message.map(|bytes| {
                AttachMessage::DetachExec(String::from_utf8_lossy(&bytes).into_owned())
            })
        })
    }

    fn next_detach_exec_shell_command_message(
        &mut self,
    ) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_structured_message(DETACH_EXEC_SHELL_COMMAND_TAG)
            .map(|message| message.map(AttachMessage::DetachExecShellCommand))
    }

    fn next_keystroke_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_structured_message(KEYSTROKE_TAG)
            .map(|message| message.map(AttachMessage::Keystroke))
    }

    fn next_key_dispatched_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_structured_message(KEY_DISPATCHED_TAG)
            .map(|message| message.map(AttachMessage::KeyDispatched))
    }

    fn next_structured_message<T>(&mut self, tag: u8) -> Result<Option<T>, RmuxError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let Some(bytes) = self.next_data_like_message(tag)? else {
            return Ok(None);
        };

        bincode::deserialize(&bytes)
            .map(Some)
            .map_err(|error| RmuxError::Decode(format!("invalid attach structured frame: {error}")))
    }

    fn next_data_like_message(&mut self, tag: u8) -> Result<Option<Vec<u8>>, RmuxError> {
        if self.buffer.len() < DATA_HEADER_LEN {
            return Ok(None);
        }

        let length = u32::from_le_bytes(
            self.buffer[1..DATA_HEADER_LEN]
                .try_into()
                .map_err(|_| RmuxError::Decode("invalid attach data header".to_owned()))?,
        ) as usize;

        if length > self.max_data_length {
            self.buffer.clear();
            return Err(RmuxError::FrameTooLarge {
                length,
                maximum: self.max_data_length,
            });
        }

        let required = DATA_HEADER_LEN + length;
        if self.buffer.len() < required {
            return Ok(None);
        }

        if self.buffer[0] != tag {
            self.buffer.clear();
            return Err(RmuxError::Decode(
                "invalid attach data-like frame".to_owned(),
            ));
        }
        self.buffer.drain(..DATA_HEADER_LEN);
        Ok(Some(self.buffer.drain(..length).collect()))
    }
}

impl Default for AttachFrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn encode_data_message(bytes: &[u8]) -> Result<Vec<u8>, RmuxError> {
    encode_data_like_message(DATA_TAG, bytes)
}

fn encode_data_like_message(tag: u8, bytes: &[u8]) -> Result<Vec<u8>, RmuxError> {
    validate_data_like_payload(bytes)?;
    let length = u32::try_from(bytes.len()).map_err(|_| RmuxError::FrameTooLarge {
        length: bytes.len(),
        maximum: u32::MAX as usize,
    })?;

    let mut frame = Vec::with_capacity(DATA_HEADER_LEN + bytes.len());
    frame.push(tag);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(bytes);
    Ok(frame)
}

fn encode_data_like_message_into_slice(
    tag: u8,
    bytes: &[u8],
    frame: &mut [u8],
) -> Result<usize, RmuxError> {
    validate_data_like_payload(bytes)?;
    let frame_len = DATA_HEADER_LEN + bytes.len();
    if frame.len() < frame_len {
        return Err(RmuxError::Encode(format!(
            "attach frame buffer too small: need {frame_len}, have {}",
            frame.len()
        )));
    }

    let length = u32::try_from(bytes.len()).map_err(|_| RmuxError::FrameTooLarge {
        length: bytes.len(),
        maximum: u32::MAX as usize,
    })?;
    frame[0] = tag;
    frame[1..DATA_HEADER_LEN].copy_from_slice(&length.to_le_bytes());
    frame[DATA_HEADER_LEN..frame_len].copy_from_slice(bytes);
    Ok(frame_len)
}

fn validate_data_like_payload(bytes: &[u8]) -> Result<(), RmuxError> {
    if bytes.len() > DEFAULT_MAX_FRAME_LENGTH {
        return Err(RmuxError::FrameTooLarge {
            length: bytes.len(),
            maximum: DEFAULT_MAX_FRAME_LENGTH,
        });
    }
    Ok(())
}

fn encode_structured_message<T>(tag: u8, message: &T) -> Result<Vec<u8>, RmuxError>
where
    T: Serialize,
{
    let bytes = bincode::serialize(message)
        .map_err(|error| RmuxError::Encode(format!("invalid attach structured frame: {error}")))?;
    encode_data_like_message(tag, &bytes)
}

fn encode_resize_message(size: TerminalSize) -> Vec<u8> {
    let mut frame = Vec::with_capacity(RESIZE_FRAME_LEN);
    frame.push(RESIZE_TAG);
    frame.extend_from_slice(&size.cols.to_le_bytes());
    frame.extend_from_slice(&size.rows.to_le_bytes());
    frame
}

#[cfg(test)]
#[path = "attach/tests.rs"]
mod tests;
