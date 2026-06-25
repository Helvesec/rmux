use std::io;

use rmux_core::{input::mode, key_code_lookup_bits};
use rmux_proto::{AttachedKeystroke, PaneTarget, Response};
#[cfg(unix)]
use rmux_pty::PtyMaster;
#[cfg(windows)]
use rmux_pty::WindowsConsoleKeyEvent;

use super::super::super::{prompt_support::PromptInputEvent, RequestHandler};
use super::super::io_other;
use super::super::pane_io_encoding::{
    prepare_attached_pane_input_writes, write_bytes_to_target_io,
};
use super::super::pane_prompt_input::{
    decode_utf8_char, is_extended_key_prefix, is_utf8_lead_byte, utf8_expected_len,
};
use super::bracketed_paste::{decode_bracketed_paste, BracketedPasteDecode};
use super::kitty_graphics::{decode_kitty_graphics_apc, KittyGraphicsApcDecode};
use super::terminal_response::{decode_attached_terminal_control, TerminalResponseDecode};
use super::{
    is_enter_key, is_mouse_prefix, resolve_input_target, retain_partial_attached_control_input,
};
use crate::client_flags::ClientFlags;
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{
    decode_attached_key, lookup_attached_key_table_binding, matches_prefix_key, session_option_key,
    AttachedKeyDecode, PREFIX_TABLE,
};

#[cfg(unix)]
const DIRECT_CURRENT_PANE_INPUT_MAX_BYTES: usize = 16;

impl RequestHandler {
    pub(crate) async fn handle_attached_keystroke_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        keystroke: &AttachedKeystroke,
    ) -> io::Result<bool> {
        self.handle_attached_live_input_inner_with_windows_console_key(
            attach_pid,
            pending_input,
            keystroke.bytes(),
            keystroke.windows_console_key(),
        )
        .await
    }

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
        self.handle_attached_live_input_inner_with_windows_console_key(
            attach_pid,
            pending_input,
            bytes,
            None,
        )
        .await
    }

    #[async_recursion::async_recursion]
    async fn handle_attached_live_input_inner_with_windows_console_key(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<rmux_proto::AttachedWindowsConsoleKey>,
    ) -> io::Result<bool> {
        let mut forwarded_to_pane = false;
        if let Some(forwarded) = self
            .try_forward_plain_attached_bytes_fast(attach_pid, pending_input, bytes)
            .await?
        {
            return Ok(forwarded);
        }
        if self.attached_client_input_is_read_only(attach_pid).await? {
            pending_input.clear();
            return Ok(false);
        }
        #[cfg(not(windows))]
        let _ = windows_console_key;
        #[cfg(windows)]
        let windows_console_key = windows_console_key
            .filter(|_| pending_input.is_empty() && !bytes.is_empty())
            .map(windows_console_key_event);
        self.clear_attached_focus_alerts(attach_pid).await;
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
        let target_mode = self.target_pane_mode(&target).await.map_err(io_other)?;
        let target_bracketed_paste = target_mode & mode::MODE_BRACKETPASTE != 0;
        let target_focus_events = target_mode & mode::MODE_FOCUSON != 0;
        let backspace = self.attached_backspace_byte().await;

        if pending_input.is_empty()
            && !self.attached_prefix_table_active(attach_pid).await
            && self
                .dispatch_immediate_prefix_detach(attach_pid, &target, bytes, backspace)
                .await?
        {
            return Ok(false);
        }

        pending_input.extend_from_slice(bytes);
        let mut raw_start = 0;
        let mut offset = 0;

        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            match decode_bracketed_paste(slice) {
                BracketedPasteDecode::Matched {
                    size,
                    body_start,
                    body_end,
                } => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                    }
                    let payload = if target_bracketed_paste {
                        &pending_input[offset..offset + size]
                    } else {
                        &pending_input[offset + body_start..offset + body_end]
                    };
                    if !payload.is_empty() {
                        self.write_attached_bytes(attach_pid, payload).await?;
                    }
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
            match decode_attached_terminal_control(slice, target_focus_events) {
                TerminalResponseDecode::Matched { size } => {
                    if raw_start < offset {
                        self.write_attached_bytes(attach_pid, &pending_input[raw_start..offset])
                            .await?;
                        forwarded_to_pane = true;
                    }
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
                        #[cfg(windows)]
                        let handled = if let Some(key_event) = windows_console_key.filter(|_| {
                            raw_start == offset
                                && offset == 0
                                && size == pending_input.len()
                                && size == bytes.len()
                        }) {
                            self.handle_attached_live_key_inner(
                                attach_pid,
                                key,
                                super::AttachedPaneForward::WindowsConsoleKey {
                                    key: key_event,
                                    bytes: &pending_input[offset..offset + size],
                                },
                            )
                            .await?
                        } else {
                            self.handle_attached_live_key(attach_pid, key).await?
                        };
                        #[cfg(not(windows))]
                        let handled = self.handle_attached_live_key(attach_pid, key).await?;
                        if !handled {
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
                offset += 1;
                continue;
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
                    #[cfg(windows)]
                    let handled = if let Some(key_event) = windows_console_key.filter(|_| {
                        raw_start == offset
                            && offset == 0
                            && size == pending_input.len()
                            && size == bytes.len()
                    }) {
                        self.handle_attached_live_key_inner(
                            attach_pid,
                            key,
                            super::AttachedPaneForward::WindowsConsoleKey {
                                key: key_event,
                                bytes: &pending_input[offset..offset + size],
                            },
                        )
                        .await?
                    } else {
                        self.handle_attached_live_key(attach_pid, key).await?
                    };
                    #[cfg(not(windows))]
                    let handled = self.handle_attached_live_key(attach_pid, key).await?;
                    if !handled {
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

    async fn try_forward_plain_attached_bytes_fast(
        &self,
        attach_pid: u32,
        pending_input: &[u8],
        bytes: &[u8],
    ) -> io::Result<Option<bool>> {
        if !pending_input.is_empty() || !is_plain_attached_fast_path_input(bytes) {
            return Ok(None);
        }

        let Some(session_name) = self.fast_path_attached_session(attach_pid).await? else {
            return Ok(None);
        };
        let (writes, clear_alerts_changed) = {
            let mut state = self.state.lock().await;
            let target =
                resolve_input_target(&state, None, Some(&session_name)).map_err(io_other)?;
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
            if first_byte_matches_prefix(bytes, &target, &state) {
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
                prepare_attached_pane_input_writes(&mut state, &target, bytes).map_err(io_other)?;
            (writes, clear_alerts_changed)
        };

        for write in writes {
            write_bytes_to_target_io(write, bytes.to_vec())
                .await
                .map_err(io_other)?;
        }
        if clear_alerts_changed {
            let handler = self.clone();
            let refresh_session_name = session_name.clone();
            tokio::spawn(async move {
                handler
                    .refresh_attached_session(&refresh_session_name)
                    .await;
            });
        }
        Ok(Some(true))
    }

    #[cfg(unix)]
    pub(crate) async fn try_forward_plain_attached_bytes_to_current_pane_fast(
        &self,
        attach_pid: u32,
        pending_input: &[u8],
        bytes: &[u8],
        target: &PaneTarget,
        master: &PtyMaster,
    ) -> io::Result<Option<bool>> {
        if !is_direct_current_pane_fast_path_input(bytes)
            || !pending_input.is_empty()
            || !is_plain_attached_fast_path_input(bytes)
        {
            return Ok(None);
        }

        let Some(session_name) = self.fast_path_attached_session(attach_pid).await? else {
            return Ok(None);
        };
        if &session_name != target.session_name() {
            return Ok(None);
        }

        let clear_alerts_changed = {
            let mut state = self.state.lock().await;
            let resolved = resolve_input_target(&state, None, Some(target.session_name()))
                .map_err(io_other)?;
            if !same_pane_target(&resolved, target) {
                return Ok(None);
            }
            if state.options.resolve_for_window(
                target.session_name(),
                target.window_index(),
                rmux_proto::OptionName::SynchronizePanes,
            ) == Some("on")
            {
                return Ok(None);
            }
            let pane_id = state
                .sessions
                .session(target.session_name())
                .and_then(|session| session.window_at(target.window_index()))
                .and_then(|window| window.pane(target.pane_index()))
                .map(rmux_core::Pane::id)
                .ok_or_else(|| {
                    io_other(rmux_proto::RmuxError::invalid_target(
                        target.to_string(),
                        "pane index does not exist in session",
                    ))
                })?;
            if state.pane_is_dead(target.session_name(), pane_id)
                || state.pane_input_is_disabled(pane_id)
            {
                return Ok(None);
            }
            let transcript = state.transcript_handle(target).map_err(io_other)?;
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
            if first_byte_matches_prefix(bytes, target, &state) {
                return Ok(None);
            }
            state
                .sessions
                .session_mut(target.session_name())
                .is_some_and(|session| session.clear_all_winlink_alert_flags(target.window_index()))
        };

        match master.try_write_immediate(bytes)? {
            written if written == bytes.len() => {
                if clear_alerts_changed {
                    let handler = self.clone();
                    let refresh_session_name = session_name.clone();
                    tokio::spawn(async move {
                        handler
                            .refresh_attached_session(&refresh_session_name)
                            .await;
                    });
                }
                Ok(Some(true))
            }
            0 => Ok(None),
            written => {
                let suffix = bytes[written..].to_vec();
                let master = master.try_clone().map_err(io::Error::other)?;
                tokio::task::spawn_blocking(move || master.write_all(&suffix))
                    .await
                    .map_err(|error| {
                        io::Error::other(format!("pane write task failed: {error}"))
                    })??;
                if clear_alerts_changed {
                    let handler = self.clone();
                    let refresh_session_name = session_name.clone();
                    tokio::spawn(async move {
                        handler
                            .refresh_attached_session(&refresh_session_name)
                            .await;
                    });
                }
                Ok(Some(true))
            }
        }
    }

    async fn fast_path_attached_session(
        &self,
        attach_pid: u32,
    ) -> io::Result<Option<rmux_proto::SessionName>> {
        let active_attach = self.active_attach.lock().await;
        let active = active_attach.by_pid.get(&attach_pid).ok_or_else(|| {
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
            || active.key_table_name.as_deref() == Some(PREFIX_TABLE)
        {
            return Ok(None);
        }
        Ok(Some(active.session_name.clone()))
    }

    async fn attached_prefix_table_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .and_then(|active| active.key_table_name.as_deref())
            == Some(PREFIX_TABLE)
    }

    async fn clear_attached_focus_alerts(&self, attach_pid: u32) {
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
            || self.attached_prefix_table_active(attach_pid).await
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

    async fn dispatch_immediate_prefix_detach(
        &self,
        attach_pid: u32,
        target: &rmux_proto::PaneTarget,
        bytes: &[u8],
        backspace: Option<u8>,
    ) -> io::Result<bool> {
        let AttachedKeyDecode::Matched {
            size: prefix_size,
            key: prefix_key,
        } = decode_attached_key(bytes, backspace)
        else {
            return Ok(false);
        };
        if prefix_size == 0 || prefix_size >= bytes.len() {
            return Ok(false);
        }

        let AttachedKeyDecode::Matched {
            size: command_size,
            key: command_key,
        } = decode_attached_key(&bytes[prefix_size..], backspace)
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

        match self.handle_detach_client(attach_pid).await {
            Response::Error(error) => Err(io_other(error.error)),
            _ => Ok(true),
        }
    }
}

fn is_plain_attached_fast_path_input(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|byte| matches!(*byte, b'\r' | b'\n' | b' '..=b'~'))
}

#[cfg(unix)]
fn is_direct_current_pane_fast_path_input(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() <= DIRECT_CURRENT_PANE_INPUT_MAX_BYTES
        && bytes.iter().all(|byte| matches!(*byte, b' '..=b'~'))
}

fn first_byte_matches_prefix(
    bytes: &[u8],
    target: &PaneTarget,
    state: &crate::pane_terminals::HandlerState,
) -> bool {
    let AttachedKeyDecode::Matched { key, .. } = decode_attached_key(bytes, None) else {
        return false;
    };
    let prefix = session_option_key(state, target.session_name(), rmux_proto::OptionName::Prefix);
    let prefix2 = session_option_key(
        state,
        target.session_name(),
        rmux_proto::OptionName::Prefix2,
    );
    matches_prefix_key(key, prefix, prefix2)
}

#[cfg(unix)]
fn same_pane_target(left: &PaneTarget, right: &PaneTarget) -> bool {
    left.session_name() == right.session_name()
        && left.window_index() == right.window_index()
        && left.pane_index() == right.pane_index()
}

fn submitted_text_before_enter(bytes: &[u8]) -> Option<&[u8]> {
    let enter = bytes
        .iter()
        .position(|byte| matches!(*byte, b'\r' | b'\n'))?;
    (enter > 0).then_some(&bytes[..enter])
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
