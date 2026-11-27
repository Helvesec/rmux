use serde::Deserialize;

use super::{
    AttachFrameDecoder, AttachMessage, AttachedKeystroke, AttachedWindowsConsoleKeystroke,
    DATA_HEADER_LEN, DATA_TAG, DETACH_EXEC_SHELL_COMMAND_TAG, DETACH_EXEC_TAG, DETACH_KILL_TAG,
    KEYSTROKE_TAG, KEY_DISPATCHED_TAG, LOCK_SHELL_COMMAND_TAG, LOCK_TAG, RENDER_TAG,
    RESIZE_FRAME_LEN, RESIZE_GEOMETRY_TAG, RESIZE_TAG, SERVER_EXITED_TAG, SINGLE_TAG_FRAME_LEN,
    SUSPEND_TAG, UNLOCK_TAG, WINDOWS_CONSOLE_KEYSTROKE_TAG,
};
use crate::{RmuxError, TerminalSize, DEFAULT_MAX_FRAME_LENGTH};

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
            WINDOWS_CONSOLE_KEYSTROKE_TAG => self.next_windows_console_keystroke_message(),
            SERVER_EXITED_TAG => self.next_server_exited_message(),
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

    fn next_server_exited_message(&mut self) -> Result<Option<AttachMessage>, RmuxError> {
        if self.buffer.len() < SINGLE_TAG_FRAME_LEN {
            return Ok(None);
        }

        self.buffer.drain(..SINGLE_TAG_FRAME_LEN);
        Ok(Some(AttachMessage::ServerExited))
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
        self.next_structured_message(KEYSTROKE_TAG).map(|message| {
            message.map(|bytes| AttachMessage::Keystroke(AttachedKeystroke::new(bytes)))
        })
    }

    fn next_windows_console_keystroke_message(
        &mut self,
    ) -> Result<Option<AttachMessage>, RmuxError> {
        self.next_structured_message::<AttachedWindowsConsoleKeystroke>(
            WINDOWS_CONSOLE_KEYSTROKE_TAG,
        )
        .map(|message| {
            message.map(|keystroke| AttachMessage::Keystroke(keystroke.into_keystroke()))
        })
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
