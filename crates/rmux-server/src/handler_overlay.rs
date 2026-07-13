use std::io;

use rmux_core::LifecycleEvent;
use rmux_proto::{RmuxError, TerminalGeometry, TerminalSize};

use super::pane_support::{
    retain_partial_attached_escape_input, strip_bracketed_paste_markers_after_append,
};
use super::prompt_support::{decode_prompt_key, PromptInputEvent};
use super::scripting_support::{QueueCommandAction, QueueExecutionContext};
use super::RequestHandler;
use crate::input_keys::{decode_mouse, MouseDecode};
use crate::key_table::{decode_attached_key, AttachedKeyDecode};
use crate::pane_io::{AttachControl, OverlayFrame};

#[path = "handler_overlay/commands.rs"]
mod commands;
#[path = "handler_overlay/interactions.rs"]
mod interactions;
#[path = "handler_overlay/parse.rs"]
mod parse;
pub(super) use parse::ParsedOverlayCommand;
use parse::{parse_display_menu, parse_display_popup};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachedOverlayInput {
    Consumed,
    Reroute(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedOverlayRoute {
    Menu(u64),
    Popup { id: u64, job_backed: bool },
}
#[path = "handler_overlay/layout.rs"]
mod layout;
#[path = "handler_overlay/menu.rs"]
mod menu;
use menu::MenuOverlayItem;
#[path = "handler_overlay/mouse.rs"]
mod mouse;
use mouse::is_mouse_prefix;
#[path = "handler_overlay/popup_job.rs"]
mod popup_job;
#[path = "handler_overlay/state.rs"]
mod state;
pub(super) use state::{ClientOverlayState, PopupOverlayState};
#[path = "handler_overlay/support.rs"]
mod support;
#[path = "handler_overlay/target.rs"]
mod target;

impl RequestHandler {
    pub(super) fn parse_overlay_queue_command(
        command_name: &str,
        arguments: Vec<String>,
    ) -> Result<Option<ParsedOverlayCommand>, RmuxError> {
        match command_name {
            "display-menu" | "menu" => parse_display_menu(arguments)
                .map(|command| Some(ParsedOverlayCommand::Menu(command))),
            "display-popup" | "popup" => parse_display_popup(arguments)
                .map(|command| Some(ParsedOverlayCommand::Popup(command))),
            _ => Ok(None),
        }
    }

    pub(super) async fn execute_queued_overlay(
        &self,
        requester_pid: u32,
        command: ParsedOverlayCommand,
        context: &QueueExecutionContext,
    ) -> Result<QueueCommandAction, RmuxError> {
        match command {
            ParsedOverlayCommand::Menu(command) => {
                self.execute_queued_display_menu(requester_pid, command, context)
                    .await
            }
            ParsedOverlayCommand::Popup(command) => {
                self.execute_queued_display_popup(requester_pid, command, context)
                    .await
            }
        }
    }

    pub(super) async fn overlay_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.overlay.is_some())
    }

    async fn attached_overlay_route(&self, attach_pid: u32) -> Option<AttachedOverlayRoute> {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
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

    pub(super) async fn handle_attached_overlay_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<AttachedOverlayInput> {
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);

        let Some(overlay_route) = self.attached_overlay_route(attach_pid).await else {
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
                    let last_mouse = self.attached_last_mouse_event(attach_pid).await;
                    match decode_mouse(slice, last_mouse) {
                        MouseDecode::Matched { size, event } => {
                            self.handle_popup_mouse_event(attach_pid, event).await?;
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
                    if self.attached_overlay_route(attach_pid).await != Some(overlay_route) {
                        break;
                    }
                    continue;
                }
                let consumed = match decode_attached_key(slice, backspace) {
                    AttachedKeyDecode::Matched { size, .. } => size,
                    AttachedKeyDecode::Partial => {
                        pending_input.drain(..offset);
                        retain_partial_attached_escape_input(
                            "popup overlay key input",
                            pending_input,
                        )?;
                        return Ok(AttachedOverlayInput::Consumed);
                    }
                    AttachedKeyDecode::Invalid => slice.len(),
                };
                let raw = pending_input[offset..offset + consumed].to_vec();
                if !self.handle_popup_raw_input(attach_pid, &raw).await? {
                    break;
                }
                offset += consumed;
                if self.attached_overlay_route(attach_pid).await != Some(overlay_route) {
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
                let last_mouse = self.attached_last_mouse_event(attach_pid).await;
                match decode_mouse(pending_input, last_mouse) {
                    MouseDecode::Matched { size, event } => {
                        pending_input.drain(..size);
                        self.handle_menu_mouse_event(attach_pid, event)
                            .await
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
                if self.attached_overlay_route(attach_pid).await != Some(overlay_route) {
                    break;
                }
                continue;
            }
            let (event, consumed) = match decode_attached_key(pending_input, backspace) {
                AttachedKeyDecode::Matched { size, key } => {
                    let event = if pending_input.first() == Some(&b'\x1b') {
                        decode_prompt_key(key)
                    } else {
                        super::pane_support::decode_prompt_input_event(&pending_input[..size])
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
                        super::pane_support::decode_prompt_input_event(pending_input)
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
            let handled = self
                .handle_menu_input_event(attach_pid, event)
                .await
                .map_err(io::Error::other)?;
            if !handled || self.attached_overlay_route(attach_pid).await != Some(overlay_route) {
                break;
            }
        }

        if pending_input.is_empty() {
            Ok(AttachedOverlayInput::Consumed)
        } else {
            Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)))
        }
    }

    pub(crate) async fn flush_attached_overlay_escape_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
    ) -> io::Result<AttachedOverlayInput> {
        let Some(overlay_route) = self.attached_overlay_route(attach_pid).await else {
            return Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)));
        };
        if pending_input.first() != Some(&b'\x1b') {
            return Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)));
        }

        pending_input.drain(..1);
        match overlay_route {
            AttachedOverlayRoute::Menu(_) => {
                self.handle_menu_input_event(attach_pid, PromptInputEvent::Escape)
                    .await
                    .map_err(io::Error::other)?;
            }
            AttachedOverlayRoute::Popup { .. } => {
                let _ = self.handle_popup_raw_input(attach_pid, b"\x1b").await?;
            }
        }

        if pending_input.is_empty() {
            Ok(AttachedOverlayInput::Consumed)
        } else {
            Ok(AttachedOverlayInput::Reroute(std::mem::take(pending_input)))
        }
    }

    pub(crate) async fn handle_attached_resize(
        &self,
        attach_pid: u32,
        size: TerminalSize,
    ) -> Result<(), RmuxError> {
        self.handle_attached_resize_geometry(attach_pid, TerminalGeometry::from_size(size))
            .await
    }

    pub(crate) async fn handle_attached_resize_geometry(
        &self,
        attach_pid: u32,
        geometry: TerminalGeometry,
    ) -> Result<(), RmuxError> {
        let size = geometry.size;
        if size.cols == 0 || size.rows == 0 {
            return Ok(());
        }

        let mut close_overlay = false;
        let (resized_session, ignores_size, client_size_changed) = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&attach_pid) else {
                return Ok(());
            };
            let ignores_size = active
                .flags
                .contains(super::attach_support::ClientFlags::IGNORESIZE);
            let client_size_changed = active.client_size != size;
            let geometry_changed = client_size_changed || active.client_pixels != geometry.pixels;
            let size_sequence = if geometry_changed {
                let size_sequence = active_attach.next_size_sequence;
                active_attach.next_size_sequence =
                    active_attach.next_size_sequence.saturating_add(1);
                Some(size_sequence)
            } else {
                None
            };
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("attached client checked above");
            if let Some(size_sequence) = size_sequence {
                active.client_size = size;
                active.client_pixels = geometry.pixels;
                active.size_sequence = size_sequence;
            }
            let session_name = active.session_name.clone();
            if let Some(overlay) = active.overlay.as_mut() {
                match overlay {
                    ClientOverlayState::Menu(menu) => {
                        if size.cols == 0 || size.rows == 0 {
                            close_overlay = true;
                        } else {
                            menu.rect.width = menu.rect.width.min(size.cols);
                            menu.rect.height = menu.rect.height.min(size.rows);
                            menu.rect.x =
                                menu.rect.x.min(size.cols.saturating_sub(menu.rect.width));
                            menu.rect.y =
                                menu.rect.y.min(size.rows.saturating_sub(menu.rect.height));
                        }
                    }
                    ClientOverlayState::Popup(popup) => {
                        if size.cols == 0 || size.rows == 0 {
                            close_overlay = true;
                        } else {
                            popup.rect.width = popup.preferred_width.min(size.cols);
                            popup.rect.height = popup.preferred_height.min(size.rows);
                            popup.rect.x =
                                popup.rect.x.min(size.cols.saturating_sub(popup.rect.width));
                            popup.rect.y = popup
                                .rect
                                .y
                                .min(size.rows.saturating_sub(popup.rect.height));
                            let content_size = popup.content_size();
                            popup
                                .surface
                                .lock()
                                .expect("popup surface")
                                .resize(content_size);
                            if let Some(job) = &popup.job {
                                let _ = job.resize(content_size);
                            }
                            if let Some(menu) = popup.nested_menu.as_mut() {
                                menu.rect.width = menu.rect.width.min(size.cols);
                                menu.rect.height = menu.rect.height.min(size.rows);
                                menu.rect.x =
                                    menu.rect.x.min(size.cols.saturating_sub(menu.rect.width));
                                menu.rect.y =
                                    menu.rect.y.min(size.rows.saturating_sub(menu.rect.height));
                            }
                        }
                    }
                }
            }
            (session_name, ignores_size, client_size_changed)
        };

        if !ignores_size
            && self
                .attached_window_size_policy_for_session(&resized_session)
                .await?
                != super::attach_support::AttachedWindowSizePolicy::Manual
        {
            let mut state = self.state.lock().await;
            state.set_attached_terminal_pixels(&resized_session, geometry.pixels);
        }
        self.reconcile_attached_session_size_and_emit(&resized_session)
            .await?;
        if client_size_changed {
            self.emit(LifecycleEvent::ClientResized {
                session_name: resized_session.clone(),
                client_name: Some(attach_pid.to_string()),
            })
            .await;
        }
        self.refresh_attached_session(&resized_session).await;

        if close_overlay {
            self.clear_interactive_overlay(attach_pid, true).await?;
        } else {
            self.refresh_interactive_overlay_if_active(attach_pid)
                .await?;
        }
        Ok(())
    }

    pub(super) async fn refresh_interactive_overlay_if_active(
        &self,
        attach_pid: u32,
    ) -> Result<(), RmuxError> {
        self.refresh_interactive_overlay_with_expected_identity(attach_pid, None)
            .await
    }

    pub(super) async fn refresh_interactive_overlay_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_interactive_overlay_with_expected_identity(
            attach_pid,
            Some((expected_attach_id, session_name)),
        )
        .await
    }

    async fn refresh_interactive_overlay_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_identity: Option<(u64, &rmux_proto::SessionName)>,
    ) -> Result<(), RmuxError> {
        let (overlay, captured_attach_id, captured_session_name) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if expected_identity.is_some_and(|(expected_attach_id, expected_session_name)| {
                active.id != expected_attach_id
                    || &active.session_name != expected_session_name
                    || active.suspended
                    || active.closing.load(std::sync::atomic::Ordering::SeqCst)
            }) {
                return Err(crate::handler_support::attached_client_required(
                    "refresh-client",
                ));
            }
            let Some(overlay) = active.overlay.clone() else {
                return Ok(());
            };
            (overlay, active.id, active.session_name.clone())
        };

        let frame = overlay.render();
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        if active.id != captured_attach_id
            || active.session_name != captured_session_name
            || active.closing.load(std::sync::atomic::Ordering::SeqCst)
        {
            return expected_identity.map_or(Ok(()), |_| {
                Err(crate::handler_support::attached_client_required(
                    "refresh-client",
                ))
            });
        }
        if expected_identity.is_some_and(|(expected_attach_id, expected_session_name)| {
            active.id != expected_attach_id
                || &active.session_name != expected_session_name
                || active.suspended
        }) {
            return Err(crate::handler_support::attached_client_required(
                "refresh-client",
            ));
        }
        if active
            .overlay
            .as_ref()
            .map(|current| current.id() != overlay.id())
            .unwrap_or(true)
        {
            return Ok(());
        }
        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let delivered = active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::persistent(
                frame,
                active.render_generation,
                active.overlay_generation,
            )));
        if delivered.is_err() && expected_identity.is_some() {
            return Err(crate::handler_support::attached_client_required(
                "refresh-client",
            ));
        }
        Ok(())
    }

    pub(super) async fn clear_interactive_overlay(
        &self,
        attach_pid: u32,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        let (control_tx, render_generation, popup_job) = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            let popup_job = match active.overlay.take() {
                Some(ClientOverlayState::Popup(popup)) if terminate_popup_job => popup.job,
                _ => None,
            };
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            (
                active.control_tx.clone(),
                active.render_generation,
                popup_job,
            )
        };
        if let Some(job) = popup_job {
            job.terminate();
        }
        let overlay_generation = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .map(|active| active.overlay_generation)
                .unwrap_or_default()
        };
        let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::persistent(
            Vec::new(),
            render_generation,
            overlay_generation,
        )));
        Ok(())
    }

    pub(super) async fn popup_reader_tick(
        &self,
        attach_pid: u32,
        popup_id: u64,
    ) -> Result<(), RmuxError> {
        let active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get(&attach_pid) else {
            return Ok(());
        };
        if active
            .overlay
            .as_ref()
            .map(|overlay| overlay.id() != popup_id)
            .unwrap_or(true)
        {
            return Ok(());
        }
        drop(active_attach);
        self.refresh_interactive_overlay_if_active(attach_pid).await
    }

    pub(super) async fn popup_job_finished(
        &self,
        attach_pid: u32,
        popup_id: u64,
        status: i32,
    ) -> Result<(), RmuxError> {
        let should_close = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(());
            };
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
                return Ok(());
            };
            if popup.id != popup_id {
                return Ok(());
            }
            popup.job = None;
            popup.close_on_exit || (popup.close_on_zero_exit && status == 0)
        };
        if should_close {
            self.clear_interactive_overlay(attach_pid, false).await?;
        } else {
            self.refresh_interactive_overlay_if_active(attach_pid)
                .await?;
        }
        Ok(())
    }
}
