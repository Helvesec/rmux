use std::io;

use super::super::super::{prompt_support::PromptInputEvent, RequestHandler};
use super::super::io_other;
use super::super::pane_prompt_input::{
    decode_utf8_char, is_extended_key_prefix, is_utf8_lead_byte, utf8_expected_len,
};
use super::bracketed_paste::{decode_bracketed_paste, BracketedPasteDecode};
use super::kitty_graphics::{decode_kitty_graphics_apc, KittyGraphicsApcDecode};
use super::terminal_response::{decode_terminal_response, TerminalResponseDecode};
use super::{is_enter_key, is_mouse_prefix, retain_partial_attached_control_input};
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{
    decode_attached_key, session_option_key, AttachedKeyDecode, PREFIX_TABLE,
};
use rmux_core::key_code_to_bytes;
use rmux_proto::OptionName;

impl RequestHandler {
    #[async_recursion::async_recursion]
    pub(crate) async fn handle_attached_live_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.handle_attached_live_input_inner(attach_pid, pending_input, bytes)
            .await
            .map(|_| ())
    }

    #[async_recursion::async_recursion]
    pub(crate) async fn handle_attached_live_input_inner(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<bool> {
        self.handle_attached_live_input_inner_dispatched(
            attach_pid,
            pending_input,
            bytes,
            /* allow_passthrough_dispatch = */ true,
        )
        .await
    }

    /// Inner implementation. `allow_passthrough_dispatch` is true on the
    /// outer entry and false when the passthrough fast path defers a
    /// prefix-byte slice back into this function — without that gate,
    /// the passthrough check at the bottom would route the prefix byte
    /// straight back into the passthrough handler and we'd recurse
    /// forever (stack overflow → server SIGABRT). Caught by
    /// `passthrough_prefix_byte_does_not_recurse_into_passthrough_handler`.
    #[async_recursion::async_recursion]
    async fn handle_attached_live_input_inner_dispatched(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        allow_passthrough_dispatch: bool,
    ) -> io::Result<bool> {
        let mut forwarded_to_pane = false;
        let focused_window = {
            let session_name = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .map(|active| active.session_name.clone())
            };
            match session_name {
                Some(session_name) => {
                    let window_index = {
                        let state = self.state.lock().await;
                        state
                            .sessions
                            .session(&session_name)
                            .map(|session| session.active_window_index())
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
        if self.prompt_active(attach_pid).await {
            self.handle_attached_prompt_input(attach_pid, pending_input, bytes)
                .await?;
            return Ok(false);
        }
        if self.mode_tree_active(attach_pid).await {
            self.handle_attached_mode_tree_input(attach_pid, pending_input, bytes)
                .await?;
            return Ok(false);
        }
        if self.overlay_active(attach_pid).await
            && self
                .handle_attached_overlay_input(attach_pid, pending_input, bytes)
                .await?
        {
            return Ok(false);
        }
        if self.display_panes_active(attach_pid).await {
            self.handle_attached_display_panes_input(attach_pid, pending_input, bytes)
                .await?;
            return Ok(false);
        }
        let target = self
            .attached_input_target(attach_pid)
            .await
            .map_err(io_other)?;
        if self
            .target_is_in_clock_mode(&target)
            .await
            .map_err(io_other)?
        {
            let _ = self.exit_clock_mode(&target).await.map_err(io_other)?;
            pending_input.clear();
            return Ok(false);
        }
        let target_in_copy_mode = self
            .target_is_in_copy_mode(&target)
            .await
            .map_err(io_other)?;

        // Passthrough sessions get a raw-forwarding path: socket bytes
        // reach the pane verbatim, with the *only* interception being
        // the configured prefix byte (so Ctrl-B w / c / etc. still
        // work). This is the philosophy of passthrough — rmux must
        // not interpret keys, because it doesn't emulate the inner
        // terminal and any decode→encode round-trip is lossy. The most
        // visible casualty without this branch was arrow keys inside
        // vim: vim sets app-cursor mode (`\x1b[?1h`) which flows to
        // the host; host then sends `\x1bOA` for ↑; rmux's encoder
        // stripped the cursor flag (pane_mode=0) and emitted `\x1b[A`
        // which vim didn't recognise.
        if allow_passthrough_dispatch
            && !target_in_copy_mode
            && !self.attached_prefix_table_active(attach_pid).await
            && self.is_session_passthrough(target.session_name()).await
        {
            return self
                .handle_attached_live_input_passthrough(
                    attach_pid,
                    &target,
                    pending_input,
                    bytes,
                )
                .await;
        }

        pending_input.extend_from_slice(bytes);
        let backspace = self.attached_backspace_byte().await;
        let mut raw_start = 0;
        let mut offset = 0;

        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            match decode_bracketed_paste(slice) {
                BracketedPasteDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                    }
                    self.write_attached_bytes(attach_pid, &pending_input[offset..offset + size])
                        .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                BracketedPasteDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input("live bracketed paste", pending_input)?;
                    return Ok(forwarded_to_pane);
                }
                BracketedPasteDecode::NotPaste => {}
            }
            match decode_kitty_graphics_apc(slice) {
                KittyGraphicsApcDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                    }
                    self.write_attached_bytes(attach_pid, &pending_input[offset..offset + size])
                        .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                KittyGraphicsApcDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input(
                        "live kitty graphics APC",
                        pending_input,
                    )?;
                    return Ok(forwarded_to_pane);
                }
                KittyGraphicsApcDecode::NotKittyGraphics => {}
            }
            match decode_terminal_response(slice) {
                TerminalResponseDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                    }
                    self.write_attached_bytes(attach_pid, &pending_input[offset..offset + size])
                        .await?;
                    forwarded_to_pane = true;
                    offset += size;
                    raw_start = offset;
                    continue;
                }
                TerminalResponseDecode::Partial => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                        forwarded_to_pane = true;
                    }
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input("live terminal response", pending_input)?;
                    return Ok(forwarded_to_pane);
                }
                TerminalResponseDecode::NotResponse => {}
            }
            if is_mouse_prefix(slice) {
                if raw_start < offset {
                    self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                        .await?;
                    forwarded_to_pane = true;
                }
                let last_mouse = self.attached_last_mouse_event(attach_pid).await;
                match decode_mouse(slice, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        self.handle_attached_live_mouse(attach_pid, event).await?;
                        offset += size;
                        raw_start = offset;
                    }
                    MouseDecode::Discard { size } => {
                        offset += size;
                        raw_start = offset;
                    }
                    MouseDecode::Partial => {
                        pending_input.drain(..raw_start);
                        retain_partial_attached_control_input("live mouse", pending_input)?;
                        return Ok(forwarded_to_pane);
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
                    self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                        .await?;
                    forwarded_to_pane = true;
                }
                match decode_extended_key(slice, backspace) {
                    ExtendedKeyDecode::Matched { size, key } => {
                        if raw_start < offset && is_enter_key(key) {
                            self.record_attached_submitted_text(
                                attach_pid,
                                &pending_input[raw_start..offset],
                            )
                            .await?;
                        }
                        if !self.handle_attached_live_key(attach_pid, key).await? {
                            forwarded_to_pane = true;
                        }
                        offset += size;
                        raw_start = offset;
                        if let Some(forwarded) = self
                            .reroute_attached_remaining_input_if_mode_changed(
                                attach_pid,
                                pending_input,
                                raw_start,
                            )
                            .await?
                        {
                            forwarded_to_pane |= forwarded;
                            return Ok(forwarded_to_pane);
                        }
                        if self.prompt_active(attach_pid).await {
                            break;
                        }
                        continue;
                    }
                    ExtendedKeyDecode::Partial => {
                        pending_input.drain(..raw_start);
                        retain_partial_attached_control_input("live extended key", pending_input)?;
                        return Ok(forwarded_to_pane);
                    }
                    ExtendedKeyDecode::Invalid => {}
                }
            }
            let prefix_table_active = self.attached_prefix_table_active(attach_pid).await;
            if slice
                .first()
                .is_some_and(|byte| byte.is_ascii() && !byte.is_ascii_control())
                && !prefix_table_active
                && !target_in_copy_mode
            {
                offset += 1;
                continue;
            }
            if !prefix_table_active
                && !target_in_copy_mode
                && slice.first().is_some_and(|byte| !byte.is_ascii())
            {
                if let Some((_, size)) = decode_utf8_char(slice) {
                    offset += size;
                    continue;
                }
                if slice.first().copied().is_some_and(is_utf8_lead_byte)
                    && slice.len()
                        < utf8_expected_len(
                            slice.first().copied().expect("slice has at least one byte"),
                        )
                {
                    pending_input.drain(..raw_start);
                    retain_partial_attached_control_input("live utf-8", pending_input)?;
                    return Ok(forwarded_to_pane);
                }
            }
            match decode_attached_key(slice, backspace) {
                AttachedKeyDecode::Matched { size, key } => {
                    if raw_start < offset && is_enter_key(key) {
                        self.record_attached_submitted_text(
                            attach_pid,
                            &pending_input[raw_start..offset],
                        )
                        .await?;
                    }
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                        forwarded_to_pane = true;
                    }
                    if !self.handle_attached_live_key(attach_pid, key).await? {
                        forwarded_to_pane = true;
                    }
                    offset += size;
                    raw_start = offset;
                    if let Some(forwarded) = self
                        .reroute_attached_remaining_input_if_mode_changed(
                            attach_pid,
                            pending_input,
                            raw_start,
                        )
                        .await?
                    {
                        forwarded_to_pane |= forwarded;
                        return Ok(forwarded_to_pane);
                    }
                    if self.prompt_active(attach_pid).await {
                        break;
                    }
                    continue;
                }
                AttachedKeyDecode::Partial => {
                    if target_in_copy_mode
                        && slice == b"\x1b"
                        && self
                            .handle_attached_copy_mode_key_event(
                                attach_pid,
                                target.clone(),
                                PromptInputEvent::Escape,
                            )
                            .await
                            .map_err(io_other)?
                    {
                        offset += 1;
                        raw_start = offset;
                        continue;
                    }
                    pending_input.drain(..raw_start);
                    retain_partial_attached_control_input("live attached key", pending_input)?;
                    return Ok(forwarded_to_pane);
                }
                AttachedKeyDecode::Invalid => {}
            }
            offset += 1;
        }

        if self.prompt_active(attach_pid).await && raw_start < pending_input.len() {
            let remaining = pending_input[raw_start..].to_vec();
            pending_input.clear();
            Box::pin(self.handle_attached_live_input(attach_pid, pending_input, &remaining))
                .await?;
            return Ok(forwarded_to_pane);
        }

        if raw_start < pending_input.len() {
            self.write_attached_bytes(attach_pid, &pending_input[raw_start..])
                .await?;
            forwarded_to_pane = true;
        }
        pending_input.clear();
        Ok(forwarded_to_pane)
    }

    /// Raw-forwarding input path used by passthrough sessions. Writes
    /// every byte of `bytes` to the pane verbatim, except where the
    /// configured prefix appears — at which point the legacy
    /// decode/dispatch loop takes over so prefix bindings still work.
    ///
    /// Prefix detection accepts three encodings of the same key:
    ///   * The literal byte (e.g. `\x02` for Ctrl-B) — the common case.
    ///   * xterm modifyOtherKeys CSI (`\x1b[27;5;98~` for Ctrl-B) —
    ///     emitted when an app like vim has opted the terminal into
    ///     modifyOtherKeys mode and the host (WezTerm etc.) honours it.
    ///   * kitty keyboard protocol CSI-u (`\x1b[98;5u` for Ctrl-B).
    ///
    /// Edge cases this deliberately leaves to the legacy path:
    ///   * Multi-byte literal prefix keys (e.g. F-keys) — we fall
    ///     back so correctness wins over the encoding round-trip cost.
    ///   * Already inside the prefix table (waiting for the follow-on
    ///     key) — checked at the call site; legacy handles dispatch.
    ///   * Overlay / mode-tree / prompt / display-panes / clock / copy
    ///     modes — checked at the call site before we get here.
    async fn handle_attached_live_input_passthrough(
        &self,
        attach_pid: u32,
        target: &rmux_proto::PaneTarget,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let prefix_byte = self.passthrough_prefix_byte(target).await;
        let prefix_key = self.passthrough_prefix_key(target).await;

        let split = find_prefix_match(bytes, prefix_byte, prefix_key);
        let raw_part = match split {
            Some((start, _)) => &bytes[..start],
            None => bytes,
        };
        let mut forwarded = false;

        // Flush any leftover legacy buffer (e.g. a partial sequence
        // captured by an earlier non-passthrough call) before our raw
        // writes so we don't reorder bytes.
        if !pending_input.is_empty() {
            self.write_attached_bytes(attach_pid, pending_input).await?;
            pending_input.clear();
            forwarded = true;
        }
        // Filter `raw_part` for orphaned terminal responses
        // (`\x1b[?…c`, `\x1b[…;…R`, etc.).  In passthrough mode the
        // host terminal sends real replies because the daemon doesn't
        // intercept queries — so if vim queries DA1 and exits before
        // the round-trip completes, the reply lands here destined for
        // the shell.  We drop it iff the counter says no query is
        // outstanding.  See the long comment on
        // `ActiveAttach::outstanding_terminal_queries`.
        if !raw_part.is_empty() {
            let kept = self
                .strip_orphan_terminal_responses(attach_pid, raw_part)
                .await;
            if !kept.is_empty() {
                self.write_attached_bytes(attach_pid, &kept).await?;
                forwarded = true;
            }
        }

        let Some((start, _)) = split else {
            return Ok(forwarded);
        };

        // Prefix sequence (and anything after it) flows through the
        // legacy decode loop — that's where bindings live, and it
        // already knows how to decode both literal bytes and extended
        // CSI key sequences. We MUST call the _dispatched variant with
        // `allow_passthrough_dispatch = false` here; otherwise the
        // passthrough check would route the prefix straight back into
        // us and we'd recurse forever.
        let prefix_and_after = &bytes[start..];
        let legacy_forwarded = Box::pin(
            self.handle_attached_live_input_inner_dispatched(
                attach_pid,
                pending_input,
                prefix_and_after,
                /* allow_passthrough_dispatch = */ false,
            ),
        )
        .await?;
        Ok(forwarded || legacy_forwarded)
    }

    /// Returns the single-byte representation of the session's prefix
    /// key, if it has one. Multi-byte prefix keys (extended / F-keys)
    /// return `None` — extended CSI encoding of those is still caught
    /// by [`passthrough_prefix_key`] + [`find_prefix_match`].
    async fn passthrough_prefix_byte(
        &self,
        target: &rmux_proto::PaneTarget,
    ) -> Option<u8> {
        let session_name = target.session_name().clone();
        let state = self.state.lock().await;
        let prefix = session_option_key(&state, &session_name, OptionName::Prefix)?;
        let bytes = key_code_to_bytes(prefix)?;
        if bytes.len() == 1 {
            Some(bytes[0])
        } else {
            None
        }
    }

    /// Returns the session's prefix as a `KeyCode` so [`find_prefix_match`]
    /// can compare against `decode_extended_key`'s output, catching
    /// modifyOtherKeys / CSI-u encodings of the prefix.
    async fn passthrough_prefix_key(
        &self,
        target: &rmux_proto::PaneTarget,
    ) -> Option<rmux_core::KeyCode> {
        let session_name = target.session_name().clone();
        let state = self.state.lock().await;
        session_option_key(&state, &session_name, OptionName::Prefix)
    }

    /// Walk `bytes`, splitting out CSI terminal-response sequences
    /// (`\x1b[?…c`, `\x1b[…;…R`, …) and **dropping** any that arrive
    /// while the outstanding-query counter is 0.  Responses that pair
    /// with a tracked query are kept and decrement the counter.
    /// Returns the filtered buffer.
    ///
    /// Used by the passthrough fast path on the client→pane direction
    /// to defuse the post-vim-exit DA1 leak.
    async fn strip_orphan_terminal_responses(
        &self,
        attach_pid: u32,
        bytes: &[u8],
    ) -> Vec<u8> {
        let mut output = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b
                && bytes.get(i + 1).copied() == Some(b'[')
            {
                match decode_terminal_response(&bytes[i..]) {
                    TerminalResponseDecode::Matched { size } => {
                        if self.consume_outstanding_terminal_query(attach_pid).await {
                            // Genuine reply — pass through.
                            output.extend_from_slice(&bytes[i..i + size]);
                        }
                        // else: orphan — drop the bytes silently.
                        i += size;
                        continue;
                    }
                    TerminalResponseDecode::Partial
                    | TerminalResponseDecode::NotResponse => {
                        // Not a complete response or not a response at
                        // all — keep flowing.  A partial here is
                        // rare (responses fit in one ssh packet
                        // typically), and getting the cleanup wrong
                        // on a partial would tear an extended-key
                        // sequence in half.  Best-effort: forward
                        // verbatim.
                    }
                }
            }
            output.push(bytes[i]);
            i += 1;
        }
        output
    }

    /// Add `count` to the outstanding-terminal-query counter for the
    /// given attach.  Called by the passthrough forwarder when it
    /// emits pane→client bytes that contain a DA1/DA2/DSR/etc. query.
    /// See [`crate::terminal_query::count_terminal_queries`] for the
    /// query shapes counted.
    pub(crate) async fn bump_outstanding_terminal_queries(&self, attach_pid: u32, count: u32) {
        if count == 0 {
            return;
        }
        let mut active_attach = self.active_attach.lock().await;
        if let Some(attach) = active_attach.by_pid.get_mut(&attach_pid) {
            attach.outstanding_terminal_queries =
                attach.outstanding_terminal_queries.saturating_add(count);
        }
    }

    /// Reset the outstanding-terminal-query counter to zero.
    ///
    /// Called when the passthrough forwarder observes alt-screen-exit
    /// (`\x1b[?1049l`) — a strong signal that a curses program (vi,
    /// less, htop, nvim) has finished and the pane is back to whatever
    /// was running before (typically the user's shell).  Any in-flight
    /// queries from the finished program are now orphans — their
    /// replies are bouncing back across SSH and will land at the new
    /// listener (the shell), not the original asker.  Wipe the counter
    /// so `strip_orphan_terminal_responses` drops those replies.
    pub(crate) async fn reset_outstanding_terminal_queries(&self, attach_pid: u32) {
        let mut active_attach = self.active_attach.lock().await;
        if let Some(attach) = active_attach.by_pid.get_mut(&attach_pid) {
            attach.outstanding_terminal_queries = 0;
        }
    }

    /// Try to consume one outstanding terminal query for the given
    /// attach.  Returns `true` if the counter was non-zero and we
    /// decremented (response is genuine — forward it).  Returns
    /// `false` if no query was outstanding (response is an orphan —
    /// drop it).
    async fn consume_outstanding_terminal_query(&self, attach_pid: u32) -> bool {
        let mut active_attach = self.active_attach.lock().await;
        if let Some(attach) = active_attach.by_pid.get_mut(&attach_pid) {
            if attach.outstanding_terminal_queries > 0 {
                attach.outstanding_terminal_queries -= 1;
                return true;
            }
        }
        false
    }

    async fn attached_prefix_table_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .and_then(|active| active.key_table_name.as_deref())
            == Some(PREFIX_TABLE)
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

    async fn reroute_attached_remaining_input_if_mode_changed(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        consumed: usize,
    ) -> io::Result<Option<bool>> {
        if consumed >= pending_input.len() {
            return Ok(None);
        }

        let target = self
            .attached_input_target(attach_pid)
            .await
            .map_err(io_other)?;
        let interactive_mode_active = self.prompt_active(attach_pid).await
            || self.mode_tree_active(attach_pid).await
            || self.overlay_active(attach_pid).await
            || self.display_panes_active(attach_pid).await
            || self
                .target_is_in_clock_mode(&target)
                .await
                .map_err(io_other)?;
        if !interactive_mode_active {
            return Ok(None);
        }

        let remaining = pending_input[consumed..].to_vec();
        pending_input.clear();
        let forwarded =
            Box::pin(self.handle_attached_live_input_inner(attach_pid, pending_input, &remaining))
                .await?;
        Ok(Some(forwarded))
    }
}

/// Scan `bytes` for the first occurrence of the session prefix.
///
/// Returns `Some((start_offset, sequence_length))` of the match, where
/// the sequence is either the literal `prefix_byte` (1 byte) or a CSI
/// extended-key sequence (`\x1b[27;…~` / `\x1b[…;…u`) that decodes to
/// the configured `prefix_key`.  Returns `None` when no occurrence
/// exists — the caller forwards the entire buffer raw in that case.
///
/// Why we scan instead of trusting `decode_extended_key` to consume
/// the byte stream linearly: the production input often contains
/// non-prefix bytes (typed shell input) followed by the prefix
/// (`hello\x02`), or extended sequences that aren't the prefix
/// (vim's `\x1bOA` up-arrow on a passthrough session).  We need to
/// preserve those bytes verbatim, intercepting *only* the configured
/// prefix.
fn find_prefix_match(
    bytes: &[u8],
    prefix_byte: Option<u8>,
    prefix_key: Option<rmux_core::KeyCode>,
) -> Option<(usize, usize)> {
    use rmux_core::{KEYC_MASK_KEY, KEYC_MASK_MODIFIERS};

    // Strip any non-essential bits so the comparison survives the
    // decode→encode round-trip the same way the legacy prefix table
    // matcher does (see `key_table::matches_prefix_key`).
    let mask = KEYC_MASK_KEY | KEYC_MASK_MODIFIERS;
    let masked_prefix = prefix_key.map(|key| key & mask);

    let mut i = 0;
    while i < bytes.len() {
        if let Some(byte) = prefix_byte {
            if bytes[i] == byte {
                return Some((i, 1));
            }
        }
        if let Some(target_key) = masked_prefix {
            if is_extended_key_prefix(&bytes[i..]) {
                if let ExtendedKeyDecode::Matched { size, key } =
                    decode_extended_key(&bytes[i..], None)
                {
                    if key & mask == target_key {
                        return Some((i, size));
                    }
                    // Decoded as something other than the prefix
                    // (e.g. an arrow key under modifyOtherKeys).
                    // Skip past it so the next iteration doesn't try
                    // to interpret its inner bytes as a fresh CSI
                    // start.  The bytes still get forwarded verbatim
                    // by the caller as part of `raw_part`.
                    i += size;
                    continue;
                }
            }
        }
        i += 1;
    }
    None
}
