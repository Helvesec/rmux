use std::io;
use std::time::Instant;

use rmux_core::{key_code_lookup_bits, key_code_to_bytes, key_string_lookup_string};
use rmux_proto::{OptionName, PaneTarget, RmuxError, Target, DEFAULT_MAX_FRAME_LENGTH};

use super::super::{
    prompt_support::{decode_prompt_key, PromptInputEvent},
    RequestHandler,
};
use super::pane_io_encoding::{
    encode_key_for_target, encode_mouse_for_target, prepare_attached_pane_input_writes,
    write_bytes_to_target_io,
};
use super::pane_prompt_input::{decode_prompt_input_event, is_extended_key_prefix};
use super::{io_other, resolve_input_target, AttachedKeyDispatch};
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};
use crate::mouse::{classify_mouse_event, layout_for_session};
use crate::pane_io::{AttachControl, OverlayFrame};

#[path = "attached_input/bracketed_paste.rs"]
mod bracketed_paste;
#[path = "attached_input/kitty_graphics.rs"]
mod kitty_graphics;
#[path = "attached_input/live.rs"]
mod live;
#[path = "attached_input/terminal_response.rs"]
mod terminal_response;

const MAX_RETAINED_ATTACHED_CONTROL_INPUT: usize = DEFAULT_MAX_FRAME_LENGTH;

pub(in crate::handler) fn retain_partial_attached_control_input(
    context: &str,
    pending_input: &mut Vec<u8>,
) -> io::Result<()> {
    let retained = pending_input.len();
    if retained <= MAX_RETAINED_ATTACHED_CONTROL_INPUT {
        return Ok(());
    }

    pending_input.clear();
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{context} retained {retained} bytes of partial attached control input; maximum is {MAX_RETAINED_ATTACHED_CONTROL_INPUT}"
        ),
    ))
}

impl RequestHandler {
    async fn handle_attached_mode_tree_key_or_prefix(
        &self,
        attach_pid: u32,
        key: rmux_core::KeyCode,
        fallback_event: PromptInputEvent,
    ) -> io::Result<()> {
        let target = self
            .attached_input_target(attach_pid)
            .await
            .map_err(io_other)?;
        let handled = self
            .dispatch_attached_key_inner(
                &target,
                AttachedKeyDispatch {
                    attach_pid,
                    requester_pid: attach_pid,
                    current_target: Some(Target::Pane(target.clone())),
                    mouse_target: None,
                    key,
                    attached_live_input: true,
                },
            )
            .await
            .map_err(io_other)?;
        if handled {
            return Ok(());
        }

        let _ = self
            .handle_mode_tree_key_event(attach_pid, fallback_event)
            .await
            .map_err(io_other)?;
        Ok(())
    }

    async fn handle_attached_live_key(
        &self,
        attach_pid: u32,
        key: rmux_core::KeyCode,
    ) -> io::Result<bool> {
        if self.mode_tree_active(attach_pid).await {
            self.handle_attached_mode_tree_key_or_prefix(attach_pid, key, decode_prompt_key(key))
                .await?;
            return Ok(true);
        }
        let target = self
            .attached_input_target(attach_pid)
            .await
            .map_err(io_other)?;
        if self
            .handle_attached_copy_mode_key_event(attach_pid, target.clone(), decode_prompt_key(key))
            .await
            .map_err(io_other)?
        {
            return Ok(true);
        }
        let handled = self
            .dispatch_attached_key_inner(
                &target,
                AttachedKeyDispatch {
                    attach_pid,
                    requester_pid: attach_pid,
                    current_target: Some(Target::Pane(target.clone())),
                    mouse_target: None,
                    key,
                    attached_live_input: true,
                },
            )
            .await
            .map_err(io_other)?;
        if handled {
            return Ok(true);
        }

        let prepared = {
            let state = self.state.lock().await;
            let Some(encoded) = encode_key_for_target(&state, &target, key).map_err(io_other)?
            else {
                return Ok(false);
            };
            let writes =
                prepare_attached_pane_input_writes(&state, &target, &encoded).map_err(io_other)?;
            (writes, encoded)
        };
        for write in prepared.0 {
            write_bytes_to_target_io(write, prepared.1.clone())
                .await
                .map_err(io_other)?;
        }
        Ok(false)
    }

    #[async_recursion::async_recursion]
    async fn handle_attached_prompt_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<()> {
        pending_input.extend_from_slice(bytes);
        let mut deferred_refresh = false;

        loop {
            let Some((event, consumed)) = decode_prompt_input_event(pending_input) else {
                if deferred_refresh {
                    self.flush_attached_prompt_refresh(attach_pid)
                        .await
                        .map_err(io_other)?;
                }
                retain_partial_attached_control_input("prompt input", pending_input)?;
                return Ok(());
            };
            pending_input.drain(..consumed);
            self.handle_prompt_event_deferred_refresh(attach_pid, event, &mut deferred_refresh)
                .await
                .map_err(io_other)?;
            if !self.prompt_active(attach_pid).await {
                break;
            }
        }

        if deferred_refresh && self.prompt_active(attach_pid).await {
            self.flush_attached_prompt_refresh(attach_pid)
                .await
                .map_err(io_other)?;
        }

        if !pending_input.is_empty() {
            let remaining = std::mem::take(pending_input);
            Box::pin(self.handle_attached_live_input(attach_pid, pending_input, &remaining))
                .await?;
        }
        Ok(())
    }

    async fn handle_attached_mode_tree_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<()> {
        pending_input.extend_from_slice(bytes);
        let backspace = self.attached_backspace_byte().await;
        let mut offset = 0;

        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            if is_mouse_prefix(slice) {
                let last_mouse = self.attached_last_mouse_event(attach_pid).await;
                match decode_mouse(slice, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        let _ = self
                            .handle_mode_tree_mouse_event(attach_pid, event)
                            .await
                            .map_err(io_other)?;
                        offset += size;
                    }
                    MouseDecode::Discard { size } => {
                        offset += size;
                    }
                    MouseDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_control_input("mode-tree mouse", pending_input)?;
                        return Ok(());
                    }
                    MouseDecode::Invalid => {
                        offset += 1;
                    }
                }
                if self.prompt_active(attach_pid).await || !self.mode_tree_active(attach_pid).await
                {
                    break;
                }
                continue;
            }
            if is_extended_key_prefix(slice) {
                match decode_extended_key(slice, backspace) {
                    ExtendedKeyDecode::Matched { size, key } => {
                        self.handle_attached_mode_tree_key_or_prefix(
                            attach_pid,
                            key,
                            decode_prompt_key(key),
                        )
                        .await?;
                        offset += size;
                        if self.prompt_active(attach_pid).await
                            || !self.mode_tree_active(attach_pid).await
                        {
                            break;
                        }
                        continue;
                    }
                    ExtendedKeyDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_control_input(
                            "mode-tree extended key",
                            pending_input,
                        )?;
                        return Ok(());
                    }
                    ExtendedKeyDecode::Invalid => {}
                }
            }

            match decode_attached_key(slice, backspace) {
                AttachedKeyDecode::Matched { size, key } => {
                    let fallback_event = decode_prompt_input_event(slice)
                        .filter(|(_, consumed)| *consumed == size)
                        .map(|(event, _)| event)
                        .unwrap_or_else(|| decode_prompt_key(key));
                    self.handle_attached_mode_tree_key_or_prefix(attach_pid, key, fallback_event)
                        .await?;
                    offset += size;
                }
                AttachedKeyDecode::Partial => {
                    pending_input.drain(..offset);
                    retain_partial_attached_control_input("mode-tree attached key", pending_input)?;
                    return Ok(());
                }
                AttachedKeyDecode::Invalid => {
                    let Some((event, consumed)) = decode_prompt_input_event(slice) else {
                        pending_input.drain(..offset);
                        retain_partial_attached_control_input(
                            "mode-tree prompt input",
                            pending_input,
                        )?;
                        return Ok(());
                    };
                    offset += consumed;
                    let _ = self
                        .handle_mode_tree_key_event(attach_pid, event)
                        .await
                        .map_err(io_other)?;
                }
            }
            if self.prompt_active(attach_pid).await || !self.mode_tree_active(attach_pid).await {
                break;
            }
        }

        pending_input.drain(..offset);
        if !pending_input.is_empty() {
            let remaining = std::mem::take(pending_input);
            Box::pin(self.handle_attached_live_input(attach_pid, pending_input, &remaining))
                .await?;
        }
        Ok(())
    }

    async fn handle_attached_live_mouse(
        &self,
        attach_pid: u32,
        raw: crate::input_keys::MouseForwardEvent,
    ) -> io::Result<()> {
        if self.mode_tree_active(attach_pid).await {
            let _ = self
                .handle_mode_tree_mouse_event(attach_pid, raw)
                .await
                .map_err(io_other)?;
            return Ok(());
        }
        let session_name = self
            .attached_session_name(attach_pid)
            .await
            .map_err(io_other)?;
        let attached_count = self.attached_count(&session_name).await;
        let layout = {
            let state = self.state.lock().await;
            layout_for_session(&state, &session_name, attached_count)
        };
        let Some(layout) = layout else {
            return Ok(());
        };
        let classified = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| io_other("attached client disappeared"))?;
            classify_mouse_event(&mut active.mouse, &layout, raw, Instant::now())
        };
        let Some(classified) = classified else {
            return Ok(());
        };
        let target = if let Some(target) = classified.event.pane_target.clone() {
            target
        } else {
            self.attached_input_target(attach_pid)
                .await
                .map_err(io_other)?
        };
        let current_target = self
            .attached_mouse_target(attach_pid, &classified.event)
            .await
            .map_err(io_other)?
            .or_else(|| Some(Target::Pane(target.clone())));
        let mouse_target = current_target.clone();
        let handled = self
            .dispatch_attached_key_inner(
                &target,
                AttachedKeyDispatch {
                    attach_pid,
                    requester_pid: attach_pid,
                    current_target,
                    mouse_target,
                    key: classified.key,
                    attached_live_input: true,
                },
            )
            .await
            .map_err(io_other)?;
        if !handled {
            self.forward_attached_mouse_event_to_pane(&target, &classified.event)
                .await?;
        }
        Ok(())
    }

    async fn forward_attached_mouse_event_to_pane(
        &self,
        target: &PaneTarget,
        event: &crate::mouse::AttachedMouseEvent,
    ) -> io::Result<bool> {
        let prepared = {
            let state = self.state.lock().await;
            let bytes = encode_mouse_for_target(&state, target, event).map_err(io_other)?;
            if bytes.is_empty() {
                return Ok(false);
            }
            let writes =
                prepare_attached_pane_input_writes(&state, target, &bytes).map_err(io_other)?;
            (writes, bytes)
        };

        for write in prepared.0 {
            write_bytes_to_target_io(write, prepared.1.clone())
                .await
                .map_err(io_other)?;
        }
        Ok(true)
    }

    async fn write_attached_bytes(&self, attach_pid: u32, bytes: &[u8]) -> io::Result<()> {
        {
            let active_attach = self.active_attach.lock().await;
            if active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                !active.can_write
                    || active
                        .flags
                        .contains(super::super::attach_support::ClientFlags::READONLY)
            }) {
                return Ok(());
            }
        }

        let target = self
            .attached_input_target(attach_pid)
            .await
            .map_err(io_other)?;
        let writes = {
            let state = self.state.lock().await;
            prepare_attached_pane_input_writes(&state, &target, bytes).map_err(io_other)?
        };
        for write in writes {
            write_bytes_to_target_io(write, bytes.to_vec())
                .await
                .map_err(io_other)?;
        }
        Ok(())
    }

    pub(crate) async fn flush_attached_pending_escape_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<bool> {
        if pending_input.is_empty() {
            return Ok(false);
        }

        let bytes = std::mem::take(pending_input);
        self.write_attached_bytes(attach_pid, &bytes).await?;
        pending_input.clear();
        Ok(true)
    }

    async fn record_attached_submitted_text(
        &self,
        attach_pid: u32,
        bytes: &[u8],
    ) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let target = self
            .attached_input_target(attach_pid)
            .await
            .map_err(io_other)?;
        let mut state = self.state.lock().await;
        state
            .record_attached_submitted_text(&target, bytes)
            .map_err(io_other)
    }

    pub(in crate::handler) async fn attached_input_target(
        &self,
        attach_pid: u32,
    ) -> Result<PaneTarget, RmuxError> {
        let session_name = self.attached_session_name(attach_pid).await?;
        let state = self.state.lock().await;
        resolve_input_target(&state, None, Some(&session_name))
    }

    pub(crate) async fn attached_session_name(
        &self,
        attach_pid: u32,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .map(|active| active.session_name.clone())
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))
    }

    async fn target_pane_mode(&self, target: &PaneTarget) -> Result<u32, RmuxError> {
        let state = self.state.lock().await;
        let transcript = state.transcript_handle(target)?;
        let mode = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .mode();
        Ok(mode)
    }

    pub(in crate::handler) async fn attached_last_mouse_event(
        &self,
        attach_pid: u32,
    ) -> Option<crate::input_keys::MouseForwardEvent> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .and_then(|active| active.mouse.current_event.as_ref().map(|event| event.raw))
    }

    async fn attached_backspace_byte(&self) -> Option<u8> {
        let state = self.state.lock().await;
        state
            .options
            .resolve(None, OptionName::Backspace)
            .and_then(key_string_lookup_string)
            .and_then(key_code_to_bytes)
            .and_then(|bytes| (bytes.len() == 1).then_some(bytes[0]))
    }

    pub(super) async fn attached_persistent_overlay_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.mode_tree.is_some() || active.overlay.is_some())
    }

    pub(super) async fn restore_mode_tree_overlay_if_active(
        &self,
        attach_pid: u32,
    ) -> Result<bool, RmuxError> {
        let Some((
            session_name,
            render_generation,
            overlay_generation,
            state_id,
            frame,
            control_tx,
        )) = ({
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(false);
            };
            let Some(frame) = active.mode_tree_frame.clone() else {
                return Ok(false);
            };
            if active.mode_tree.is_none() || active.suspended {
                return Ok(false);
            }
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            Some((
                active.session_name.clone(),
                active.render_generation,
                active.overlay_generation,
                active.mode_tree_state_id,
                frame,
                active.control_tx.clone(),
            ))
        })
        else {
            return Ok(false);
        };
        let mut restore_frame = self
            .render_mode_tree_overlay_clear_frame(&session_name)
            .await
            .unwrap_or_default();
        restore_frame.extend(frame);
        let overlay = OverlayFrame::persistent_with_state(
            restore_frame,
            render_generation,
            overlay_generation,
            state_id,
        );
        Ok(control_tx.send(AttachControl::Overlay(overlay)).is_ok())
    }

    async fn render_mode_tree_overlay_clear_frame(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> Option<Vec<u8>> {
        let state = self.state.lock().await;
        let session = state.sessions.session(session_name)?;
        let size = session.window().size();
        let status_on = state
            .options
            .resolve(Some(session.name()), OptionName::Status)
            .map(|value| value != "off")
            .unwrap_or(true);
        let usable_rows = size.rows.saturating_sub(u16::from(status_on));
        if usable_rows == 0 || size.cols == 0 {
            return Some(Vec::new());
        }

        let blank = " ".repeat(usize::from(size.cols));
        let mut frame = Vec::new();
        frame.extend_from_slice(b"\x1b[s\x1b[0m");
        for row in 0..usable_rows {
            frame.extend_from_slice(
                format!("\x1b[{};1H{}", row.saturating_add(1), blank).as_bytes(),
            );
        }
        frame.extend_from_slice(b"\x1b[0m\x1b[u");
        Some(frame)
    }
}

fn is_mouse_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[M") || bytes.starts_with(b"\x1b[<")
}

fn is_enter_key(key: rmux_core::KeyCode) -> bool {
    key_string_lookup_string("Enter")
        .is_some_and(|enter| key_code_lookup_bits(enter) == key_code_lookup_bits(key))
}
