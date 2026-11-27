use std::borrow::Cow;
use std::collections::VecDeque;
use std::io;
use std::sync::{atomic::Ordering, Arc};

use rmux_core::kitty_keyboard::Event;
use rmux_core::{input::mode, KeyCode, LifecycleEvent};
#[cfg(windows)]
use rmux_core::{
    key_code_lookup_bits, key_string_lookup_string, KEYC_CTRL, KEYC_IMPLIED_META, KEYC_META,
    KEYC_SHIFT,
};
use rmux_proto::{AttachedKeystroke, PaneTarget, WindowTarget};
#[cfg(windows)]
use rmux_pty::WindowsConsoleKeyEvent;

use super::super::super::RequestHandler;
use super::super::decode_prompt_input_event;
use super::super::io_other;
use super::super::pane_io_encoding::{
    prepare_attached_pane_input_writes, write_attached_bytes_to_target_io,
};
use super::super::pane_prompt_input::{
    decode_utf8_char, is_extended_key_prefix, is_utf8_lead_byte, utf8_expected_len,
};
use super::bracketed_paste::{
    decode_bracketed_paste_after_append, take_incomplete_bracketed_paste_segment,
    BracketedPasteDecode,
};
use super::kitty_graphics::{decode_kitty_graphics_apc_after_append, KittyGraphicsApcDecode};
use super::live_key_decode::{
    decode_live_attached_key, is_plain_attached_fast_path_input, plain_input_requires_key_dispatch,
    PlainKeyDispatchSnapshot,
};
use super::palette_response::{
    decode_pane_bound_terminal_string, ModalPaletteInput, ModalPaletteInputSegment,
    PaneBoundTerminalStringDecode,
};
use super::retained::MAX_RETAINED_ATTACHED_CONTROL_INPUT;
use super::terminal_response::{
    decode_attached_terminal_control_after_append, decode_focus_event, TerminalControlEvent,
    TerminalResponseDecode,
};
use super::{
    input_arrives_now, is_enter_key, is_mouse_prefix, resolve_input_target,
    retain_partial_attached_control_input, retain_partial_attached_escape_input,
};
use crate::client_flags::ClientFlags;
#[cfg(test)]
use crate::handler::attach_support::test_barrier;
use crate::handler::overlay_support::AttachedOverlayInput;
use crate::handler::{
    attach_support::{
        on_attached_input_path, ActiveAttachIdentity, FrameCursor, FramePart, InputArrival,
        OrderedInput, TransientMessageInput,
    },
    focus_support::AttachFocusReport,
    prepare_lifecycle_event,
};
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};

mod activity;
mod decode;
mod dispatch;
mod fast_path;
mod frame_order;
mod modal;
mod read_only_client_action;
#[cfg(windows)]
mod windows;
#[cfg(test)]
mod work_probe;
mod work_queue;

use decode::LiveInputDecode;
#[cfg(windows)]
pub(super) use windows::windows_console_key_event;
#[cfg(windows)]
use windows::{
    windows_console_binding_key, windows_console_binding_override_key, windows_key_code_named,
    windows_synthetic_console_key_for_decoded_key,
};
#[cfg(test)]
pub(in crate::handler) use work_probe::{LiveInputWork, LiveInputWorkProbe};

pub(crate) type ActiveClientEmitCache = Option<(u64, WindowTarget)>;

// Rerouting must re-evaluate attach state after hooks, mouse bindings, and prefix
// commands. Keep each reroute suffix bounded so one maximum-size local frame
// cannot turn those necessary copies into quadratic work.
const ATTACHED_LIVE_REROUTE_CHUNK_BYTES: usize = 1024;

enum AttachedLiveInputStep {
    Complete(bool),
    Reroute { bytes: Vec<u8>, forwarded: bool },
}

/// How a chunk of live input went on; `Waits` when what a key handed back waits behind released
/// bindings.
enum AttachedLiveChunkResult {
    Complete(bool),
    Rechunk { bytes: Vec<u8>, forwarded: bool },
    Waits(bool),
}

/// How many bytes at the end of live input the input path keeps pending until its next bytes or the
/// escape time: a lone ESC or ESC [, or an unfinished keyboard sequence.
fn unfinished_live_tail(input: &[u8]) -> usize {
    let Some(start) = input.iter().rposition(|byte| *byte == 0x1b) else {
        return 0;
    };
    let tail = &input[start..];
    if tail == b"\x1b" || tail == b"\x1b[" || super::keyboard::keyboard_prefix(tail) {
        tail.len()
    } else {
        0
    }
}

/// How a live frame went on against the client's released bindings.
enum Deferral {
    /// Its ordinary input waits; acting on its focus reports forwarded this.
    Waited(bool),
    /// The input path handles it now, part after part.
    HandleNow(FrameCursor),
}

/// A modal reader sees the legacy CR for Shift+Enter. Keep its origin until
/// the pane write, since an unbound key can pass through a transient message.
#[derive(Clone, Copy)]
pub(super) enum LegacyInput<'a> {
    Bytes(&'a [u8]),
    ShiftEnter,
}

impl<'a> LegacyInput<'a> {
    fn bytes(self) -> &'a [u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::ShiftEnter => b"\r",
        }
    }
}

struct AttachedLiveInputWork {
    bytes: Arc<[u8]>,
    start: usize,
    end: usize,
    windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
}

impl AttachedLiveInputWork {
    fn new(
        bytes: Arc<[u8]>,
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
    ) -> Self {
        let end = bytes.len();
        Self {
            bytes,
            start: 0,
            end,
            windows_console_key,
        }
    }

    fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    fn split_at(&mut self, len: usize) -> Self {
        let split = self.start.saturating_add(len).min(self.end);
        let remaining = Self {
            bytes: Arc::clone(&self.bytes),
            start: split,
            end: self.end,
            windows_console_key: None,
        };
        self.end = split;
        remaining
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }
}

fn take_attached_remaining_input(pending_input: &mut Vec<u8>, consumed: usize) -> Option<Vec<u8>> {
    if consumed >= pending_input.len() {
        return None;
    }
    let remaining = pending_input[consumed..].to_vec();
    pending_input.clear();
    Some(remaining)
}

impl RequestHandler {
    #[cfg(all(test, windows))]
    pub(crate) async fn handle_attached_keystroke_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        keystroke: &AttachedKeystroke,
    ) -> io::Result<bool> {
        let identity = self
            .active_attach_identity(attach_pid)
            .await
            .ok_or_else(|| io_other("attached client disappeared"))?;
        self.handle_attached_keystroke_input_for_identity(identity, pending_input, keystroke)
            .await
    }

    #[cfg(all(test, windows))]
    pub(crate) async fn handle_attached_keystroke_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        keystroke: &AttachedKeystroke,
    ) -> io::Result<bool> {
        let mut active_emit_cache = None;
        self.handle_attached_live_input_inner_with_windows_console_key(
            identity,
            pending_input,
            keystroke.bytes(),
            keystroke.windows_console_key(),
            &mut active_emit_cache,
        )
        .await
    }

    pub(crate) async fn handle_attached_keystroke_input_with_active_cache_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        keystroke: &AttachedKeystroke,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        self.handle_attached_live_input_inner_with_windows_console_key(
            identity,
            pending_input,
            keystroke.bytes(),
            keystroke.windows_console_key(),
            active_emit_cache,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn handle_attached_live_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<()> {
        let identity = self
            .active_attach_identity(attach_pid)
            .await
            .ok_or_else(|| io_other("attached client disappeared"))?;
        let mut active_emit_cache = None;
        self.handle_attached_live_input_inner_cached(
            identity,
            pending_input,
            bytes,
            &mut active_emit_cache,
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn handle_attached_live_input_with_active_cache_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        self.handle_attached_live_input_inner_cached(
            identity,
            pending_input,
            bytes,
            active_emit_cache,
        )
        .await
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn handle_attached_live_input_inner(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let identity = self
            .active_attach_identity(attach_pid)
            .await
            .ok_or_else(|| io_other("attached client disappeared"))?;
        let mut active_emit_cache = None;
        self.handle_attached_live_input_inner_cached(
            identity,
            pending_input,
            bytes,
            &mut active_emit_cache,
        )
        .await
    }

    pub(crate) async fn handle_attached_live_input_inner_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let mut active_emit_cache = None;
        self.handle_attached_live_input_inner_cached(
            identity,
            pending_input,
            bytes,
            &mut active_emit_cache,
        )
        .await
    }

    /// Flushes retained input that starts with the ambiguous `ESC ]` or
    /// `ESC _` bytes once the escape deadline fires. Reaching the deadline
    /// resolves the ambiguity between a fragmented consumed terminal
    /// response (OSC 4/10/11/12/52), a kitty graphics APC (`ESC _G`), and
    /// keyboard input in favor of the keyboard: dispatch M-] / M-_ through
    /// the normal key path (key tables, copy mode, per-pane key encoding)
    /// and reroute any accumulated body bytes as ordinary input. Writing the
    /// raw bytes to the pane instead would start an unterminated OSC/APC
    /// inside the pane's own parser and swallow subsequent input there.
    pub(super) async fn flush_attached_consumed_osc_prefix(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<bool> {
        let bytes = std::mem::take(pending_input);
        let backspace = self.attached_backspace_byte().await;
        let AttachedKeyDecode::Matched { size, key } = decode_live_attached_key(&bytes, backspace)
        else {
            // `ESC ]` always decodes as M-]; keep the raw forward as a
            // defensive fallback rather than dropping the input.
            self.write_attached_bytes_for_identity(identity, &bytes)
                .await?;
            return Ok(true);
        };
        let handled = self.handle_attached_live_key(identity, key).await?;
        let mut forwarded = !handled;
        if let Some(remaining) = bytes.get(size..).filter(|rest| !rest.is_empty()) {
            forwarded |= self
                .handle_attached_live_input_inner_for_identity(identity, pending_input, remaining)
                .await?;
        }
        Ok(forwarded)
    }

    async fn handle_attached_live_input_inner_cached(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        self.handle_attached_live_input_inner_with_windows_console_key(
            identity,
            pending_input,
            bytes,
            None,
            active_emit_cache,
        )
        .await
    }

    async fn handle_attached_live_input_inner_with_windows_console_key(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        on_attached_input_path(
            identity.attach_pid(),
            Box::pin(self.handle_arrived_frame(identity, async move {
                let mut cursor = match self
                    .defer_behind_released_work(
                        identity,
                        pending_input,
                        bytes,
                        windows_console_key,
                        active_emit_cache,
                    )
                    .await?
                {
                    Deferral::Waited(forwarded) => return Ok(forwarded),
                    Deferral::HandleNow(cursor) => cursor,
                };
                self.handle_attached_live_input_on_input_path(
                    identity,
                    pending_input,
                    bytes,
                    windows_console_key,
                    &mut cursor,
                    active_emit_cache,
                )
                .await
            })),
        )
        .await
    }

    /// Replays one input frame of the client that was queued behind its released bindings.
    pub(in crate::handler) async fn replay_queued_attached_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
    ) -> io::Result<bool> {
        let mut active_emit_cache = None;
        self.handle_attached_live_input_inner_with_windows_console_key(
            identity,
            pending_input,
            bytes,
            windows_console_key,
            &mut active_emit_cache,
        )
        .await
    }

    /// Replays one decoded keyboard event of the client that was queued behind its released
    /// bindings.
    pub(in crate::handler) async fn replay_queued_keyboard_event(
        &self,
        identity: ActiveAttachIdentity,
        event: Event,
    ) -> io::Result<bool> {
        let mut active_emit_cache = None;
        on_attached_input_path(
            identity.attach_pid(),
            Box::pin(self.handle_acquired_keyboard_event(
                identity,
                event,
                None,
                &mut active_emit_cache,
            )),
        )
        .await
    }

    /// Leaves a live frame waiting behind the client's released bindings when its ordinary input
    /// has to, acting on the focus reports among it now.
    #[cfg(test)]
    pub(crate) async fn handle_attached_live_input_for_test(
        &self,
        attach_pid: u32,
        bytes: &[u8],
    ) -> io::Result<()> {
        let mut pending_input = Vec::new();
        self.handle_attached_live_input(attach_pid, &mut pending_input, bytes)
            .await
    }
}

#[cfg(test)]
mod live_key_decode_tests {
    use rmux_core::{key_code_lookup_bits, key_string_lookup_string, KEYC_IMPLIED_META, KEYC_META};

    use super::{decode_live_attached_key, AttachedKeyDecode};

    #[test]
    fn meta_unicode_decodes_as_one_key_with_implied_meta() {
        let AttachedKeyDecode::Matched { size, key } =
            decode_live_attached_key(b"\x1b\xc3\xa9", None)
        else {
            panic!("Meta-é should decode as one attached key");
        };
        assert_eq!(size, 3);
        assert_eq!(
            key_code_lookup_bits(key),
            key_code_lookup_bits(key_string_lookup_string("M-é").expect("Meta-é parses"))
        );
        assert_ne!(key & KEYC_META, 0);
        assert_ne!(key & KEYC_IMPLIED_META, 0);
    }

    #[test]
    fn meta_unicode_waits_for_every_utf8_byte() {
        for partial in [
            b"\x1b\xc3".as_slice(),
            b"\x1b\xe6".as_slice(),
            b"\x1b\xe6\x97".as_slice(),
            b"\x1b\xf0".as_slice(),
            b"\x1b\xf0\x9f".as_slice(),
            b"\x1b\xf0\x9f\x92".as_slice(),
        ] {
            assert_eq!(
                decode_live_attached_key(partial, None),
                AttachedKeyDecode::Partial,
                "{partial:?} must stay pending until its declared UTF-8 length"
            );
        }
        assert!(matches!(
            decode_live_attached_key(b"\x1b\xc3\xa9", None),
            AttachedKeyDecode::Matched { size: 3, .. }
        ));
        assert!(matches!(
            decode_live_attached_key(b"\x1b\xe6\x97\xa5", None),
            AttachedKeyDecode::Matched { size: 4, .. }
        ));
        assert!(matches!(
            decode_live_attached_key(b"\x1b\xf0\x9f\x92\xa1", None),
            AttachedKeyDecode::Matched { size: 5, .. }
        ));
    }
}
