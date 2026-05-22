use std::io;

use rmux_proto::RmuxError;

use super::super::prompt_support::PromptInputEvent;
use super::super::scripting_support::QueueExecutionContext;
use super::super::RequestHandler;
use super::layout::{menu_option_styles, menu_width, target_window_index};
use super::menu::{
    menu_handle_event, menu_handle_mouse, popup_menu_items, MenuOutcome, MenuOverlayState,
    OverlayMenuAction, PopupMenuAction,
};
use super::mouse::{popup_handle_mouse, PopupMouseOutcome};
use super::state::ClientOverlayState;
use super::support::decode_prompt_key_guess;
use crate::handler_support::attached_client_required;
use crate::input_keys::{encode_mouse_event, MouseForwardEvent};
use crate::renderer::OverlayRect;

impl RequestHandler {
    pub(super) async fn handle_menu_input_event(
        &self,
        attach_pid: u32,
        event: PromptInputEvent,
    ) -> Result<bool, RmuxError> {
        let outcome = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
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

        self.apply_menu_outcome(attach_pid, outcome).await?;
        Ok(true)
    }

    pub(super) async fn handle_menu_mouse_event(
        &self,
        attach_pid: u32,
        raw: MouseForwardEvent,
    ) -> Result<(), RmuxError> {
        let outcome = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
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

        self.apply_menu_outcome(attach_pid, outcome).await
    }

    async fn apply_menu_outcome(
        &self,
        attach_pid: u32,
        outcome: MenuOutcome,
    ) -> Result<(), RmuxError> {
        match outcome {
            MenuOutcome::Stay => {}
            MenuOutcome::Redraw => {
                self.refresh_interactive_overlay_if_active(attach_pid)
                    .await?;
            }
            MenuOutcome::Close => {
                let mut clear_root = false;
                {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach.by_pid.get_mut(&attach_pid).ok_or_else(|| {
                        RmuxError::Server("attached client disappeared".to_owned())
                    })?;
                    match active.overlay.as_mut() {
                        Some(ClientOverlayState::Menu(_)) => clear_root = true,
                        Some(ClientOverlayState::Popup(popup)) => popup.nested_menu = None,
                        None => {}
                    }
                }
                if clear_root {
                    self.clear_interactive_overlay(attach_pid, true).await?;
                } else {
                    self.refresh_interactive_overlay_if_active(attach_pid)
                        .await?;
                }
            }
            MenuOutcome::Execute(action) => {
                let (requester_pid, target) = {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach.by_pid.get_mut(&attach_pid).ok_or_else(|| {
                        RmuxError::Server("attached client disappeared".to_owned())
                    })?;
                    match active.overlay.as_mut() {
                        Some(ClientOverlayState::Menu(menu)) => {
                            let target = menu.current_target.clone();
                            let requester_pid = menu.requester_pid;
                            active.overlay = None;
                            (requester_pid, target)
                        }
                        Some(ClientOverlayState::Popup(popup)) => {
                            let requester_pid = popup.requester_pid;
                            let target = popup.current_target.clone();
                            popup.nested_menu = None;
                            (requester_pid, target)
                        }
                        None => return Ok(()),
                    }
                };
                match action {
                    OverlayMenuAction::Command(command) => {
                        self.refresh_interactive_overlay_if_active(attach_pid)
                            .await
                            .ok();
                        let parsed = self.parse_command_string_one_group(&command).await?;
                        let _ = self
                            .execute_parsed_commands(
                                requester_pid,
                                parsed,
                                QueueExecutionContext::without_caller_cwd()
                                    .with_current_target(Some(target)),
                            )
                            .await?;
                    }
                    OverlayMenuAction::Popup(action) => {
                        self.apply_popup_menu_action(attach_pid, action).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn apply_popup_menu_action(
        &self,
        attach_pid: u32,
        action: PopupMenuAction,
    ) -> Result<(), RmuxError> {
        match action {
            PopupMenuAction::Close => {
                self.clear_interactive_overlay(attach_pid, true).await?;
            }
            PopupMenuAction::Paste => {
                let bytes = {
                    let state = self.state.lock().await;
                    state
                        .buffers
                        .stack_head()
                        .and_then(|name| state.buffers.get(name))
                        .map(ToOwned::to_owned)
                        .unwrap_or_default()
                };
                let mut refreshed = false;
                {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach.by_pid.get_mut(&attach_pid).ok_or_else(|| {
                        RmuxError::Server("attached client disappeared".to_owned())
                    })?;
                    if let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() {
                        popup.nested_menu = None;
                        if let Some(job) = &popup.job {
                            let _ = job.write(&bytes);
                        }
                        refreshed = true;
                    }
                }
                if refreshed {
                    self.refresh_interactive_overlay_if_active(attach_pid)
                        .await?;
                }
            }
            PopupMenuAction::FillSpace | PopupMenuAction::Centre => {
                let mut refreshed = false;
                {
                    let mut active_attach = self.active_attach.lock().await;
                    let active = active_attach.by_pid.get_mut(&attach_pid).ok_or_else(|| {
                        RmuxError::Server("attached client disappeared".to_owned())
                    })?;
                    let client_size = active.client_size;
                    if let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() {
                        popup.nested_menu = None;
                        match action {
                            PopupMenuAction::FillSpace => {
                                popup.rect = OverlayRect {
                                    x: 0,
                                    y: 0,
                                    width: client_size.cols.max(1),
                                    height: client_size.rows.max(1),
                                };
                                popup.preferred_width = popup.rect.width;
                                popup.preferred_height = popup.rect.height;
                            }
                            PopupMenuAction::Centre => {
                                popup.rect.x =
                                    client_size.cols.saturating_sub(popup.rect.width) / 2;
                                popup.rect.y =
                                    client_size.rows.saturating_sub(popup.rect.height) / 2;
                            }
                            _ => {}
                        }
                        let content_size = popup.content_size();
                        popup
                            .surface
                            .lock()
                            .expect("popup surface")
                            .resize(content_size);
                        if let Some(job) = &popup.job {
                            let _ = job.resize(content_size);
                        }
                        refreshed = true;
                    }
                }
                if refreshed {
                    self.refresh_interactive_overlay_if_active(attach_pid)
                        .await?;
                }
            }
            PopupMenuAction::HorizontalPane | PopupMenuAction::VerticalPane => {
                self.clear_interactive_overlay(attach_pid, true).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn handle_popup_raw_input(
        &self,
        attach_pid: u32,
        bytes: &[u8],
    ) -> io::Result<bool> {
        let popup = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
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

        if bytes.is_empty() {
            return Ok(true);
        }
        if (bytes == b"\x1b"
            || bytes == b"\x03"
            || matches!(
                decode_prompt_key_guess(bytes),
                Some(PromptInputEvent::Escape | PromptInputEvent::Ctrl('c'))
            ))
            && ((!popup.close_on_exit && !popup.close_on_zero_exit) || popup.no_job)
        {
            self.clear_interactive_overlay(attach_pid, true)
                .await
                .map_err(io::Error::other)?;
            return Ok(true);
        }
        if popup.no_job && popup.close_any_key {
            self.clear_interactive_overlay(attach_pid, true)
                .await
                .map_err(io::Error::other)?;
            return Ok(true);
        }
        if let Some(job) = &popup.job {
            job.write(bytes)?;
        }
        Ok(true)
    }

    pub(super) async fn handle_popup_mouse_event(
        &self,
        attach_pid: u32,
        raw: MouseForwardEvent,
    ) -> io::Result<()> {
        let nested_menu_active = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .and_then(|active| active.overlay.as_ref())
                .is_some_and(|overlay| {
                    matches!(overlay, ClientOverlayState::Popup(popup) if popup.nested_menu.is_some())
                })
        };
        if nested_menu_active {
            self.handle_menu_mouse_event(attach_pid, raw)
                .await
                .map_err(io::Error::other)?;
            return Ok(());
        }
        let outcome = {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| io::Error::other("attached client disappeared"))?;
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() else {
                return Ok(());
            };
            popup_handle_mouse(popup.as_mut(), active.client_size, raw)
        };

        match outcome {
            PopupMouseOutcome::Ignore => {}
            PopupMouseOutcome::Redraw => {
                self.refresh_interactive_overlay_if_active(attach_pid)
                    .await
                    .map_err(io::Error::other)?;
            }
            PopupMouseOutcome::Forward { mode, event, x, y } => {
                let popup = {
                    let active_attach = self.active_attach.lock().await;
                    active_attach.by_pid.get(&attach_pid).and_then(|active| {
                        match active.overlay.as_ref() {
                            Some(ClientOverlayState::Popup(popup)) => Some(popup.clone()),
                            _ => None,
                        }
                    })
                };
                if let Some(popup) = popup
                    && let Some(job) = &popup.job
                        && let Some(bytes) = encode_mouse_event(mode, &event, x, y) {
                            let _ = job.write(&bytes);
                        }
            }
            PopupMouseOutcome::OpenMenu { x, y } => {
                self.open_popup_internal_menu(attach_pid, x, y)
                    .await
                    .map_err(io::Error::other)?;
            }
        }
        Ok(())
    }

    async fn open_popup_internal_menu(
        &self,
        attach_pid: u32,
        x: u16,
        y: u16,
    ) -> Result<(), RmuxError> {
        let (client_size, popup_target, popup_requester_pid) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| attached_client_required("display-menu"))?;
            let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_ref() else {
                return Ok(());
            };
            (
                active.client_size,
                popup.current_target.clone(),
                popup.requester_pid,
            )
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
            popup_target.session_name(),
            target_window_index(&popup_target).unwrap_or(0),
            None,
        );

        {
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .ok_or_else(|| attached_client_required("display-menu"))?;
            if let Some(ClientOverlayState::Popup(popup)) = active.overlay.as_mut() {
                popup.nested_menu = Some(MenuOverlayState {
                    id: popup.id,
                    requester_pid: popup_requester_pid,
                    current_target: popup_target,
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
        self.refresh_interactive_overlay_if_active(attach_pid)
            .await?;
        Ok(())
    }
}
