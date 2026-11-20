use std::io;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::pane_support::{
    retain_partial_attached_escape_input, strip_bracketed_paste_markers_after_append,
};
use super::super::prompt_support::{decode_prompt_key, PromptInputEvent};
use super::super::RequestHandler;
use super::mouse::is_mouse_prefix;
use super::{AttachedOverlayInput, ClientOverlayState};
use crate::input_keys::{decode_mouse, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedOverlayRoute {
    Menu(u64),
    Popup { id: u64, job_backed: bool },
}

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) async fn overlay_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.overlay.is_some())
    }

    pub(in crate::handler) async fn overlay_active_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.overlay.is_some()
            })
    }

    async fn attached_overlay_route_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
    ) -> Option<AttachedOverlayRoute> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
            .and_then(|active| active.overlay.as_ref())
            .map(|overlay| match overlay {
                ClientOverlayState::Menu(menu) => AttachedOverlayRoute::Menu(menu.id),
                ClientOverlayState::Popup(popup) => popup.nested_menu.as_ref().map_or_else(
                    || AttachedOverlayRoute::Popup {
                        id: popup.id,
                        job_backed: !popup.no_job && popup.job.is_some(),
                    },
                    |menu| AttachedOverlayRoute::Menu(menu.id),
                ),
            })
    }

    #[cfg(test)]
    pub(in crate::handler) async fn handle_attached_overlay_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<AttachedOverlayInput> {
        self.handle_attached_overlay_input_with_identity(attach_pid, None, pending_input, bytes)
            .await
    }

    pub(in crate::handler) async fn handle_attached_overlay_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<AttachedOverlayInput> {
        self.handle_attached_overlay_input_with_identity(
            identity.attach_pid(),
            Some(identity),
            pending_input,
            bytes,
        )
        .await
    }

    async fn handle_attached_overlay_input_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<AttachedOverlayInput> {
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);

        let Some(overlay_route) = self
            .attached_overlay_route_with_identity(attach_pid, identity)
            .await
        else {
            return Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)));
        };

        // Menus and no-job popups decode bytes as keys. Strip a complete
        // bracketed-paste envelope from the concatenated buffer before the
        // leading ESC can close the overlay and reroute the body to the pane.
        // Job-backed popups keep raw bytes for the child process's ?2004h path.
        if matches!(
            overlay_route,
            AttachedOverlayRoute::Menu(_)
                | AttachedOverlayRoute::Popup {
                    job_backed: false,
                    ..
                }
        ) {
            strip_bracketed_paste_markers_after_append(pending_input, new_input_at);
        }

        if matches!(overlay_route, AttachedOverlayRoute::Popup { .. }) {
            let backspace = self.attached_backspace_byte().await;
            let mut offset = 0;
            while offset < pending_input.len() {
                let slice = &pending_input[offset..];
                if is_mouse_prefix(slice) {
                    match self.decode_popup_mouse(attach_pid, identity, slice).await {
                        MouseDecode::Matched { size, event } => {
                            match identity {
                                Some(identity) => {
                                    self.handle_popup_mouse_event_for_identity(identity, event)
                                        .await?
                                }
                                None => self.handle_popup_mouse_event(attach_pid, event).await?,
                            }
                            offset += size;
                        }
                        MouseDecode::Discard { size } => offset += size,
                        MouseDecode::Partial | MouseDecode::Overlong => {
                            pending_input.drain(..offset);
                            retain_partial_attached_escape_input(
                                "popup overlay mouse",
                                pending_input,
                            )?;
                            return Ok(AttachedOverlayInput::Consumed);
                        }
                        MouseDecode::Invalid => offset += 1,
                    }
                    if self
                        .attached_overlay_route_with_identity(attach_pid, identity)
                        .await
                        != Some(overlay_route)
                    {
                        break;
                    }
                    continue;
                }
                let (consumed, key) = match decode_attached_key(slice, backspace) {
                    AttachedKeyDecode::Matched { size, key } => (size, key),
                    AttachedKeyDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input(
                            "popup overlay key input",
                            pending_input,
                        )?;
                        return Ok(AttachedOverlayInput::Consumed);
                    }
                    AttachedKeyDecode::Invalid => (slice.len(), rmux_core::KEYC_UNKNOWN),
                };
                let raw = pending_input[offset..offset + consumed].to_vec();
                let handled = match identity {
                    Some(identity) => {
                        self.handle_popup_key_input_for_identity(identity, key, &raw)
                            .await?
                    }
                    None => self.handle_popup_key_input(attach_pid, key, &raw).await?,
                };
                if !handled {
                    break;
                }
                offset += consumed;
                if self
                    .attached_overlay_route_with_identity(attach_pid, identity)
                    .await
                    != Some(overlay_route)
                {
                    break;
                }
            }
            pending_input.drain(..offset);
            return if pending_input.is_empty() {
                Ok(AttachedOverlayInput::Consumed)
            } else {
                Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)))
            };
        }

        let backspace = self.attached_backspace_byte().await;
        loop {
            if is_mouse_prefix(pending_input) {
                let last_mouse = match identity {
                    Some(identity) => self.attached_last_mouse_event_for_identity(identity).await,
                    None => self.attached_last_mouse_event(attach_pid).await,
                };
                match decode_mouse(pending_input, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        pending_input.drain(..size);
                        match identity {
                            Some(identity) => {
                                self.handle_menu_mouse_event_for_identity(identity, event)
                                    .await
                            }
                            None => self.handle_menu_mouse_event(attach_pid, event).await,
                        }
                        .map_err(io::Error::other)?;
                    }
                    MouseDecode::Discard { size } => {
                        pending_input.drain(..size);
                    }
                    MouseDecode::Partial | MouseDecode::Overlong => {
                        retain_partial_attached_escape_input("menu overlay mouse", pending_input)?;
                        return Ok(AttachedOverlayInput::Consumed);
                    }
                    MouseDecode::Invalid => {
                        pending_input.drain(..1);
                    }
                }
                if self
                    .attached_overlay_route_with_identity(attach_pid, identity)
                    .await
                    != Some(overlay_route)
                {
                    break;
                }
                continue;
            }
            let (event, consumed) = match decode_attached_key(pending_input, backspace) {
                AttachedKeyDecode::Matched { size, key } => {
                    let event = if pending_input.first() == Some(&b'\x1b') {
                        decode_prompt_key(key)
                    } else {
                        crate::handler::pane_support::decode_prompt_input_event(
                            &pending_input[..size],
                        )
                        .map(|(event, _)| event)
                        .unwrap_or_else(|| decode_prompt_key(key))
                    };
                    (event, size)
                }
                AttachedKeyDecode::Partial => {
                    retain_partial_attached_escape_input(
                        "menu overlay prompt input",
                        pending_input,
                    )?;
                    return Ok(AttachedOverlayInput::Consumed);
                }
                AttachedKeyDecode::Invalid => {
                    let Some(decoded) =
                        crate::handler::pane_support::decode_prompt_input_event(pending_input)
                    else {
                        retain_partial_attached_escape_input(
                            "menu overlay prompt input",
                            pending_input,
                        )?;
                        return Ok(AttachedOverlayInput::Consumed);
                    };
                    decoded
                }
            };
            pending_input.drain(..consumed);
            let handled = match identity {
                Some(identity) => {
                    self.handle_menu_input_event_for_identity(identity, event)
                        .await
                }
                None => self.handle_menu_input_event(attach_pid, event).await,
            }
            .map_err(io::Error::other)?;
            if !handled
                || self
                    .attached_overlay_route_with_identity(attach_pid, identity)
                    .await
                    != Some(overlay_route)
            {
                break;
            }
        }

        if pending_input.is_empty() {
            Ok(AttachedOverlayInput::Consumed)
        } else {
            Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)))
        }
    }

    pub(crate) async fn flush_attached_overlay_escape_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<AttachedOverlayInput> {
        self.flush_attached_overlay_escape_input_with_identity(
            identity.attach_pid(),
            Some(identity),
            pending_input,
        )
        .await
    }

    async fn flush_attached_overlay_escape_input_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<AttachedOverlayInput> {
        let Some(overlay_route) = self
            .attached_overlay_route_with_identity(attach_pid, identity)
            .await
        else {
            return Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)));
        };
        if pending_input.first() != Some(&b'\x1b') {
            return Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)));
        }

        pending_input.drain(..1);
        match overlay_route {
            AttachedOverlayRoute::Menu(_) => {
                match identity {
                    Some(identity) => {
                        self.handle_menu_input_event_for_identity(
                            identity,
                            PromptInputEvent::Escape,
                        )
                        .await
                    }
                    None => {
                        self.handle_menu_input_event(attach_pid, PromptInputEvent::Escape)
                            .await
                    }
                }
                .map_err(io::Error::other)?;
            }
            AttachedOverlayRoute::Popup { .. } => {
                let _ = match identity {
                    Some(identity) => {
                        self.handle_popup_key_input_for_identity(
                            identity,
                            rmux_core::KeyCode::from(b'\x1b'),
                            b"\x1b",
                        )
                        .await?
                    }
                    None => {
                        self.handle_popup_key_input(
                            attach_pid,
                            rmux_core::KeyCode::from(b'\x1b'),
                            b"\x1b",
                        )
                        .await?
                    }
                };
            }
        }

        if pending_input.is_empty() {
            Ok(AttachedOverlayInput::Consumed)
        } else {
            Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)))
        }
    }
}
