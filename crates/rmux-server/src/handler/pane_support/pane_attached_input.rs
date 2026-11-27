use std::io;
use std::marker::PhantomData;

use rmux_core::{key_code_lookup_bits, key_string_lookup_string};
use rmux_proto::{PaneTarget, RmuxError, Target};
#[cfg(windows)]
use rmux_pty::WindowsConsoleKeyEvent;

use super::super::{prompt_support::decode_prompt_key, RequestHandler};
use super::pane_io_encoding::{
    prepare_attached_pane_input_writes, prepare_pane_input_write,
    write_attached_bytes_to_target_io, PaneInputLiveness,
};
use super::pane_prompt_input::decode_expired_escape_key;
use super::{io_other, resolve_input_target, AttachedKeyDispatch};
use crate::client_flags::ClientFlags;
use crate::handler::attach_support::{
    on_attached_input_path, replays_queued_input, ActiveAttachIdentity, InputArrival, OrderedInput,
    TransientMessageInput,
};
use crate::handler::overlay_support::AttachedOverlayInput;
use crate::input_keys::{decode_mouse, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};
use crate::pane_terminals::HandlerState;

pub(super) mod bracketed_paste;
mod click_timer_lifecycle;
#[cfg(test)]
pub(in crate::handler) use click_timer_lifecycle::AttachedMouseTimerPause;
#[cfg(test)]
pub(in crate::handler) use live::{LiveInputWork, LiveInputWorkProbe};
mod frame_arrival;
mod keyboard;
mod kitty_graphics;
pub(in crate::handler) use keyboard::HeldKeys;
mod assumed_paste;
mod live;
mod live_key_decode;
mod modal_input;
mod mouse_input;
mod palette_response;
mod retained;
mod session_state;
mod synchronized;
mod terminal_response;
mod theme;
mod user_key_input;
pub(in crate::handler) use assumed_paste::AssumedPasteState;

pub(in crate::handler) use palette_response::{
    decode_pane_bound_terminal_string, PaneBoundTerminalStringDecode,
};
pub(in crate::handler) use retained::{
    retain_partial_attached_control_input, retain_partial_attached_escape_input,
};
use synchronized::{
    prepare_attached_bracketed_paste_forwards, prepare_attached_key_forwards,
    write_prepared_attached_pane_forwards,
};
pub(in crate::handler) use terminal_response::{
    decode_attached_terminal_control_after_append, TerminalResponseDecode,
};

fn ensure_session_identity(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    session_id: rmux_proto::SessionId,
) -> Result<(), RmuxError> {
    state
        .sessions
        .session(session_name)
        .filter(|session| session.id() == session_id)
        .map(|_| ())
        .ok_or_else(|| RmuxError::Server("attached session disappeared".to_owned()))
}

fn ensure_target_session_identity(
    state: &HandlerState,
    target: &PaneTarget,
    session_id: rmux_proto::SessionId,
) -> Result<(), RmuxError> {
    ensure_session_identity(state, target.session_name(), session_id)
}

#[derive(Clone, Copy)]
enum AttachedPaneForward<'a> {
    ProjectedShiftEnter,
    EncodedKey(PhantomData<&'a ()>),
    #[cfg(windows)]
    WindowsConsoleKey {
        key: WindowsConsoleKeyEvent,
        bytes: &'a [u8],
    },
}

impl RequestHandler {
    async fn handle_attached_live_key(
        &self,
        identity: ActiveAttachIdentity,
        key: rmux_core::KeyCode,
    ) -> io::Result<bool> {
        self.handle_attached_live_key_inner(
            identity,
            key,
            AttachedPaneForward::EncodedKey(PhantomData),
        )
        .await
    }

    async fn handle_attached_live_key_inner(
        &self,
        identity: ActiveAttachIdentity,
        key: rmux_core::KeyCode,
        forward: AttachedPaneForward<'_>,
    ) -> io::Result<bool> {
        let attach_pid = identity.attach_pid();
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(false);
        }
        if self.mode_tree_active_for_identity(identity).await {
            self.handle_attached_mode_tree_key_or_prefix(identity, key, decode_prompt_key(key))
                .await?;
            return Ok(true);
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        // No copy-mode shim ahead of the tables: in copy mode the
        // copy-mode / copy-mode-vi table is the only authority, exactly as
        // tmux 3.7b behaves and as the detached `send-keys -X` path already
        // did. `dispatch_attached_key_inner` resolves the binding and swallows
        // the key when the table leaves it unbound.
        let dismissal = self
            .capture_key_dismissal(identity, &target, session_id)
            .await;
        let handled = self
            .dispatch_attached_key_inner(
                &target,
                AttachedKeyDispatch {
                    attach_pid,
                    live_identity: Some(identity),
                    live_session_id: Some(session_id),
                    requester_pid: attach_pid,
                    current_target: Some(Target::Pane(target.clone())),
                    mouse_target: None,
                    mouse_event: None,
                    key,
                    attached_live_input: true,
                },
            )
            .await
            .map_err(io_other)?;
        if handled {
            return Ok(true);
        }
        if let Some(intent) = dismissal {
            self.dismiss_kept_pane_by_key(intent).await?;
            return Ok(true);
        }
        let Some(prepared) = self
            .with_live_input_state(identity, &target, session_id, |state| {
                prepare_attached_key_forwards(state, &target, key, forward).map_err(io_other)
            })
            .await?
        else {
            return Ok(true);
        };
        write_prepared_attached_pane_forwards(prepared)
            .await
            .map_err(io_other)?;
        Ok(false)
    }

    async fn write_attached_bytes_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        bytes: &[u8],
    ) -> io::Result<()> {
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(());
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        let Some(writes) = self
            .with_live_input_state(identity, &target, session_id, |state| {
                prepare_attached_pane_input_writes(state, &target, bytes).map_err(io_other)
            })
            .await?
        else {
            return Ok(());
        };
        for write in writes {
            write_attached_bytes_to_target_io(write, bytes.to_vec())
                .await
                .map_err(io_other)?;
        }
        Ok(())
    }

    async fn write_attached_target_bytes_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        bytes: &[u8],
    ) -> io::Result<()> {
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(());
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        let Some(write) = self
            .with_live_input_state(identity, &target, session_id, |state| {
                prepare_pane_input_write(state, &target, bytes, PaneInputLiveness::TolerateDead)
                    .map_err(io_other)
            })
            .await?
        else {
            return Ok(());
        };
        write_attached_bytes_to_target_io(write, bytes.to_vec())
            .await
            .map_err(io_other)
    }

    async fn write_attached_bracketed_paste_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        body: &[u8],
    ) -> io::Result<()> {
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(());
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        let Some(prepared) = self
            .with_live_input_state(identity, &target, session_id, |state| {
                prepare_attached_bracketed_paste_forwards(state, &target, body).map_err(io_other)
            })
            .await?
        else {
            return Ok(());
        };
        write_prepared_attached_pane_forwards(prepared)
            .await
            .map_err(io_other)
    }

    #[cfg(test)]
    pub(crate) async fn flush_attached_pending_escape_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<bool> {
        let identity = self
            .active_attach_identity(attach_pid)
            .await
            .ok_or_else(|| io_other("attached client disappeared"))?;
        self.flush_attached_pending_escape_input_for_identity(identity, pending_input)
            .await
    }

    pub(crate) async fn flush_attached_pending_escape_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<bool> {
        on_attached_input_path(
            identity.attach_pid(),
            Box::pin(async move {
                let ordered = self
                    .order_input_behind_released_work(
                        identity,
                        pending_input,
                        &[],
                        None,
                        InputArrival::EscapeFlush,
                        // Nothing completes flushed input any more.
                        |_| 0,
                    )
                    .await;
                if matches!(ordered, OrderedInput::Waits { .. }) {
                    return Ok(false);
                }
                self.flush_attached_pending_escape_input_on_input_path(identity, pending_input)
                    .await
            }),
        )
        .await
    }

    async fn flush_attached_pending_escape_input_on_input_path(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<bool> {
        let mut forwarded = false;
        loop {
            let mut repeat = false;
            forwarded |= self
                .flush_attached_escape_fragment(identity, pending_input, &mut repeat)
                .await?;
            if !repeat {
                return Ok(forwarded);
            }
        }
    }

    async fn flush_attached_escape_fragment(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        repeat: &mut bool,
    ) -> io::Result<bool> {
        if pending_input.is_empty() {
            return Ok(false);
        }

        if self
            .timed_out_keyboard_frame_is_quarantined(identity, pending_input)
            .await
        {
            pending_input.clear();
            return Ok(false);
        }

        // Match the live input path: a read-only client's retained input is
        // discarded before any mode handling, so a client switched to
        // read-only mid-retention cannot exit clock mode or cancel copy mode
        // through the flush.
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            pending_input.clear();
            return Ok(false);
        }

        // An unterminated SGR mouse field that has already overflowed `u16`
        // can never become a valid event. Forwarding its CSI bytes on timeout
        // would merely move the incomplete control sequence into the pane's
        // terminal parser, where it could consume otherwise ordinary input.
        // Discard it before mode-specific Escape handling so prompts,
        // overlays, mode trees, copy mode, and the live pane agree.
        if matches!(decode_mouse(pending_input, None), MouseDecode::Overlong) {
            pending_input.clear();
            return Ok(false);
        }

        if let Some(body) = bracketed_paste::neutralize_timed_out_bracketed_paste(pending_input) {
            pending_input.clear();
            if body.is_empty() {
                return Ok(false);
            }
            let completed_paste = bracketed_paste::encode_bracketed_paste_for_mode(&body, true);
            return self
                .handle_attached_live_input_inner_for_identity(
                    identity,
                    pending_input,
                    &completed_paste,
                )
                .await;
        }

        match self
            .handle_transient_message_input_for_identity(identity, pending_input, &[])
            .await
        {
            // The message may have expired before this identity-guarded
            // operation. Continue with the ordinary flush rules so retained
            // bytes are neither dropped nor left stuck.
            TransientMessageInput::Inactive => {
                let residual = self
                    .take_transient_terminal_prefix_for_identity(identity)
                    .await;
                if !residual.is_empty() {
                    pending_input.extend(residual);
                    return Ok(false);
                }
            }
            TransientMessageInput::Consumed => return Ok(false),
            TransientMessageInput::TerminalControls(controls) => {
                return self
                    .handle_transient_terminal_controls(identity, controls)
                    .await;
            }
            TransientMessageInput::Dismissed(bytes) => {
                return self
                    .handle_attached_live_input_inner_for_identity(identity, pending_input, &bytes)
                    .await;
            }
        }

        if pending_input.first() == Some(&b'\x1b')
            && input_arrives_now(identity)
            && self.prompt_active_for_identity(identity).await
        {
            // Only a lone Escape is Escape: an unfinished key is Meta of its next character.
            let (key, consumed) = decode_expired_escape_key(pending_input, Some(0x7f))
                .ok_or_else(|| io_other("Escape key is unavailable"))?;
            pending_input.drain(..consumed);
            let mut deferred_refresh = false;
            self.handle_prompt_event_deferred_refresh_for_identity(
                identity,
                decode_prompt_key(key),
                &mut deferred_refresh,
            )
            .await
            .map_err(io_other)?;
            if deferred_refresh && self.prompt_active_for_identity(identity).await {
                self.flush_attached_prompt_refresh_for_identity(identity)
                    .await
                    .map_err(io_other)?;
            }
            return if pending_input.is_empty() {
                Ok(false)
            } else {
                let remaining = std::mem::take(pending_input);
                self.handle_attached_live_input_inner_for_identity(
                    identity,
                    pending_input,
                    &remaining,
                )
                .await
            };
        }

        if pending_input.first() == Some(&b'\x1b')
            && input_arrives_now(identity)
            && self.overlay_active_for_identity(identity).await
        {
            return match self
                .flush_attached_overlay_escape_input_for_identity(identity, pending_input)
                .await?
            {
                AttachedOverlayInput::Consumed => Ok(false),
                AttachedOverlayInput::Reroute(bytes) => {
                    self.handle_attached_live_input_inner_for_identity(
                        identity,
                        pending_input,
                        &bytes,
                    )
                    .await
                }
            };
        }

        if pending_input.first() == Some(&b'\x1b')
            && self.mode_tree_active_for_identity(identity).await
        {
            let backspace = self.attached_backspace_byte().await;
            let (key, consumed) = decode_expired_escape_key(pending_input, backspace)
                .ok_or_else(|| io_other("Escape key is unavailable"))?;
            pending_input.drain(..consumed);
            self.handle_attached_mode_tree_key_or_prefix(identity, key, decode_prompt_key(key))
                .await?;
            return if pending_input.is_empty() {
                Ok(false)
            } else {
                let remaining = std::mem::take(pending_input);
                self.handle_attached_live_input_inner_for_identity(
                    identity,
                    pending_input,
                    &remaining,
                )
                .await
            };
        }

        if pending_input.starts_with(b"\x1b]") || pending_input.starts_with(b"\x1b_") {
            // Ambiguous between a fragmented consumed terminal response or
            // kitty graphics APC and the M-] / M-_ keystroke; the expired
            // deadline resolves it as keyboard input. Dispatch through the
            // live key path (key tables, copy mode, per-pane encoding)
            // instead of the raw ESC handling below, which would treat the
            // retained bytes as a bare Escape plus literal text.
            return self
                .flush_attached_consumed_osc_prefix(identity, pending_input)
                .await;
        }

        let bytes = std::mem::take(pending_input);
        if bytes.first() == Some(&b'\x1b') {
            let (target, target_session_id) = self
                .attached_input_target_identity(identity)
                .await
                .map_err(io_other)?;
            let forwarded = if self
                .target_is_in_clock_mode(&target)
                .await
                .map_err(io_other)?
            {
                #[cfg(test)]
                self.pause_before_live_clock_mode_exit_for_test(identity)
                    .await;
                !self
                    .exit_clock_mode_for_attached_identity(&target, identity, target_session_id)
                    .await
                    .map_err(io_other)?
            } else {
                // The timeout completes a legacy Escape press. Bindings and
                // pane encoding must receive it just like a decoded key.
                let escape = key_string_lookup_string("Escape")
                    .ok_or_else(|| io_other("Escape key is unavailable"))?;
                !self.handle_attached_live_key(identity, escape).await?
            };
            return if let Some(remaining) = bytes.get(1..).filter(|bytes| !bytes.is_empty()) {
                let rest = self
                    .handle_attached_live_input_inner_for_identity(
                        identity,
                        pending_input,
                        remaining,
                    )
                    .await?;
                // Every retained byte belonged to the expired deadline. Recheck
                // the current surface without recursively polling an ESC burst.
                *repeat = !pending_input.is_empty();
                Ok(forwarded || rest)
            } else {
                Ok(forwarded)
            };
        }
        if let AttachedKeyDecode::Matched { size, key } = decode_attached_key(&bytes, None) {
            if size == bytes.len() {
                let handled = self.handle_attached_live_key(identity, key).await?;
                return Ok(!handled);
            }
        }
        self.write_attached_bytes_for_identity(identity, &bytes)
            .await?;
        pending_input.clear();
        Ok(true)
    }

    async fn record_attached_submitted_text(
        &self,
        identity: ActiveAttachIdentity,
        bytes: &[u8],
    ) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
        let _ = self
            .with_live_input_state(identity, &target, session_id, |state| {
                state
                    .record_attached_submitted_text(&target, bytes)
                    .map_err(io_other)
            })
            .await?;
        Ok(())
    }
}

/// Whether this input of the client arrives now rather than being replayed from behind its
/// released bindings. tmux hands a popup, menu, prompt or display-panes only the keys that arrive
/// while it is up; replayed input arrived while none took the client's input, so it goes on to
/// the key tables and the pane even if one has opened since.
fn input_arrives_now(identity: ActiveAttachIdentity) -> bool {
    !replays_queued_input(identity.attach_pid())
}

fn is_mouse_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[M") || bytes.starts_with(b"\x1b[<")
}

fn is_enter_key(key: rmux_core::KeyCode) -> bool {
    key_string_lookup_string("Enter")
        .is_some_and(|enter| key_code_lookup_bits(enter) == key_code_lookup_bits(key))
}
