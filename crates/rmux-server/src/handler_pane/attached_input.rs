use std::io;
use std::marker::PhantomData;
use std::time::Instant;

use rmux_core::{key_code_lookup_bits, key_code_to_bytes, key_string_lookup_string};
use rmux_proto::{OptionName, PaneTarget, RmuxError, Target};
#[cfg(windows)]
use rmux_pty::WindowsConsoleKeyEvent;

use super::super::{
    prompt_support::{decode_prompt_key, PromptInputEvent},
    RequestHandler,
};
use super::pane_io_encoding::{
    encode_mouse_for_target, prepare_attached_pane_input_writes, prepare_pane_input_write,
    write_attached_bytes_to_target_io, PaneInputLiveness,
};
use super::pane_prompt_input::{
    decode_prompt_input_event, is_extended_key_prefix, prompt_text_prefix_len,
};
use super::{io_other, resolve_input_target, AttachedKeyDispatch};
use crate::client_flags::ClientFlags;
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::overlay_support::AttachedOverlayInput;
use crate::input_keys::{decode_extended_key, decode_mouse, ExtendedKeyDecode, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};
use crate::mouse::{
    classify_mouse_events, layout_for_session, mouse_event_for_pane_passthrough,
    ClassifiedMouseEvent,
};
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::HandlerState;

#[path = "attached_input/bracketed_paste.rs"]
pub(super) mod bracketed_paste;
#[path = "attached_input/kitty_graphics.rs"]
mod kitty_graphics;
#[path = "attached_input/live.rs"]
mod live;
#[path = "attached_input/palette_response.rs"]
mod palette_response;
#[path = "attached_input/retained.rs"]
mod retained;
#[path = "attached_input/synchronized.rs"]
mod synchronized;
#[path = "attached_input/terminal_response.rs"]
mod terminal_response;

pub(in crate::handler) use retained::{
    retain_partial_attached_control_input, retain_partial_attached_escape_input,
};
use synchronized::{
    prepare_attached_bracketed_paste_forwards, prepare_attached_key_forwards,
    write_prepared_attached_pane_forwards,
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
    EncodedKey(PhantomData<&'a ()>),
    #[cfg(windows)]
    WindowsConsoleKey {
        key: WindowsConsoleKeyEvent,
        bytes: &'a [u8],
    },
}

impl RequestHandler {
    async fn with_live_input_session_state<T>(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        mutate: impl FnOnce(&mut HandlerState) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        // Publication of a replacement attach and preparation of pane input
        // are linearized by this lock order.  No lock is retained across the
        // eventual PTY write.
        let mut state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let Some(_active) = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active_session(active, session_name, session_id)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.can_write
                    && !active.flags.contains(ClientFlags::READONLY)
            })
        else {
            return Ok(None);
        };
        ensure_session_identity(&state, session_name, session_id).map_err(io_other)?;
        mutate(&mut state).map(Some)
    }

    async fn with_live_input_state<T>(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        session_id: rmux_proto::SessionId,
        mutate: impl FnOnce(&mut HandlerState) -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        self.with_live_input_session_state(identity, target.session_name(), session_id, mutate)
            .await
    }

    async fn handle_attached_mode_tree_key_or_prefix(
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
            .handle_mode_tree_key_event_for_identity(identity, fallback_event)
            .await
            .map_err(io_other)?;
        Ok(())
    }

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
        if self
            .handle_attached_copy_mode_key_event_for_identity(
                identity,
                target.clone(),
                decode_prompt_key(key),
            )
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

    async fn handle_attached_prompt_input(
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

    async fn handle_attached_mode_tree_input(
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
            if is_extended_key_prefix(slice) {
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

            match decode_attached_key(slice, backspace) {
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
                        .handle_mode_tree_key_event_for_identity(identity, event)
                        .await
                        .map_err(io_other)?;
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

    async fn handle_attached_live_mouse(
        &self,
        identity: ActiveAttachIdentity,
        raw: crate::input_keys::MouseForwardEvent,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        if self
            .attached_client_input_is_read_only_for_identity(identity)
            .await?
        {
            return Ok(());
        }
        let (session_name, session_id) = self
            .attached_session_identity_for_identity(identity)
            .await
            .map_err(io_other)?;
        let mouse_enabled = {
            let state = self.state.lock().await;
            ensure_session_identity(&state, &session_name, session_id).map_err(io_other)?;
            matches!(
                state
                    .options
                    .resolve(Some(&session_name), OptionName::Mouse),
                Some("on")
            )
        };
        if self.mode_tree_active_for_identity(identity).await {
            if mouse_enabled {
                let _ = self
                    .handle_mode_tree_mouse_event_for_identity(identity, raw)
                    .await
                    .map_err(io_other)?;
            }
            return Ok(());
        }
        let attached_count = self
            .attached_count_for_session_identity(&session_name, session_id)
            .await;
        let layout = {
            let state = self.state.lock().await;
            ensure_session_identity(&state, &session_name, session_id).map_err(io_other)?;
            layout_for_session(&state, &session_name, attached_count)
        };
        let Some(layout) = layout else {
            return Ok(());
        };
        if !mouse_enabled {
            let Some(event) = mouse_event_for_pane_passthrough(&layout, raw) else {
                return Ok(());
            };
            let Some(target) = event.pane_target.clone() else {
                return Ok(());
            };
            self.forward_attached_mouse_event_to_pane_for_session_identity(
                identity, session_id, &target, &event,
            )
            .await?;
            return Ok(());
        }
        let (classified, click_deadline) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    identity.matches_active_session(active, &session_name, session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| io_other("attached client disappeared"))?;
            let classified = classify_mouse_events(&mut active.mouse, &layout, raw, Instant::now());
            (classified, active.mouse.click_deadline())
        };
        if let Some(deadline) = click_deadline {
            self.schedule_attached_mouse_click_timer(
                identity,
                session_name.clone(),
                session_id,
                deadline,
            );
        }
        if classified.is_empty() {
            return Ok(());
        }
        for classified in classified {
            self.dispatch_attached_mouse_classified(
                identity,
                &session_name,
                session_id,
                classified,
            )
            .await?;
        }
        Ok(())
    }

    fn schedule_attached_mouse_click_timer(
        &self,
        identity: ActiveAttachIdentity,
        session_name: rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        deadline: Instant,
    ) {
        let handler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            let _ = handler
                .dispatch_expired_attached_mouse_click(identity, session_name, session_id)
                .await;
        });
    }

    async fn dispatch_expired_attached_mouse_click(
        &self,
        identity: ActiveAttachIdentity,
        session_name: rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        let attached_count = self
            .attached_count_for_session_identity(&session_name, session_id)
            .await;
        let layout = {
            let state = self.state.lock().await;
            ensure_session_identity(&state, &session_name, session_id).map_err(io_other)?;
            layout_for_session(&state, &session_name, attached_count)
        };
        let Some(layout) = layout else {
            return Ok(());
        };
        let classified = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(());
            };
            if !identity.matches_active_session(active, &session_name, session_id)
                || active.closing.load(std::sync::atomic::Ordering::SeqCst)
            {
                return Ok(());
            }
            active.mouse.expire_click_timer(Instant::now(), &layout)
        };
        if let Some(classified) = classified {
            self.dispatch_attached_mouse_classified(
                identity,
                &session_name,
                session_id,
                classified,
            )
            .await?;
        }
        Ok(())
    }

    async fn dispatch_attached_mouse_classified(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        classified: ClassifiedMouseEvent,
    ) -> io::Result<()> {
        let attach_pid = identity.attach_pid();
        let target = if let Some(target) = classified.event.pane_target.clone() {
            target
        } else {
            let (target, current_session_id) = self
                .attached_input_target_identity(identity)
                .await
                .map_err(io_other)?;
            if current_session_id != session_id || target.session_name() != session_name {
                return Ok(());
            }
            target
        };
        if target.session_name() != session_name {
            return Ok(());
        }
        let current_target = self
            .attached_mouse_target_for_session_identity(
                identity,
                session_name,
                session_id,
                &classified.event,
            )
            .await
            .map_err(io_other)?
            .or_else(|| Some(Target::Pane(target.clone())));
        let mouse_target = current_target.clone();
        let handled = self
            .dispatch_attached_key_inner(
                &target,
                AttachedKeyDispatch {
                    attach_pid,
                    live_identity: Some(identity),
                    live_session_id: Some(session_id),
                    requester_pid: attach_pid,
                    current_target,
                    mouse_target,
                    mouse_event: Some(classified.event.clone()),
                    key: classified.key,
                    attached_live_input: true,
                },
            )
            .await
            .map_err(io_other)?;
        if !handled {
            self.forward_attached_mouse_event_to_pane_for_session_identity(
                identity,
                session_id,
                &target,
                &classified.event,
            )
            .await?;
        }
        Ok(())
    }

    async fn forward_attached_mouse_event_to_pane_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_id: rmux_proto::SessionId,
        target: &PaneTarget,
        event: &crate::mouse::AttachedMouseEvent,
    ) -> io::Result<bool> {
        let Some(prepared) = self
            .with_live_input_state(identity, target, session_id, |state| {
                let bytes = encode_mouse_for_target(state, target, event).map_err(io_other)?;
                if bytes.is_empty() {
                    return Ok(None);
                }
                let writes =
                    prepare_attached_pane_input_writes(state, target, &bytes).map_err(io_other)?;
                Ok(Some((writes, bytes)))
            })
            .await?
            .flatten()
        else {
            return Ok(false);
        };

        for write in prepared.0 {
            write_attached_bytes_to_target_io(write, prepared.1.clone())
                .await
                .map_err(io_other)?;
        }
        Ok(true)
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

    async fn attached_client_input_is_read_only_for_identity(
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
            .ok_or_else(|| io_other(RmuxError::Server("attached client disappeared".to_owned())))?;
        Ok(!active.can_write || active.flags.contains(ClientFlags::READONLY))
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
        if pending_input.is_empty() {
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

        if pending_input.first() == Some(&b'\x1b')
            && self.prompt_active_for_identity(identity).await
        {
            pending_input.drain(..1);
            let mut deferred_refresh = false;
            self.handle_prompt_event_deferred_refresh_for_identity(
                identity,
                PromptInputEvent::Escape,
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
            pending_input.drain(..1);
            let escape = key_string_lookup_string("Escape")
                .ok_or_else(|| io_other("Escape key is unavailable"))?;
            self.handle_attached_mode_tree_key_or_prefix(
                identity,
                escape,
                PromptInputEvent::Escape,
            )
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
            let target = self
                .attached_input_target_for_identity(identity)
                .await
                .map_err(io_other)?;
            let consumed_by_mode = if self
                .target_is_in_clock_mode(&target)
                .await
                .map_err(io_other)?
            {
                self.exit_clock_mode(&target).await.map_err(io_other)?
            } else {
                self.handle_attached_copy_mode_key_event_for_identity(
                    identity,
                    target,
                    PromptInputEvent::Escape,
                )
                .await
                .map_err(io_other)?
            };
            if consumed_by_mode {
                return if let Some(remaining) = bytes.get(1..).filter(|bytes| !bytes.is_empty()) {
                    self.handle_attached_live_input_inner_for_identity(
                        identity,
                        pending_input,
                        remaining,
                    )
                    .await
                } else {
                    Ok(false)
                };
            }
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

    pub(in crate::handler) async fn attached_input_target(
        &self,
        attach_pid: u32,
    ) -> Result<PaneTarget, RmuxError> {
        let session_name = self.attached_session_name(attach_pid).await?;
        let state = self.state.lock().await;
        resolve_input_target(&state, None, Some(&session_name))
    }

    pub(in crate::handler) async fn attached_input_target_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<PaneTarget, RmuxError> {
        self.attached_input_target_identity(identity)
            .await
            .map(|(target, _)| target)
    }

    async fn attached_input_target_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Result<(PaneTarget, rmux_proto::SessionId), RmuxError> {
        let (session_name, session_id) = self
            .attached_session_identity_for_identity(identity)
            .await?;
        let state = self.state.lock().await;
        ensure_session_identity(&state, &session_name, session_id)?;
        resolve_input_target(&state, None, Some(&session_name)).map(|target| (target, session_id))
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

    pub(crate) async fn attached_session_name_for_identity(
        &self,
        identity: crate::handler::attach_support::ActiveAttachIdentity,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        self.attached_session_identity_for_identity(identity)
            .await
            .map(|(session_name, _)| session_name)
    }

    pub(crate) async fn attached_session_identity_for_identity(
        &self,
        identity: crate::handler::attach_support::ActiveAttachIdentity,
    ) -> Result<(rmux_proto::SessionName, rmux_proto::SessionId), RmuxError> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .map(|active| (active.session_name.clone(), active.session_id))
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

    #[cfg(test)]
    pub(crate) async fn start_attached_input_capture_for_test(&self, target: &PaneTarget) {
        let state = self.state.lock().await;
        state.start_pane_input_capture_for_test(target);
    }

    #[cfg(test)]
    pub(crate) async fn attached_input_capture_for_test(
        &self,
        target: &PaneTarget,
    ) -> Option<Vec<u8>> {
        let state = self.state.lock().await;
        state.pane_input_capture_for_test(target)
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

    pub(in crate::handler) async fn attached_last_mouse_event_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> Option<crate::input_keys::MouseForwardEvent> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .and_then(|active| active.mouse.current_event.as_ref().map(|event| event.raw))
    }

    pub(in crate::handler) async fn attached_backspace_byte(&self) -> Option<u8> {
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

fn is_mouse_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[M") || bytes.starts_with(b"\x1b[<")
}

fn is_enter_key(key: rmux_core::KeyCode) -> bool {
    key_string_lookup_string("Enter")
        .is_some_and(|enter| key_code_lookup_bits(enter) == key_code_lookup_bits(key))
}
