use std::io;

use rmux_core::KeyCode;
use rmux_proto::RmuxError;

use super::super::prompt_support::PromptInputEvent;
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use super::layout::{menu_option_styles, menu_width, target_window_index};
use super::menu::{
    menu_handle_event, menu_handle_mouse, popup_menu_items, MenuOutcome, MenuOverlayState,
    OverlayMenuAction,
};
use super::mouse::{popup_handle_mouse, PopupMouseOutcome};
use super::state::ClientOverlayState;
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler_support::attached_client_required;
use crate::input_keys::{encode_mouse_event, MouseForwardEvent};
use crate::renderer::OverlayRect;

impl RequestHandler {
    pub(super) async fn handle_menu_input_event(
        &self,
        attach_pid: u32,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_menu_input_event_with_identity(attach_pid, None, event)
            .await
    }

    pub(super) async fn handle_menu_input_event_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        self.handle_menu_input_event_with_identity(identity.attach_pid(), Some(identity), event)
            .await
    }

    async fn handle_menu_input_event_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        let status = self.overlay_action_is_current(attach_pid, identity).await?;
        if !status.is_current() {
            return Ok(status.was_retired());
        }
        let outcome = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            match active.overlay.as_mut() {
                Some(ClientOverlayState::Menu(menu)) => menu_handle_event(menu, event),
                Some(ClientOverlayState::Popup(popup)) => {
                    let Some(menu) = popup.nested_menu.as_mut() else {
                        return Ok(false);
                    };
                    menu_handle_event(menu, event)
                }
                None => return Ok(false),
            }
        };

        self.apply_menu_outcome(attach_pid, identity, outcome)
            .await?;
        Ok(true)
    }

    pub(super) async fn handle_menu_mouse_event(
        &self,
        attach_pid: u32,
        raw: MouseForwardEvent,
    ) -> Result<(), RmuxError> {
        self.handle_menu_mouse_event_with_identity(attach_pid, None, raw)
            .await
    }

    pub(super) async fn handle_menu_mouse_event_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        raw: MouseForwardEvent,
    ) -> Result<(), RmuxError> {
        self.handle_menu_mouse_event_with_identity(identity.attach_pid(), Some(identity), raw)
            .await
    }

    async fn handle_menu_mouse_event_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        raw: MouseForwardEvent,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(attach_pid, identity)
            .await?
            .is_current()
        {
            return Ok(());
        }
        let outcome = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            match active.overlay.as_mut() {
                Some(ClientOverlayState::Menu(menu)) => menu_handle_mouse(menu, raw),
                Some(ClientOverlayState::Popup(popup)) => {
                    let Some(menu) = popup.nested_menu.as_mut() else {
                        return Ok(());
                    };
                    menu_handle_mouse(menu, raw)
                }
                None => return Ok(()),
            }
        };

        self.apply_menu_outcome(attach_pid, identity, outcome).await
    }

    async fn apply_menu_outcome(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        outcome: MenuOutcome,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(attach_pid, identity)
            .await?
            .is_current()
        {
            return Ok(());
        }
        match outcome {
            MenuOutcome::Stay => {}
            MenuOutcome::Redraw => {
                self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                    .await?;
            }
            MenuOutcome::Close => {
                let mut clear_root = false;
                {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach
                        .by_pid
                        .get_mut(&attach_pid)
                        .filter(|active| {
                            identity.is_none_or(|identity| identity.matches_active(active))
                        })
                        .ok_or_else(|| {
                            RmuxError::Server("attached client disappeared".to_owned())
                        })?;
                    match active.overlay.as_mut() {
                        Some(ClientOverlayState::Menu(_)) => clear_root = true,
                        Some(ClientOverlayState::Popup(popup)) => popup.nested_menu = None,
                        None => {}
                    }
                }
                if clear_root {
                    self.clear_interactive_overlay_for_optional_identity(
                        attach_pid, identity, true,
                    )
                    .await?;
                } else {
                    self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                        .await?;
                }
            }
            MenuOutcome::Execute(action) => {
                let (origin, target, command_context) = {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach
                        .by_pid
                        .get_mut(&attach_pid)
                        .filter(|active| {
                            identity.is_none_or(|identity| identity.matches_active(active))
                        })
                        .ok_or_else(|| {
                            RmuxError::Server("attached client disappeared".to_owned())
                        })?;
                    match active.overlay.as_mut() {
                        Some(ClientOverlayState::Menu(menu)) => {
                            let target = menu.current_target.clone();
                            let origin = menu.origin.clone();
                            let command_context = menu.command_context.clone();
                            active.overlay = None;
                            (origin, target, command_context)
                        }
                        Some(ClientOverlayState::Popup(popup)) => {
                            let Some(menu) = popup.nested_menu.take() else {
                                return Ok(());
                            };
                            (menu.origin, menu.current_target, menu.command_context)
                        }
                        None => return Ok(()),
                    }
                };
                let requester_pid = origin.requester_pid();
                let _access = self.begin_requester_origin_access(&origin);
                match action {
                    OverlayMenuAction::Command(command) => {
                        self.refresh_interactive_overlay_for_optional_identity(
                            attach_pid, identity,
                        )
                        .await
                        .ok();
                        let parsed = self.parse_command_string_one_group(&command).await?;
                        let _ = self
                            .execute_parsed_commands(
                                requester_pid,
                                parsed,
                                command_context.with_current_target(Some(target)),
                            )
                            .await?;
                    }
                    OverlayMenuAction::Popup(action) => {
                        self.apply_popup_menu_action(attach_pid, identity, action)
                            .await?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) async fn handle_popup_key_input(
        &self,
        attach_pid: u32,
        key: KeyCode,
        bytes: &[u8],
    ) -> io::Result<bool> {
        self.handle_popup_key_input_with_identity(attach_pid, None, key, bytes)
            .await
    }

    pub(super) async fn handle_popup_key_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        key: KeyCode,
        bytes: &[u8],
    ) -> io::Result<bool> {
        self.handle_popup_key_input_with_identity(identity.attach_pid(), Some(identity), key, bytes)
            .await
    }

    async fn handle_popup_key_input_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        key: KeyCode,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let status = self
            .overlay_action_is_current(attach_pid, identity)
            .await
            .map_err(io::Error::other)?;
        if !status.is_current() {
            return Ok(status.was_retired());
        }
        let popup = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .and_then(|active| match active.overlay.as_ref() {
                    Some(ClientOverlayState::Popup(popup)) => Some(popup.clone()),
                    _ => None,
                })
        };
        let Some(popup) = popup else {
            return Ok(false);
        };

        if popup.nested_menu.is_some() {
            return Ok(false);
        }

        // A no-job popup consumes bytes as keys (Escape closes, close_any_key
        // closes on any input). A bracketed-paste envelope arriving here would
        // otherwise let the leading ESC close the popup and leak the body to
        // the underlying pane — so scrub markers when we're in the key-decoding
        // mode. A job-backed popup forwards bytes verbatim to the child
        // process (which handles its own ?2004h), so keep those raw.
        let scrubbed = if popup.no_job || popup.job.is_none() {
            Some(crate::handler::pane_support::strip_bracketed_paste_markers(
                bytes,
            ))
        } else {
            None
        };
        let bytes: &[u8] = scrubbed.as_deref().unwrap_or(bytes);

        if bytes.is_empty() {
            return Ok(true);
        }
        if popup.scrollable_text.is_some() {
            return self
                .handle_scrollable_popup_key_input(attach_pid, identity, popup.id, key)
                .await;
        }
        if (bytes == b"\x1b" || bytes == b"\x03")
            && ((!popup.close_on_exit && !popup.close_on_zero_exit) || popup.no_job)
        {
            self.clear_interactive_overlay_for_optional_identity(attach_pid, identity, true)
                .await
                .map_err(io::Error::other)?;
            return Ok(true);
        }
        if popup.no_job && popup.close_any_key {
            self.clear_interactive_overlay_for_optional_identity(attach_pid, identity, true)
                .await
                .map_err(io::Error::other)?;
            return Ok(true);
        }
        if popup.job.is_some() {
            let receipt = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&attach_pid)
                    .filter(|active| {
                        identity.is_none_or(|identity| identity.matches_active(active))
                    })
                    .and_then(|active| match active.overlay.as_ref() {
                        Some(ClientOverlayState::Popup(current)) if current.id == popup.id => {
                            current.job.as_ref()
                        }
                        _ => None,
                    })
                    .map(|job| job.enqueue_write(bytes))
                    .transpose()?
            };
            if let Some(receipt) = receipt {
                if receipt.wait().await.is_err() {
                    // A synchronous ConPTY write can remain blocked when the
                    // popup child stops reading. Retire only the popup whose
                    // write failed so a concurrently installed replacement is
                    // never cleared, and keep the attached client alive.
                    self.clear_interactive_overlay_for_optional_identity_and_id(
                        attach_pid, identity, popup.id, true,
                    )
                    .await
                    .map_err(io::Error::other)?;
                }
            }
        }
        Ok(true)
    }

    pub(super) async fn handle_popup_mouse_event(
        &self,
        attach_pid: u32,
        raw: MouseForwardEvent,
    ) -> io::Result<()> {
        self.handle_popup_mouse_event_with_identity(attach_pid, None, raw)
            .await
    }

    pub(super) async fn handle_popup_mouse_event_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        raw: MouseForwardEvent,
    ) -> io::Result<()> {
        self.handle_popup_mouse_event_with_identity(identity.attach_pid(), Some(identity), raw)
            .await
    }

    async fn handle_popup_mouse_event_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        raw: MouseForwardEvent,
    ) -> io::Result<()> {
        if !self
            .overlay_action_is_current(attach_pid, identity)
            .await
            .map_err(io::Error::other)?
            .is_current()
        {
            return Ok(());
        }
        let nested_menu_active = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .and_then(|active| active.overlay.as_ref())
                .is_some_and(|overlay| {
                    matches!(overlay, ClientOverlayState::Popup(popup) if popup.nested_menu.is_some())
                })
        };
        if nested_menu_active {
            match identity {
                Some(identity) => {
                    self.handle_menu_mouse_event_for_identity(identity, raw)
                        .await
                }
                None => self.handle_menu_mouse_event(attach_pid, raw).await,
            }
            .map_err(io::Error::other)?;
            return Ok(());
        }
        let outcome = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .ok_or_else(|| io::Error::other("attached client disappeared"))?;
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
                return Ok(());
            };
            popup_handle_mouse(popup.as_mut(), active.client_size, raw)
        };

        match outcome {
            PopupMouseOutcome::Ignore => {}
            PopupMouseOutcome::Redraw { resize } => {
                if let Some(receipt) = resize {
                    let _ = receipt.wait().await;
                }
                self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
                    .await
                    .map_err(io::Error::other)?;
            }
            PopupMouseOutcome::Forward { mode, event, x, y } => {
                let encoded = encode_mouse_event(mode, &event, x, y);
                let popup_write = {
                    let active_attach = self.active_attach.lock().await;
                    active_attach
                        .by_pid
                        .get(&attach_pid)
                        .filter(|active| {
                            identity.is_none_or(|identity| identity.matches_active(active))
                        })
                        .and_then(|active| active.overlay.as_ref())
                        .and_then(|overlay| match overlay {
                            ClientOverlayState::Popup(popup) => popup.job.as_ref(),
                            ClientOverlayState::Menu(_) => None,
                        })
                        .and_then(|job| {
                            encoded
                                .as_deref()
                                .and_then(|bytes| job.enqueue_write(bytes).ok())
                        })
                };
                if let Some(receipt) = popup_write {
                    let _ = receipt.wait().await;
                }
            }
            PopupMouseOutcome::OpenMenu { x, y } => {
                self.open_popup_internal_menu(attach_pid, identity, x, y)
                    .await
                    .map_err(io::Error::other)?;
            }
        }
        Ok(())
    }

    async fn open_popup_internal_menu(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        x: u16,
        y: u16,
    ) -> Result<(), RmuxError> {
        if !self
            .overlay_action_is_current(attach_pid, identity)
            .await?
            .is_current()
        {
            return Ok(());
        }
        let (client_size, popup_id, menu_client_identity) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .ok_or_else(|| attached_client_required("display-menu"))?;
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
                return Ok(());
            };
            (active.client_size, popup.id, active.identity(attach_pid))
        };
        let origin = self.capture_requester_origin(attach_pid).await;
        let menu_target = self
            .resolve_overlay_target_for_identity(menu_client_identity, None, None)
            .await?;
        let menu_identity = {
            let mut state = self.state.lock().await;
            super::identity::OverlayIdentity::capture(
                &mut state,
                menu_client_identity,
                menu_target.clone(),
            )?
        };
        let state = self.state.lock().await;
        let items = popup_menu_items(&state);
        let title = String::new();
        let width = menu_width(&title, &items).saturating_add(4).max(4);
        let height = u16::try_from(items.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .max(2);
        let rect = OverlayRect {
            x: x.min(client_size.cols.saturating_sub(width.min(client_size.cols))),
            y: y.min(
                client_size
                    .rows
                    .saturating_sub(height.min(client_size.rows)),
            ),
            width: width.min(client_size.cols.max(1)),
            height: height.min(client_size.rows.max(1)),
        };
        let options = menu_option_styles(
            &state,
            menu_target.session_name(),
            target_window_index(&menu_target).unwrap_or(0),
            None,
        );
        drop(state);

        {
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
                .ok_or_else(|| attached_client_required("display-menu"))?;
            let popup_is_current = active.overlay.as_ref().is_some_and(|overlay| {
                overlay.id() == popup_id
                    && overlay
                        .identity()
                        .matches(&state, active, overlay.current_target())
            });
            if !popup_is_current || !menu_identity.matches(&state, active, &menu_target) {
                return Ok(());
            }
            if let Some(ClientOverlayState::Popup(popup)) = active
                .overlay
                .as_mut()
                .filter(|overlay| overlay.id() == popup_id)
            {
                popup.nested_menu = Some(MenuOverlayState {
                    id: popup_id,
                    identity: menu_identity.clone(),
                    origin,
                    command_context: menu_identity.command_context(
                        QueueExecutionContext::without_caller_cwd(),
                        menu_target.clone(),
                    ),
                    current_target: menu_target,
                    rect,
                    title,
                    style: options.style,
                    selected_style: options.selected_style,
                    border_style: options.border_style,
                    border_lines: options.border_lines,
                    flags: 0,
                    choice: items.iter().position(|item| !item.separator),
                    items,
                });
            }
        }
        self.refresh_interactive_overlay_for_optional_identity(attach_pid, identity)
            .await?;
        Ok(())
    }
}
