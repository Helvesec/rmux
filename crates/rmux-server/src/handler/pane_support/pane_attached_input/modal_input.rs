use std::io;

use rmux_proto::{RmuxError, Target};

use super::super::pane_prompt_input::{
    decode_prompt_input_event, is_extended_key_prefix, prompt_text_prefix_len,
};
use super::{
    bracketed_paste, is_mouse_prefix, retain_partial_attached_escape_input, user_key_input,
};
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::mode_tree_support::ModeTreeInputError;
use crate::handler::pane_support::{io_other, AttachedKeyDispatch};
use crate::handler::prompt_support::{decode_prompt_key, PromptInputEvent};
use crate::handler::RequestHandler;
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};
use crate::pane_io::{AttachControl, OverlayFrame};

impl RequestHandler {
    pub(super) async fn handle_attached_mode_tree_key_or_prefix(
        &self,
        identity: ActiveAttachIdentity,
        key: rmux_core::KeyCode,
        fallback_event: PromptInputEvent,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        let (target, session_id) = self
            .attached_input_target_identity(identity)
            .await
            .map_err(io_other)?;
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
            return Ok(());
        }

        let _ = self
            .handle_attached_mode_tree_key_event(identity, fallback_event)
            .await?;
        Ok(())
    }

    pub(super) async fn handle_attached_mode_tree_key_event(
        &self,
        identity: ActiveAttachIdentity,
        event: PromptInputEvent,
    ) -> io::Result<bool> {
        match self
            .handle_mode_tree_key_event_for_identity(identity, event)
            .await
        {
            Ok(handled) => Ok(handled),
            Err(ModeTreeInputError::Fatal(error)) => Err(io_other(error)),
            Err(ModeTreeInputError::UserCommandAfterModeExit(error)) => {
                let session_name = self
                    .attached_session_name_for_identity(identity)
                    .await
                    .map_err(io_other)?;
                self.report_attached_command_error(&session_name, identity.attach_pid(), &error)
                    .await;
                Ok(true)
            }
        }
    }

    pub(super) async fn handle_attached_prompt_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        // Overlays that decode input as keys (the command prompt here,
        // mode-tree, display-panes, no-job popups) treat a paste as literal
        // text. Strip embedded bracketed-paste markers before decoding —
        // otherwise the leading ESC of ESC[200~ cancels the overlay and the
        // pasted body leaks to the pane's shell. On Windows the attach client
        // wraps a console-input burst in these markers (issue #92); on Unix a
        // real terminal paste into an ?2004h attach hits the same class, so
        // the strip runs on every platform. Scrub the CONCATENATED buffer so
        // a marker whose bytes straddle the pending_input / bytes seam
        // (delivered across two socket reads or attach-input frames) still
        // collapses to nothing.
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        bracketed_paste::strip_bracketed_paste_markers_after_append(pending_input, new_input_at);
        let mut deferred_refresh = false;
        let mut offset = 0;
        let mut try_batched_text = true;

        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            if try_batched_text {
                let text_len = prompt_text_prefix_len(slice);
                if text_len > 0 {
                    let text = std::str::from_utf8(&slice[..text_len])
                        .expect("prompt text prefix is valid UTF-8");
                    match self
                        .try_handle_prompt_text_deferred_refresh_for_identity(
                            identity,
                            text,
                            &mut deferred_refresh,
                        )
                        .await
                    {
                        Ok(true) => {
                            offset += text_len;
                            continue;
                        }
                        Ok(false) => try_batched_text = false,
                        Err(error) => {
                            pending_input.drain(..offset);
                            return Err(io_other(error));
                        }
                    }
                }
            }

            let slice = &pending_input[offset..];
            let Some((event, consumed)) = decode_prompt_input_event(slice) else {
                pending_input.drain(..offset);
                if deferred_refresh {
                    self.flush_attached_prompt_refresh_for_identity(identity)
                        .await
                        .map_err(io_other)?;
                }
                retain_partial_attached_escape_input("prompt input", pending_input)?;
                return Ok(None);
            };
            offset += consumed;
            if let Err(error) = self
                .handle_prompt_event_deferred_refresh_for_identity(
                    identity,
                    event,
                    &mut deferred_refresh,
                )
                .await
            {
                pending_input.drain(..offset);
                return Err(io_other(error));
            }
            if !self.prompt_active_for_identity(identity).await {
                break;
            }
        }

        pending_input.drain(..offset);
        if deferred_refresh && self.prompt_active_for_identity(identity).await {
            self.flush_attached_prompt_refresh_for_identity(identity)
                .await
                .map_err(io_other)?;
        }

        Ok((!pending_input.is_empty()).then(|| std::mem::take(pending_input)))
    }

    pub(super) async fn handle_attached_mode_tree_input(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        // See `handle_attached_prompt_input` for the paste-marker strip
        // rationale — mode-tree also decodes input as keys. Strip the
        // concatenated buffer so a marker straddling pending_input / bytes
        // still collapses.
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        bracketed_paste::strip_bracketed_paste_markers_after_append(pending_input, new_input_at);
        let backspace = self.attached_backspace_byte().await;
        let mut offset = 0;

        let user_keys = self.configured_user_keys().await;
        while offset < pending_input.len() {
            let slice = &pending_input[offset..];
            if is_mouse_prefix(slice) {
                let last_mouse = self.attached_last_mouse_event_for_identity(identity).await;
                match decode_mouse(slice, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        let _ = self
                            .handle_mode_tree_mouse_event_for_identity(identity, event)
                            .await
                            .map_err(io_other)?;
                        offset += size;
                    }
                    MouseDecode::Discard { size } => {
                        offset += size;
                    }
                    MouseDecode::Partial | MouseDecode::Overlong => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input("mode-tree mouse", pending_input)?;
                        return Ok(None);
                    }
                    MouseDecode::Invalid => {
                        offset += 1;
                    }
                }
                if self.prompt_active_for_identity(identity).await
                    || !self.mode_tree_active_for_identity(identity).await
                {
                    break;
                }
                continue;
            }
            let user_key = user_key_input::decode_user_key(slice, &user_keys);
            if user_key.is_none() && is_extended_key_prefix(slice) {
                match decode_extended_key(slice, backspace) {
                    ExtendedKeyDecode::Matched { size, key } => {
                        self.handle_attached_mode_tree_key_or_prefix(
                            identity,
                            key,
                            decode_prompt_key(key),
                        )
                        .await?;
                        offset += size;
                        if self.prompt_active_for_identity(identity).await
                            || !self.mode_tree_active_for_identity(identity).await
                        {
                            break;
                        }
                        continue;
                    }
                    ExtendedKeyDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input(
                            "mode-tree extended key",
                            pending_input,
                        )?;
                        return Ok(None);
                    }
                    ExtendedKeyDecode::Invalid => {}
                }
            }

            match user_key.unwrap_or_else(|| decode_attached_key(slice, backspace)) {
                AttachedKeyDecode::Matched { size, key } => {
                    let fallback_event = decode_prompt_input_event(slice)
                        .filter(|(_, consumed)| *consumed == size)
                        .map(|(event, _)| event)
                        .unwrap_or_else(|| decode_prompt_key(key));
                    self.handle_attached_mode_tree_key_or_prefix(identity, key, fallback_event)
                        .await?;
                    offset += size;
                }
                AttachedKeyDecode::Partial => {
                    pending_input.drain(..offset);
                    retain_partial_attached_escape_input("mode-tree attached key", pending_input)?;
                    return Ok(None);
                }
                AttachedKeyDecode::Invalid => {
                    let Some((event, consumed)) = decode_prompt_input_event(slice) else {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input(
                            "mode-tree prompt input",
                            pending_input,
                        )?;
                        return Ok(None);
                    };
                    offset += consumed;
                    let _ = self
                        .handle_attached_mode_tree_key_event(identity, event)
                        .await?;
                }
            }
            if self.prompt_active_for_identity(identity).await
                || !self.mode_tree_active_for_identity(identity).await
            {
                break;
            }
        }

        pending_input.drain(..offset);
        Ok((!pending_input.is_empty()).then(|| std::mem::take(pending_input)))
    }

    pub(in crate::handler::pane_support) async fn attached_persistent_overlay_active(
        &self,
        attach_pid: u32,
    ) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.mode_tree.is_some() || active.overlay.is_some())
    }

    pub(in crate::handler::pane_support) async fn restore_mode_tree_overlay_if_active(
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
            if active.mode_tree.is_none() || active.overlay.is_some() || active.suspended {
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
        let geometry = crate::renderer::StatusGeometry::for_session(session, &state.options);
        let usable_rows = geometry.content_rows();
        if usable_rows == 0 || size.cols == 0 {
            return Some(Vec::new());
        }

        let blank = " ".repeat(usize::from(size.cols));
        let mut frame = Vec::new();
        frame.extend_from_slice(b"\x1b[s\x1b[0m");
        for row in 0..usable_rows {
            frame.extend_from_slice(
                format!(
                    "\x1b[{};1H{}",
                    geometry
                        .content_y_offset()
                        .saturating_add(row)
                        .saturating_add(1),
                    blank
                )
                .as_bytes(),
            );
        }
        frame.extend_from_slice(b"\x1b[0m\x1b[u");
        Some(frame)
    }

    #[cfg(test)]
    pub(crate) async fn mode_tree_overlay_clear_frame_for_test(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> Option<Vec<u8>> {
        self.render_mode_tree_overlay_clear_frame(session_name)
            .await
    }
}
