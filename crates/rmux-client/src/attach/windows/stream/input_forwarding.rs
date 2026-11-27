use std::io;

use rmux_proto::{AttachMessage, AttachedKeystroke, AttachedWindowsConsoleKey};
use tokio::sync::oneshot;

use crate::ClientError;

use super::write_async_attach_message;

pub(super) struct PendingRepeatedAttachInput {
    pub(super) input: super::super::input::AttachInput,
    pub(super) remaining: u16,
}

impl PendingRepeatedAttachInput {
    pub(super) fn new(input: super::super::input::AttachInput) -> Self {
        let remaining = input.repeat_count();
        debug_assert!(remaining > 1);
        Self { input, remaining }
    }

    pub(super) fn consume_one(&mut self) -> bool {
        self.remaining -= 1;
        self.remaining == 0
    }
}

pub(super) fn take_ready_input_worker_completion(
    completion_rx: &mut Option<oneshot::Receiver<std::result::Result<(), ClientError>>>,
) -> Option<std::result::Result<(), ClientError>> {
    let completion = match completion_rx.as_mut()?.try_recv() {
        Ok(completion) => completion,
        Err(oneshot::error::TryRecvError::Empty) => return None,
        Err(oneshot::error::TryRecvError::Closed) => Err(input_worker_stopped_error()),
    };
    *completion_rx = None;
    Some(completion)
}

pub(super) fn input_worker_stopped_error() -> ClientError {
    ClientError::Io(io::Error::other(
        "attach input worker stopped before reporting completion",
    ))
}

pub(super) async fn await_input_worker_completion(
    completion_rx: &mut Option<oneshot::Receiver<std::result::Result<(), ClientError>>>,
) -> std::result::Result<std::result::Result<(), ClientError>, oneshot::error::RecvError> {
    completion_rx
        .as_mut()
        .expect("guarded attach input completion receiver remains present")
        .await
}

pub(super) async fn write_attach_input_once<Writer>(
    writer: &mut Writer,
    input: &super::super::input::AttachInput,
    windows_console_key_enabled: bool,
) -> std::result::Result<(), ClientError>
where
    Writer: tokio::io::AsyncWrite + Unpin,
{
    let input_bytes = input.payload();
    let windows_console_key = if windows_console_key_enabled {
        input
            .windows_console_key()
            .map(single_repeat_windows_console_key)
    } else {
        None
    };
    for chunk in super::super::input::attach_input_chunks(input_bytes) {
        let mut keystroke = AttachedKeystroke::new(chunk.to_vec());
        if chunk.len() == input_bytes.len() {
            if let Some(key) = windows_console_key {
                keystroke = keystroke.with_windows_console_key(key);
            }
        }
        write_async_attach_message(&mut *writer, AttachMessage::Keystroke(keystroke)).await?;
    }
    Ok(())
}

fn single_repeat_windows_console_key(key: AttachedWindowsConsoleKey) -> AttachedWindowsConsoleKey {
    AttachedWindowsConsoleKey::new(
        key.virtual_key_code(),
        key.virtual_scan_code(),
        key.unicode_char(),
        key.control_key_state(),
        1,
    )
}
