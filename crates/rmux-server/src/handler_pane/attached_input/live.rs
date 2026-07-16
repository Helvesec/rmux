use std::borrow::Cow;
use std::collections::VecDeque;
use std::io;
use std::sync::{atomic::Ordering, Arc};

use rmux_core::{input::mode, key_code_lookup_bits, KeyCode, LifecycleEvent, KEYC_ANY};
#[cfg(windows)]
use rmux_core::{key_string_lookup_string, KEYC_CTRL, KEYC_IMPLIED_META, KEYC_META, KEYC_SHIFT};
use rmux_proto::{AttachedKeystroke, PaneTarget, Response, WindowTarget};
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
    is_enter_key, is_mouse_prefix, resolve_input_target, retain_partial_attached_control_input,
    retain_partial_attached_escape_input,
};
use crate::client_flags::ClientFlags;
use crate::handler::overlay_support::AttachedOverlayInput;
use crate::handler::{attach_support::ActiveAttachIdentity, prepare_lifecycle_event};
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{
    decode_attached_key, default_key_table_name, lookup_attached_key_table_binding,
    matches_prefix_key, session_option_key, AttachedKeyDecode, PREFIX_TABLE,
};

pub(crate) type ActiveClientEmitCache = Option<(u64, WindowTarget)>;

// Rerouting must re-evaluate attach state after hooks, mouse bindings, and prefix
// commands. Keep each reroute suffix bounded so one maximum-size local frame
// cannot turn those necessary copies into quadratic work.
const ATTACHED_LIVE_REROUTE_CHUNK_BYTES: usize = 1024;

enum AttachedLiveInputStep {
    Complete(bool),
    Reroute { bytes: Vec<u8>, forwarded: bool },
}

enum AttachedLiveChunkResult {
    Complete(bool),
    Rechunk { bytes: Vec<u8>, forwarded: bool },
}

struct AttachedLiveInputWork {
    bytes: Arc<[u8]>,
    start: usize,
    end: usize,
    windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
}

struct PlainKeyDispatchSnapshot {
    prefix: Option<KeyCode>,
    prefix2: Option<KeyCode>,
    bindings: Vec<KeyCode>,
}

impl PlainKeyDispatchSnapshot {
    fn new(state: &crate::pane_terminals::HandlerState, target: &PaneTarget) -> Self {
        let table_name = default_key_table_name(state, target);
        let bindings = state
            .key_bindings
            .table(&table_name)
            .into_iter()
            .flat_map(|table| table.active().keys())
            .map(|key| key_code_lookup_bits(*key))
            .filter(|key| *key == KEYC_ANY || *key <= u64::from(char::MAX as u32))
            .collect();
        Self {
            prefix: session_option_key(
                state,
                target.session_name(),
                rmux_proto::OptionName::Prefix,
            ),
            prefix2: session_option_key(
                state,
                target.session_name(),
                rmux_proto::OptionName::Prefix2,
            ),
            bindings,
        }
    }

    fn requires_dispatch(&self, key: KeyCode) -> bool {
        let key = key_code_lookup_bits(key);
        matches_prefix_key(key, self.prefix, self.prefix2)
            || self
                .bindings
                .iter()
                .any(|binding| *binding == KEYC_ANY || *binding == key)
    }
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
        if bytes.len() <= ATTACHED_LIVE_REROUTE_CHUNK_BYTES {
            return match self
                .handle_attached_live_input_chunk(
                    identity,
                    pending_input,
                    bytes,
                    windows_console_key,
                    active_emit_cache,
                )
                .await?
            {
                AttachedLiveChunkResult::Complete(forwarded) => Ok(forwarded),
                AttachedLiveChunkResult::Rechunk { bytes, forwarded } => {
                    self.handle_attached_live_input_work_queue(
                        identity,
                        pending_input,
                        AttachedLiveInputWork::new(Arc::from(bytes), None),
                        forwarded,
                        active_emit_cache,
                    )
                    .await
                }
            };
        }

        // Preserve one-shot fast-path semantics for a large printable line,
        // including submitted-line tracking. Inputs that contain a prefix or
        // terminal control fall back to bounded reroute chunks below.
        if windows_console_key.is_none() {
            if let Some(forwarded) = self
                .try_forward_plain_attached_bytes_fast(
                    identity,
                    pending_input,
                    bytes,
                    active_emit_cache,
                )
                .await?
            {
                return Ok(forwarded);
            }
        }

        self.handle_attached_live_input_work_queue(
            identity,
            pending_input,
            AttachedLiveInputWork::new(Arc::from(bytes), windows_console_key),
            false,
            active_emit_cache,
        )
        .await
    }

    async fn handle_attached_live_input_work_queue(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        initial_work: AttachedLiveInputWork,
        mut forwarded: bool,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<bool> {
        let mut work_queue = VecDeque::from([initial_work]);
        while let Some(mut work) = work_queue.pop_front() {
            if work.len() > ATTACHED_LIVE_REROUTE_CHUNK_BYTES
                && self
                    .attached_live_input_can_use_reroute_chunks(identity)
                    .await?
            {
                let remaining = work.split_at(ATTACHED_LIVE_REROUTE_CHUNK_BYTES);
                work_queue.push_front(remaining);
            }

            let windows_console_key = work.windows_console_key.take();
            match self
                .handle_attached_live_input_chunk(
                    identity,
                    pending_input,
                    work.as_bytes(),
                    windows_console_key,
                    active_emit_cache,
                )
                .await?
            {
                AttachedLiveChunkResult::Complete(step_forwarded) => {
                    forwarded |= step_forwarded;
                }
                AttachedLiveChunkResult::Rechunk {
                    bytes,
                    forwarded: step_forwarded,
                } => {
                    forwarded |= step_forwarded;
                    work_queue.push_front(AttachedLiveInputWork::new(Arc::from(bytes), None));
                }
            }
        }
        Ok(forwarded)
    }

    async fn handle_attached_live_input_chunk(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        mut windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<AttachedLiveChunkResult> {
        let mut bytes = Cow::Borrowed(bytes);
        let mut forwarded = false;

        loop {
            match self
                .handle_attached_live_input_step(
                    identity,
                    pending_input,
                    bytes.as_ref(),
                    windows_console_key.take(),
                    active_emit_cache,
                )
                .await?
            {
                AttachedLiveInputStep::Complete(step_forwarded) => {
                    return Ok(AttachedLiveChunkResult::Complete(
                        forwarded | step_forwarded,
                    ));
                }
                AttachedLiveInputStep::Reroute {
                    bytes: remaining,
                    forwarded: step_forwarded,
                } => {
                    forwarded |= step_forwarded;
                    if remaining.len() > ATTACHED_LIVE_REROUTE_CHUNK_BYTES
                        && self
                            .attached_live_input_can_use_reroute_chunks(identity)
                            .await?
                    {
                        return Ok(AttachedLiveChunkResult::Rechunk {
                            bytes: remaining,
                            forwarded,
                        });
                    }
                    bytes = Cow::Owned(remaining);
                }
            }
        }
    }

    async fn handle_attached_live_input_step(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<AttachedLiveInputStep> {
        #[cfg(not(windows))]
        let _ = windows_console_key;
        #[cfg(windows)]
        let windows_console_key = windows_console_key
            .filter(|_| pending_input.is_empty() && !bytes.is_empty())
            .map(windows_console_key_event);
        let mut forwarded_to_pane = false;
        #[cfg(windows)]
        let try_plain_fast_path = windows_console_key.is_none();
        #[cfg(not(windows))]
        let try_plain_fast_path = true;
        if try_plain_fast_path {
            if let Some(forwarded) = self
                .try_forward_plain_attached_bytes_fast(
                    identity,
                    pending_input,
                    bytes,
                    active_emit_cache,
                )
                .await?
            {
                return Ok(AttachedLiveInputStep::Complete(forwarded));
            }
        }
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            pending_input.clear();
            return Ok(AttachedLiveInputStep::Complete(false));
        }
        self.clear_attached_focus_alerts(identity).await;
        if self
            .attached_modal_surface_active_for_identity(identity)
            .await
        {
            if let Some(input) = self.split_attached_modal_palette_input(pending_input, bytes) {
                pending_input.clear();
                return self
                    .handle_attached_modal_palette_input(identity, pending_input, input)
                    .await;
            }
        }
        if let Some(step) = self
            .handle_attached_modal_input_step(identity, pending_input, bytes)
            .await?
        {
            return Ok(step);
        }
        let target = self
            .attached_input_target_for_identity(identity)
            .await
            .map_err(io_other)?;
        self.emit_attached_client_active_if_changed(identity, &target, active_emit_cache)
            .await;
        if self
            .target_is_in_clock_mode(&target)
            .await
            .map_err(io_other)?
        {
            let new_input_at = pending_input.len();
            pending_input.extend_from_slice(bytes);
            match decode_bracketed_paste_after_append(pending_input, new_input_at) {
                BracketedPasteDecode::Matched { .. } => {
                    let _ = self.exit_clock_mode(&target).await.map_err(io_other)?;
                    return Ok(AttachedLiveInputStep::Reroute {
                        bytes: std::mem::take(pending_input),
                        forwarded: false,
                    });
                }
                BracketedPasteDecode::Partial => {
                    if pending_input.len() > MAX_RETAINED_ATTACHED_CONTROL_INPUT {
                        let _ = self.exit_clock_mode(&target).await.map_err(io_other)?;
                        return Ok(AttachedLiveInputStep::Reroute {
                            bytes: std::mem::take(pending_input),
                            forwarded: false,
                        });
                    }
                    retain_partial_attached_control_input(
                        "clock mode bracketed paste",
                        pending_input,
                    )?;
                    return Ok(AttachedLiveInputStep::Complete(false));
                }
                BracketedPasteDecode::NotPaste => {}
            }
            if is_mouse_prefix(pending_input) {
                let last_mouse = self.attached_last_mouse_event_for_identity(identity).await;
                let consumed = match decode_mouse(pending_input, last_mouse) {
                    MouseDecode::Matched { size, .. } | MouseDecode::Discard { size } => size,
                    MouseDecode::Partial | MouseDecode::Overlong => {
                        retain_partial_attached_escape_input(
                            "clock mode mouse input",
                            pending_input,
                        )?;
                        return Ok(AttachedLiveInputStep::Complete(false));
                    }
                    MouseDecode::Invalid => 0,
                };
                if consumed > 0 {
                    pending_input.drain(..consumed);
                    let _ = self.exit_clock_mode(&target).await.map_err(io_other)?;
                    let remaining =
                        (!pending_input.is_empty()).then(|| std::mem::take(pending_input));
                    return Ok(attached_mode_input_step(remaining));
                }
            }
            let backspace = self.attached_backspace_byte().await;
            let consumed = match decode_attached_key(pending_input, backspace) {
                AttachedKeyDecode::Matched { size, .. } => size,
                AttachedKeyDecode::Partial => {
                    retain_partial_attached_escape_input("clock mode input", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(false));
                }
                AttachedKeyDecode::Invalid => {
                    let Some((_, consumed)) = decode_prompt_input_event(pending_input) else {
                        retain_partial_attached_escape_input("clock mode input", pending_input)?;
                        return Ok(AttachedLiveInputStep::Complete(false));
                    };
                    consumed
                }
            };
            pending_input.drain(..consumed);
            let _ = self.exit_clock_mode(&target).await.map_err(io_other)?;
            let remaining = (!pending_input.is_empty()).then(|| std::mem::take(pending_input));
            return Ok(attached_mode_input_step(remaining));
        }
        let target_in_copy_mode = self
            .target_is_in_copy_mode(&target)
            .await
            .map_err(io_other)?;
        let target_mode = self.target_pane_mode(&target).await.map_err(io_other)?;
        let target_focus_events = target_mode & mode::MODE_FOCUSON != 0;
        let backspace = self.attached_backspace_byte().await;

        #[cfg(windows)]
        if pending_input.is_empty() && bytes == b"\x04" {
            if let Some(key) = windows_key_code_named("C-d") {
                let handled = self
                    .handle_attached_live_key_inner(
                        identity,
                        key,
                        super::AttachedPaneForward::WindowsConsoleKey {
                            key: WindowsConsoleKeyEvent::ctrl_d(),
                            bytes,
                        },
                    )
                    .await?;
                return Ok(AttachedLiveInputStep::Complete(!handled));
            }
        }

        #[cfg(windows)]
        if let Some(key_event) = windows_console_key.filter(|_| pending_input.is_empty()) {
            if let AttachedKeyDecode::Matched { size, key } = decode_attached_key(bytes, backspace)
            {
                if size == bytes.len() {
                    if let Some(key) = windows_console_binding_override_key(key, key_event) {
                        let handled = self
                            .handle_attached_live_key_inner(
                                identity,
                                key,
                                super::AttachedPaneForward::WindowsConsoleKey {
                                    key: key_event,
                                    bytes,
                                },
                            )
                            .await?;
                        return Ok(AttachedLiveInputStep::Complete(!handled));
                    }
                }
            }
        }

        if pending_input.is_empty()
            && !self.attached_key_table_active(identity).await
            && self
                .dispatch_immediate_prefix_detach(identity, &target, bytes, backspace)
                .await?
        {
            return Ok(AttachedLiveInputStep::Complete(false));
        }

        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        let mut raw_start = 0;
        let mut offset = 0;
        let mut plain_key_dispatch = None;

        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            let slice_new_input_at = new_input_at.saturating_sub(offset);
            match decode_bracketed_paste_after_append(slice, slice_new_input_at) {
                BracketedPasteDecode::Matched {
                    size,
                    body_start,
                    body_end,
                } => {
                    if target_in_copy_mode {
                        offset += size;
                        raw_start = offset;
                        continue;
                    }
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_bracketed_paste_for_identity(
                        identity,
                        &pending_input[offset + body_start..offset + body_end],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                BracketedPasteDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    if let Some(segment) = take_incomplete_bracketed_paste_segment(
                        pending_input,
                        MAX_RETAINED_ATTACHED_CONTROL_INPUT,
                    ) {
                        if !target_in_copy_mode {
                            self.write_attached_bracketed_paste_for_identity(identity, &segment)
                                .await?;
                            forwarded_to_pane = true;
                        }
                    }
                    retain_partial_attached_control_input("live bracketed paste", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                BracketedPasteDecode::NotPaste => {}
            }
            match decode_kitty_graphics_apc_after_append(slice, slice_new_input_at) {
                KittyGraphicsApcDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + size],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                KittyGraphicsApcDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input(
                        "live kitty graphics APC",
                        pending_input,
                    )?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                KittyGraphicsApcDecode::NotKittyGraphics => {}
            }
            if let Some(event) = decode_focus_event(slice) {
                if raw_start < offset {
                    self.write_attached_bytes_for_identity(
                        identity,
                        &pending_input[raw_start..offset],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                if target_focus_events {
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + 3],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                self.handle_attached_terminal_control_event(identity, &target, event)
                    .await;
                offset += 3;
                raw_start = offset;
                if let Some(remaining) = take_attached_remaining_input(pending_input, raw_start) {
                    return Ok(AttachedLiveInputStep::Reroute {
                        bytes: remaining,
                        forwarded: forwarded_to_pane,
                    });
                }
                continue;
            }
            match decode_attached_terminal_control_after_append(
                slice,
                target_focus_events,
                slice_new_input_at,
            ) {
                TerminalResponseDecode::Matched { size, event } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    if let Some(event) = event {
                        self.handle_attached_terminal_control_event(identity, &target, event)
                            .await;
                    }
                    offset += size;
                    raw_start = offset;
                    if event.is_some() {
                        if let Some(remaining) =
                            take_attached_remaining_input(pending_input, raw_start)
                        {
                            return Ok(AttachedLiveInputStep::Reroute {
                                bytes: remaining,
                                forwarded: forwarded_to_pane,
                            });
                        }
                    }
                    continue;
                }
                TerminalResponseDecode::PaneBound { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + size],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                TerminalResponseDecode::PaletteResponse { size, index } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    forwarded_to_pane |= self
                        .write_attached_palette_response_for_identity(
                            identity,
                            index,
                            &pending_input[offset..offset + size],
                        )
                        .await?;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                TerminalResponseDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input("live terminal response", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                TerminalResponseDecode::NotResponse => {}
            }
            match decode_pane_bound_terminal_string(slice) {
                PaneBoundTerminalStringDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    self.write_attached_target_bytes_for_identity(
                        identity,
                        &pending_input[offset..offset + size],
                    )
                    .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                PaneBoundTerminalStringDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input(
                        "live pane-bound terminal string",
                        pending_input,
                    )?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                PaneBoundTerminalStringDecode::NotString => {}
            }
            if is_mouse_prefix(slice) {
                if raw_start < offset {
                    self.write_attached_bytes_for_identity(
                        identity,
                        &pending_input[raw_start..offset],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                let last_mouse = self.attached_last_mouse_event_for_identity(identity).await;
                match decode_mouse(slice, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        self.handle_attached_live_mouse(identity, event).await?;
                        offset += size;
                        raw_start = offset;
                        if let Some(remaining) =
                            take_attached_remaining_input(pending_input, raw_start)
                        {
                            return Ok(AttachedLiveInputStep::Reroute {
                                bytes: remaining,
                                forwarded: forwarded_to_pane,
                            });
                        }
                    }
                    MouseDecode::Discard { size } => {
                        offset += size;
                        raw_start = offset;
                    }
                    MouseDecode::Partial | MouseDecode::Overlong => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input("live mouse", pending_input)?;
                        return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                    }
                    MouseDecode::Invalid => {
                        raw_start = offset;
                        offset += 1;
                    }
                }
                continue;
            }
            if is_extended_key_prefix(slice) {
                if raw_start < offset {
                    self.write_attached_bytes_for_identity(
                        identity,
                        &pending_input[raw_start..offset],
                    )
                    .await?;
                    forwarded_to_pane = true;
                }
                match decode_extended_key(slice, backspace) {
                    ExtendedKeyDecode::Matched { size, key } => {
                        if raw_start < offset && is_enter_key(key) {
                            self.record_attached_submitted_text(
                                identity,
                                &pending_input[raw_start..offset],
                            )
                            .await?;
                        }
                        #[cfg(windows)]
                        let handled = if let Some(key_event) = windows_console_key
                            .filter(|_| {
                                raw_start == offset
                                    && offset == 0
                                    && size == pending_input.len()
                                    && size == bytes.len()
                            })
                            .or_else(|| windows_synthetic_console_key_for_decoded_key(key))
                        {
                            let key = windows_console_binding_key(key, key_event);
                            self.handle_attached_live_key_inner(
                                identity,
                                key,
                                super::AttachedPaneForward::WindowsConsoleKey {
                                    key: key_event,
                                    bytes: &pending_input[offset..offset + size],
                                },
                            )
                            .await?
                        } else {
                            self.handle_attached_live_key(identity, key).await?
                        };
                        #[cfg(not(windows))]
                        let handled = self.handle_attached_live_key(identity, key).await?;
                        if !handled {
                            forwarded_to_pane = true;
                        }
                        offset += size;
                        raw_start = offset;
                        if handled {
                            if let Some(remaining) =
                                take_attached_remaining_input(pending_input, raw_start)
                            {
                                return Ok(AttachedLiveInputStep::Reroute {
                                    bytes: remaining,
                                    forwarded: forwarded_to_pane,
                                });
                            }
                        }
                        if self.prompt_active_for_identity(identity).await {
                            break;
                        }
                        continue;
                    }
                    ExtendedKeyDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input("live extended key", pending_input)?;
                        return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                    }
                    ExtendedKeyDecode::Invalid => {
                        raw_start = offset;
                    }
                }
            }
            let key_table_active = self.attached_key_table_active(identity).await;
            if !key_table_active && !target_in_copy_mode {
                let first = slice[0];
                if !first.is_ascii_control() {
                    let (key, size) = if first.is_ascii() {
                        (KeyCode::from(first), 1)
                    } else if let Some((character, size)) = decode_utf8_char(slice) {
                        (KeyCode::from(character), size)
                    } else {
                        if is_utf8_lead_byte(first) && slice.len() < utf8_expected_len(first) {
                            if raw_start < offset {
                                self.write_attached_bytes_for_identity(
                                    identity,
                                    &pending_input[raw_start..offset],
                                )
                                .await?;
                                forwarded_to_pane = true;
                            }
                            pending_input.drain(..offset);
                            retain_partial_attached_escape_input("live utf-8", pending_input)?;
                            return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                        }
                        offset += 1;
                        continue;
                    };
                    if plain_key_dispatch.is_none() {
                        let state = self.state.lock().await;
                        plain_key_dispatch = Some(PlainKeyDispatchSnapshot::new(&state, &target));
                    }
                    if !plain_key_dispatch
                        .as_ref()
                        .expect("plain key dispatch snapshot must be initialized")
                        .requires_dispatch(key)
                    {
                        offset += size;
                        continue;
                    }
                }
            }
            match decode_live_attached_key(slice, backspace) {
                AttachedKeyDecode::Matched { size, key } => {
                    if raw_start < offset && is_enter_key(key) {
                        self.record_attached_submitted_text(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    #[cfg(windows)]
                    let handled = if let Some(key_event) = windows_console_key
                        .filter(|_| {
                            raw_start == offset
                                && offset == 0
                                && size == pending_input.len()
                                && size == bytes.len()
                        })
                        .or_else(|| windows_synthetic_console_key_for_decoded_key(key))
                    {
                        let key = windows_console_binding_key(key, key_event);
                        self.handle_attached_live_key_inner(
                            identity,
                            key,
                            super::AttachedPaneForward::WindowsConsoleKey {
                                key: key_event,
                                bytes: &pending_input[offset..offset + size],
                            },
                        )
                        .await?
                    } else {
                        self.handle_attached_live_key(identity, key).await?
                    };
                    #[cfg(not(windows))]
                    let handled = self.handle_attached_live_key(identity, key).await?;
                    if !handled {
                        forwarded_to_pane = true;
                    }
                    offset += size;
                    raw_start = offset;
                    if handled {
                        if let Some(remaining) =
                            take_attached_remaining_input(pending_input, raw_start)
                        {
                            return Ok(AttachedLiveInputStep::Reroute {
                                bytes: remaining,
                                forwarded: forwarded_to_pane,
                            });
                        }
                    }
                    if self.prompt_active_for_identity(identity).await {
                        break;
                    }
                    continue;
                }
                AttachedKeyDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes_for_identity(
                            identity,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_escape_input("live attached key", pending_input)?;
                    return Ok(AttachedLiveInputStep::Complete(forwarded_to_pane));
                }
                AttachedKeyDecode::Invalid => {}
            }
            offset += 1;
        }

        if self.prompt_active_for_identity(identity).await && raw_start < pending_input.len() {
            let remaining = pending_input[raw_start..].to_vec();
            pending_input.clear();
            return Ok(AttachedLiveInputStep::Reroute {
                bytes: remaining,
                forwarded: forwarded_to_pane,
            });
        }

        if raw_start < pending_input.len() {
            self.write_attached_bytes_for_identity(identity, &pending_input[raw_start..])
                .await?;
            forwarded_to_pane = true;
        }
        pending_input.clear();
        Ok(AttachedLiveInputStep::Complete(forwarded_to_pane))
    }

    async fn handle_attached_modal_input_step(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<AttachedLiveInputStep>> {
        if self.prompt_active_for_identity(identity).await {
            let remaining = self
                .handle_attached_prompt_input(identity, pending_input, bytes)
                .await?;
            return Ok(Some(attached_mode_input_step(remaining)));
        }
        if self.mode_tree_active_for_identity(identity).await {
            let remaining = self
                .handle_attached_mode_tree_input(identity, pending_input, bytes)
                .await?;
            return Ok(Some(attached_mode_input_step(remaining)));
        }
        if self.overlay_active_for_identity(identity).await {
            return match self
                .handle_attached_overlay_input_for_identity(identity, pending_input, bytes)
                .await?
            {
                AttachedOverlayInput::Consumed => Ok(Some(AttachedLiveInputStep::Complete(false))),
                AttachedOverlayInput::Reroute(bytes) => Ok(Some(AttachedLiveInputStep::Reroute {
                    bytes,
                    forwarded: false,
                })),
            };
        }
        if self.display_panes_active_for_identity(identity).await {
            let remaining = self
                .handle_attached_display_panes_input_for_identity(identity, pending_input, bytes)
                .await?;
            return Ok(Some(attached_mode_input_step(remaining)));
        }
        Ok(None)
    }

    async fn handle_attached_modal_palette_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        input: ModalPaletteInput,
    ) -> io::Result<AttachedLiveInputStep> {
        let ModalPaletteInput { segments, retained } = input;
        let mut segments = VecDeque::from(segments);
        let mut modal_pending = Vec::new();
        let mut forwarded = false;

        while let Some(segment) = segments.pop_front() {
            let bytes = match segment {
                ModalPaletteInputSegment::Input(bytes) => bytes,
                ModalPaletteInputSegment::PaneBound(bytes) => {
                    self.write_attached_target_bytes_for_identity(identity, &bytes)
                        .await?;
                    forwarded = true;
                    continue;
                }
                ModalPaletteInputSegment::Response { index, bytes } => {
                    if !self
                        .attached_modal_surface_active_for_identity(identity)
                        .await
                    {
                        let mut rerouted = modal_pending;
                        rerouted.extend_from_slice(&bytes);
                        append_modal_palette_remainder(&mut rerouted, segments, &retained);
                        return Ok(AttachedLiveInputStep::Reroute {
                            bytes: rerouted,
                            forwarded,
                        });
                    }
                    if self
                        .write_attached_palette_response_for_identity(identity, index, &bytes)
                        .await?
                    {
                        forwarded = true;
                    }
                    // A syntactically valid OSC 4 response is terminal
                    // protocol input, never a modal keystroke. Matching
                    // responses reach the pane above; unsolicited, mismatched,
                    // or duplicate responses are discarded just as they are
                    // on the ordinary live-input path. Feeding a duplicate
                    // through a prompt or mode-tree would let its leading ESC
                    // close the surface after another attached client had
                    // already consumed the shared query slot.
                    continue;
                }
            };

            let Some(step) = self
                .handle_attached_modal_input_step(identity, &mut modal_pending, &bytes)
                .await?
            else {
                let mut rerouted = modal_pending;
                rerouted.extend_from_slice(&bytes);
                append_modal_palette_remainder(&mut rerouted, segments, &retained);
                return Ok(AttachedLiveInputStep::Reroute {
                    bytes: rerouted,
                    forwarded,
                });
            };
            match step {
                AttachedLiveInputStep::Complete(step_forwarded) => {
                    forwarded |= step_forwarded;
                }
                AttachedLiveInputStep::Reroute {
                    bytes,
                    forwarded: step_forwarded,
                } => {
                    let mut rerouted = modal_pending;
                    rerouted.extend_from_slice(&bytes);
                    append_modal_palette_remainder(&mut rerouted, segments, &retained);
                    return Ok(AttachedLiveInputStep::Reroute {
                        bytes: rerouted,
                        forwarded: forwarded | step_forwarded,
                    });
                }
            }
        }

        modal_pending.extend_from_slice(&retained);
        if modal_pending.is_empty() {
            return Ok(AttachedLiveInputStep::Complete(forwarded));
        }
        if self
            .attached_modal_surface_active_for_identity(identity)
            .await
        {
            *pending_input = modal_pending;
            retain_partial_attached_control_input("modal palette response", pending_input)?;
            Ok(AttachedLiveInputStep::Complete(forwarded))
        } else {
            Ok(AttachedLiveInputStep::Reroute {
                bytes: modal_pending,
                forwarded,
            })
        }
    }

    async fn try_forward_plain_attached_bytes_fast(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &[u8],
        bytes: &[u8],
        active_emit_cache: &mut ActiveClientEmitCache,
    ) -> io::Result<Option<bool>> {
        if !pending_input.is_empty() || !is_plain_attached_fast_path_input(bytes) {
            return Ok(None);
        }

        let Some((session_name, session_id)) = self.fast_path_attached_session(identity).await?
        else {
            return Ok(None);
        };
        let prepared = self
            .with_live_input_session_state(identity, &session_name, session_id, |state| {
                let target =
                    resolve_input_target(state, None, Some(&session_name)).map_err(io_other)?;
                let transcript = state.transcript_handle(&target).map_err(io_other)?;
                {
                    let transcript = transcript
                        .lock()
                        .expect("pane transcript mutex must not be poisoned");
                    if transcript.copy_mode_state().is_some()
                        || transcript.clock_mode_generation().is_some()
                        || transcript.mode() & mode::MODE_MOUSE_ALL != 0
                    {
                        return Ok(None);
                    }
                }
                if plain_input_requires_key_dispatch(bytes, &target, state) {
                    return Ok(None);
                }
                if let Some(submitted) = submitted_text_before_enter(bytes) {
                    state
                        .record_attached_submitted_text(&target, submitted)
                        .map_err(io_other)?;
                }
                let clear_alerts_changed =
                    state
                        .sessions
                        .session_mut(&session_name)
                        .is_some_and(|session| {
                            session.clear_all_winlink_alert_flags(target.window_index())
                        });
                let writes =
                    prepare_attached_pane_input_writes(state, &target, bytes).map_err(io_other)?;
                Ok(Some((target, writes, clear_alerts_changed)))
            })
            .await?
            .flatten();
        let Some((target, writes, clear_alerts_changed)) = prepared else {
            return Ok(None);
        };

        self.emit_attached_client_active_if_changed(identity, &target, active_emit_cache)
            .await;
        for write in writes {
            write_attached_bytes_to_target_io(write, bytes.to_vec())
                .await
                .map_err(io_other)?;
        }
        if clear_alerts_changed {
            let handler = self.clone();
            let refresh_session_name = session_name.clone();
            tokio::spawn(async move {
                handler
                    .refresh_attached_session_for_session_identity(
                        &refresh_session_name,
                        session_id,
                    )
                    .await;
            });
        }
        Ok(Some(true))
    }

    async fn fast_path_attached_session(
        &self,
        identity: ActiveAttachIdentity,
    ) -> io::Result<Option<(rmux_proto::SessionName, rmux_proto::SessionId)>> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| {
                io_other(rmux_proto::RmuxError::Server(
                    "attached client disappeared".to_owned(),
                ))
            })?;
        if !active.can_write || active.flags.contains(ClientFlags::READONLY) {
            return Ok(None);
        }
        if active.prompt.is_some()
            || active.mode_tree.is_some()
            || active.overlay.is_some()
            || active.display_panes.is_some()
            || active.key_table_name.is_some()
        {
            return Ok(None);
        }
        Ok(Some((active.session_name.clone(), active.session_id)))
    }

    async fn attached_live_input_can_use_reroute_chunks(
        &self,
        identity: ActiveAttachIdentity,
    ) -> io::Result<bool> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| {
                io_other(rmux_proto::RmuxError::Server(
                    "attached client disappeared".to_owned(),
                ))
            })?;
        Ok(active.can_write
            && !active.flags.contains(ClientFlags::READONLY)
            && active.prompt.is_none()
            && active.mode_tree.is_none()
            && active.overlay.is_none()
            && active.display_panes.is_none())
    }

    async fn attached_modal_surface_active_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> bool {
        let active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        else {
            return false;
        };
        active.prompt.is_some()
            || active.mode_tree.is_some()
            || active.overlay.is_some()
            || active.display_panes.is_some()
    }

    async fn attached_key_table_active(&self, identity: ActiveAttachIdentity) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.key_table_name.is_some()
            })
    }

    async fn emit_attached_client_active_if_changed(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        active_emit_cache: &mut ActiveClientEmitCache,
    ) {
        let attach_pid = identity.attach_pid();
        let window_target =
            WindowTarget::with_window(target.session_name().clone(), target.window_index());
        let epoch = self.active_attach_epoch.load(Ordering::Acquire);
        if active_emit_cache
            .as_ref()
            .is_some_and(|(cached_epoch, cached)| {
                *cached_epoch == epoch && cached == &window_target
            })
        {
            return;
        }

        let (should_emit, cache_epoch) = {
            let mut active_attach = self.active_attach.lock().await;
            if !active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            }) {
                return;
            }
            let changed = active_attach.record_active_client_for_window(attach_pid, target);
            let cache_epoch = if changed {
                self.active_attach_epoch
                    .fetch_add(1, Ordering::AcqRel)
                    .saturating_add(1)
            } else {
                self.active_attach_epoch.load(Ordering::Acquire)
            };
            (changed, cache_epoch)
        };
        *active_emit_cache = Some((cache_epoch, window_target));
        if should_emit {
            self.emit(LifecycleEvent::ClientActive {
                session_name: target.session_name().clone(),
                client_name: Some(attach_pid.to_string()),
            })
            .await;
        }
    }

    async fn handle_attached_terminal_control_event(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        event: TerminalControlEvent,
    ) {
        let Ok((session_name, session_id)) =
            self.attached_session_identity_for_identity(identity).await
        else {
            return;
        };
        if target.session_name() != &session_name {
            return;
        }
        let attach_pid = identity.attach_pid();
        let session_name = target.session_name().clone();
        let client_name = Some(attach_pid.to_string());
        let events = match event {
            TerminalControlEvent::FocusIn => {
                vec![
                    LifecycleEvent::ClientFocusIn {
                        session_name,
                        client_name,
                    },
                    LifecycleEvent::PaneFocusIn {
                        target: target.clone(),
                    },
                ]
            }
            TerminalControlEvent::FocusOut => {
                vec![
                    LifecycleEvent::PaneFocusOut {
                        target: target.clone(),
                    },
                    LifecycleEvent::ClientFocusOut {
                        session_name,
                        client_name,
                    },
                ]
            }
            TerminalControlEvent::ClientLightTheme => {
                vec![LifecycleEvent::ClientLightTheme {
                    session_name,
                    client_name,
                }]
            }
            TerminalControlEvent::ClientDarkTheme => {
                vec![LifecycleEvent::ClientDarkTheme {
                    session_name,
                    client_name,
                }]
            }
        };
        let prepared = {
            let mut state = self.state.lock().await;
            if super::ensure_target_session_identity(&state, target, session_id).is_err() {
                return;
            }
            events
                .iter()
                .map(|event| prepare_lifecycle_event(&mut state, event))
                .collect::<Vec<_>>()
        };
        for event in prepared {
            self.emit_prepared_and_wait(event).await;
        }
    }

    async fn clear_attached_focus_alerts(&self, identity: ActiveAttachIdentity) {
        let focused_window = {
            let session_name = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&identity.attach_pid())
                    .filter(|active| {
                        identity.matches_active(active)
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|active| active.session_name.clone())
            };
            match session_name {
                Some(session_name) => {
                    let window_index = {
                        let state = self.state.lock().await;
                        state
                            .sessions
                            .session(&session_name)
                            .map(rmux_core::Session::active_window_index)
                    };
                    window_index.map(|window_index| (session_name, window_index))
                }
                None => None,
            }
        };
        if let Some((session_name, window_index)) = focused_window {
            let _ = self
                .clear_session_alerts_on_focus(&session_name, window_index)
                .await;
        }
    }

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

    async fn dispatch_immediate_prefix_detach(
        &self,
        identity: ActiveAttachIdentity,
        target: &rmux_proto::PaneTarget,
        bytes: &[u8],
        backspace: Option<u8>,
    ) -> io::Result<bool> {
        let AttachedKeyDecode::Matched {
            size: prefix_size,
            key: prefix_key,
        } = decode_live_attached_key(bytes, backspace)
        else {
            return Ok(false);
        };
        if prefix_size == 0 || prefix_size >= bytes.len() {
            return Ok(false);
        }

        let AttachedKeyDecode::Matched {
            size: command_size,
            key: command_key,
        } = decode_live_attached_key(&bytes[prefix_size..], backspace)
        else {
            return Ok(false);
        };
        if prefix_size.saturating_add(command_size) != bytes.len() {
            return Ok(false);
        }

        let is_bare_detach_binding = {
            let state = self.state.lock().await;
            let prefix = session_option_key(
                &state,
                target.session_name(),
                rmux_proto::OptionName::Prefix,
            );
            let prefix2 = session_option_key(
                &state,
                target.session_name(),
                rmux_proto::OptionName::Prefix2,
            );
            if !matches_prefix_key(prefix_key, prefix, prefix2) {
                return Ok(false);
            }
            lookup_attached_key_table_binding(
                &state,
                PREFIX_TABLE,
                key_code_lookup_bits(command_key),
            )
            .is_some_and(|binding| {
                let commands = binding.commands().commands();
                commands.len() == 1
                    && commands[0].name() == "detach-client"
                    && commands[0].arguments().is_empty()
            })
        };
        if !is_bare_detach_binding {
            return Ok(false);
        }

        if !self.current_live_attach_input(identity).await {
            return Ok(false);
        }
        match self.handle_detach_client_for_identity(identity).await {
            Response::Error(error) => Err(io_other(error.error)),
            _ => Ok(true),
        }
    }
}

fn attached_mode_input_step(remaining: Option<Vec<u8>>) -> AttachedLiveInputStep {
    match remaining {
        Some(bytes) => AttachedLiveInputStep::Reroute {
            bytes,
            forwarded: false,
        },
        None => AttachedLiveInputStep::Complete(false),
    }
}

fn append_modal_palette_remainder(
    bytes: &mut Vec<u8>,
    mut segments: VecDeque<ModalPaletteInputSegment>,
    retained: &[u8],
) {
    while let Some(segment) = segments.pop_front() {
        bytes.extend_from_slice(&segment.into_bytes());
    }
    bytes.extend_from_slice(retained);
}

fn is_plain_attached_fast_path_input(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|byte| matches!(*byte, b'\r' | b'\n' | b' '..=b'~'))
}

fn decode_live_attached_key(input: &[u8], backspace: Option<u8>) -> AttachedKeyDecode {
    match decode_attached_key(input, backspace) {
        AttachedKeyDecode::Invalid => {
            let Some(&first) = input.first() else {
                return AttachedKeyDecode::Partial;
            };
            let (utf8, consumed_prefix, modifiers) = if first == b'\x1b' {
                let Some(&second) = input.get(1) else {
                    return AttachedKeyDecode::Partial;
                };
                if second.is_ascii() {
                    return AttachedKeyDecode::Invalid;
                }
                (
                    &input[1..],
                    1,
                    rmux_core::KEYC_META | rmux_core::KEYC_IMPLIED_META,
                )
            } else {
                (input, 0, 0)
            };
            let Some(&utf8_first) = utf8.first() else {
                return AttachedKeyDecode::Partial;
            };
            if let Some((character, size)) = decode_utf8_char(utf8) {
                return AttachedKeyDecode::Matched {
                    size: consumed_prefix + size,
                    key: character as rmux_core::KeyCode | modifiers,
                };
            }
            if is_utf8_lead_byte(utf8_first) && utf8.len() < utf8_expected_len(utf8_first) {
                AttachedKeyDecode::Partial
            } else {
                AttachedKeyDecode::Invalid
            }
        }
        decoded => decoded,
    }
}

fn plain_input_requires_key_dispatch(
    bytes: &[u8],
    target: &PaneTarget,
    state: &crate::pane_terminals::HandlerState,
) -> bool {
    let prefix = session_option_key(state, target.session_name(), rmux_proto::OptionName::Prefix);
    let prefix2 = session_option_key(
        state,
        target.session_name(),
        rmux_proto::OptionName::Prefix2,
    );
    let table_name = default_key_table_name(state, target);
    let mut checked_bytes = [false; 256];

    bytes.iter().copied().any(|byte| {
        let byte_index = usize::from(byte);
        if checked_bytes[byte_index] {
            return false;
        }
        checked_bytes[byte_index] = true;

        let encoded = [byte];
        let AttachedKeyDecode::Matched { key, .. } = decode_live_attached_key(&encoded, None)
        else {
            return true;
        };
        matches_prefix_key(key, prefix, prefix2)
            || lookup_attached_key_table_binding(state, &table_name, key_code_lookup_bits(key))
                .is_some()
    })
}

fn submitted_text_before_enter(bytes: &[u8]) -> Option<&[u8]> {
    let enter = bytes
        .iter()
        .position(|byte| matches!(*byte, b'\r' | b'\n'))?;
    (enter > 0).then_some(&bytes[..enter])
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

#[cfg(windows)]
fn windows_console_key_event(key: rmux_proto::AttachedWindowsConsoleKey) -> WindowsConsoleKeyEvent {
    WindowsConsoleKeyEvent::new(
        key.virtual_key_code(),
        key.virtual_scan_code(),
        key.unicode_char(),
        key.control_key_state(),
        key.repeat_count(),
    )
}

#[cfg(windows)]
fn windows_console_binding_key(
    decoded: rmux_core::KeyCode,
    key: WindowsConsoleKeyEvent,
) -> rmux_core::KeyCode {
    windows_console_binding_override_key(decoded, key).unwrap_or(decoded)
}

#[cfg(windows)]
fn windows_synthetic_console_key_for_decoded_key(
    decoded: rmux_core::KeyCode,
) -> Option<WindowsConsoleKeyEvent> {
    key_matches_name(decoded, "C-d").then(WindowsConsoleKeyEvent::ctrl_d)
}

#[cfg(windows)]
fn windows_console_binding_override_key(
    decoded: rmux_core::KeyCode,
    key: WindowsConsoleKeyEvent,
) -> Option<rmux_core::KeyCode> {
    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;
    const RIGHT_CTRL_PRESSED: u32 = 0x0004;
    const CTRL_PRESSED: u32 = LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;

    let control_key_state = key.control_key_state();
    if decoded & KEYC_CTRL != 0 || control_key_state & CTRL_PRESSED == 0 {
        return None;
    }

    if control_key_state & RIGHT_ALT_PRESSED != 0 {
        return None;
    }
    if control_key_state & LEFT_ALT_PRESSED != 0 && control_key_state & RIGHT_CTRL_PRESSED == 0 {
        return None;
    }

    let character = char::from_u32(u32::from(key.unicode_char()))?;
    if !character.is_ascii() || character.is_ascii_control() {
        return None;
    }

    let preserved_modifiers = decoded & (KEYC_META | KEYC_IMPLIED_META | KEYC_SHIFT);
    Some(character.to_ascii_lowercase() as rmux_core::KeyCode | KEYC_CTRL | preserved_modifiers)
}

#[cfg(windows)]
fn key_matches_name(key: rmux_core::KeyCode, name: &str) -> bool {
    windows_key_code_named(name).is_some_and(|expected| expected == key)
}

#[cfg(windows)]
fn windows_key_code_named(name: &str) -> Option<rmux_core::KeyCode> {
    key_string_lookup_string(name).map(key_code_lookup_bits)
}

#[cfg(all(test, windows))]
mod windows_console_binding_tests {
    use rmux_core::{KEYC_CTRL, KEYC_IMPLIED_META, KEYC_META, KEYC_SHIFT};
    use rmux_pty::WindowsConsoleKeyEvent;

    use super::windows_console_binding_override_key;

    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const RIGHT_CTRL_PRESSED: u32 = 0x0004;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;

    fn key(unicode_char: char, control_key_state: u32) -> WindowsConsoleKeyEvent {
        WindowsConsoleKeyEvent::new(0, 0, unicode_char as u16, control_key_state, 1)
    }

    #[test]
    fn alt_gr_is_not_promoted_to_control_binding() {
        assert_eq!(
            windows_console_binding_override_key(
                b'[' as u64,
                key('[', RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED),
            ),
            None
        );
    }

    #[test]
    fn plain_left_control_promotes_printable_character() {
        assert_eq!(
            windows_console_binding_override_key(b';' as u64, key(';', LEFT_CTRL_PRESSED)),
            Some(b';' as u64 | KEYC_CTRL)
        );
    }

    #[test]
    fn meta_and_shift_modifiers_survive_control_promotion() {
        let decoded = b';' as u64 | KEYC_META | KEYC_IMPLIED_META | KEYC_SHIFT;

        assert_eq!(
            windows_console_binding_override_key(decoded, key(';', LEFT_CTRL_PRESSED)),
            Some(b';' as u64 | KEYC_CTRL | KEYC_META | KEYC_IMPLIED_META | KEYC_SHIFT)
        );
    }

    #[test]
    fn left_alt_without_right_ctrl_is_not_alt_gr_or_control_override() {
        assert_eq!(
            windows_console_binding_override_key(b'q' as u64, key('q', LEFT_ALT_PRESSED)),
            None
        );
    }

    #[test]
    fn right_control_still_promotes_printable_character() {
        assert_eq!(
            windows_console_binding_override_key(b'a' as u64, key('A', RIGHT_CTRL_PRESSED)),
            Some(b'a' as u64 | KEYC_CTRL)
        );
    }
}
