//! Per-pane encoding for synchronized attached input.

use rmux_core::input::mode;
use rmux_proto::{PaneTarget, RmuxError};

use super::{bracketed_paste, AttachedPaneForward};
use crate::pane_terminals::HandlerState;

use super::super::pane_io_encoding::{
    encode_key_for_target, pane_input_mode, prepare_pane_input_write, synchronized_input_targets,
    write_attached_bytes_to_target_io, PaneInputLiveness, PaneInputWrite,
};
#[cfg(windows)]
use super::super::pane_io_encoding::{
    prepare_pane_console_input_write, should_emulate_windows_cmd_select_all,
    should_route_windows_control_as_pty_bytes, windows_console_input_for_attached_key,
    write_windows_console_input_action_to_target_io, PaneConsoleInputWrite,
    WindowsConsoleInputAction,
};

pub(super) enum PreparedAttachedPaneForward {
    EncodedKey {
        write: PaneInputWrite,
        bytes: Vec<u8>,
    },
    #[cfg(windows)]
    WindowsConsoleKey {
        write: PaneConsoleInputWrite,
        action: WindowsConsoleInputAction,
    },
}

pub(super) fn prepare_attached_key_forwards(
    state: &mut HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
    forward: AttachedPaneForward<'_>,
) -> Result<Vec<PreparedAttachedPaneForward>, RmuxError> {
    let targets = synchronized_input_targets(state, target)?;
    let mut prepared = Vec::with_capacity(targets.len());
    for target in targets {
        match forward {
            AttachedPaneForward::EncodedKey(_) => {
                append_encoded_key_forward(state, &target, key, &mut prepared)?;
            }
            #[cfg(windows)]
            AttachedPaneForward::WindowsConsoleKey {
                key: console_key,
                bytes,
            } => {
                let route_bytes = should_emulate_windows_cmd_select_all(state, &target, key)
                    || should_route_windows_control_as_pty_bytes(state, &target, key);
                if route_bytes {
                    append_encoded_key_forward(state, &target, key, &mut prepared)?;
                } else {
                    let action =
                        windows_console_input_for_attached_key(state, &target, key, console_key);
                    let write = prepare_pane_console_input_write(state, &target, bytes, action)?;
                    prepared.push(PreparedAttachedPaneForward::WindowsConsoleKey { write, action });
                }
            }
        }
    }
    Ok(prepared)
}

fn append_encoded_key_forward(
    state: &mut HandlerState,
    target: &PaneTarget,
    key: rmux_core::KeyCode,
    prepared: &mut Vec<PreparedAttachedPaneForward>,
) -> Result<(), RmuxError> {
    let Some(bytes) = encode_key_for_target(state, target, key)? else {
        return Ok(());
    };
    let write = prepare_pane_input_write(state, target, &bytes, PaneInputLiveness::TolerateDead)?;
    prepared.push(PreparedAttachedPaneForward::EncodedKey { write, bytes });
    Ok(())
}

pub(super) fn prepare_attached_bracketed_paste_forwards(
    state: &mut HandlerState,
    target: &PaneTarget,
    body: &[u8],
) -> Result<Vec<PreparedAttachedPaneForward>, RmuxError> {
    let targets = synchronized_input_targets(state, target)?;
    let mut prepared = Vec::with_capacity(targets.len());
    for target in targets {
        let bracketed = pane_input_mode(state, &target)? & mode::MODE_BRACKETPASTE != 0;
        let bytes = bracketed_paste::encode_bracketed_paste_for_mode(body, bracketed);
        if bytes.is_empty() {
            continue;
        }
        let write =
            prepare_pane_input_write(state, &target, &bytes, PaneInputLiveness::TolerateDead)?;
        prepared.push(PreparedAttachedPaneForward::EncodedKey { write, bytes });
    }
    Ok(prepared)
}

pub(super) async fn write_prepared_attached_pane_forwards(
    prepared: Vec<PreparedAttachedPaneForward>,
) -> Result<(), RmuxError> {
    for forward in prepared {
        match forward {
            PreparedAttachedPaneForward::EncodedKey { write, bytes } => {
                write_attached_bytes_to_target_io(write, bytes).await?;
            }
            #[cfg(windows)]
            PreparedAttachedPaneForward::WindowsConsoleKey { write, action } => {
                write_windows_console_input_action_to_target_io(write, action).await?;
            }
        }
    }
    Ok(())
}
