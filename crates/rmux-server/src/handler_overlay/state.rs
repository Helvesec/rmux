use std::sync::{Arc, Mutex as StdMutex};

use rmux_core::{BoxLines, Style};
use rmux_proto::{Target, TerminalSize};

use super::super::scripting_support::rename_target_session;
use crate::renderer::{render_popup_overlay, OverlayRect, PopupRenderSpec};

use super::menu::MenuOverlayState;
use super::popup_job::{PopupDragMode, PopupJob, PopupSurface};

#[derive(Debug, Clone)]
pub(in crate::handler) enum ClientOverlayState {
    Menu(Box<MenuOverlayState>),
    Popup(Box<PopupOverlayState>),
}

impl ClientOverlayState {
    pub(in crate::handler) fn id(&self) -> u64 {
        match self {
            Self::Menu(menu) => menu.id,
            Self::Popup(popup) => popup.id,
        }
    }

    pub(in crate::handler) fn render(&self) -> Vec<u8> {
        match self {
            Self::Menu(menu) => menu.render(),
            Self::Popup(popup) => popup.render(),
        }
    }

    pub(in crate::handler) fn rename_session_targets(
        &mut self,
        old_name: &rmux_proto::SessionName,
        new_name: &rmux_proto::SessionName,
    ) {
        match self {
            Self::Menu(menu) => {
                rename_target_session(&mut menu.current_target, old_name, new_name);
            }
            Self::Popup(popup) => {
                rename_target_session(&mut popup.current_target, old_name, new_name);
                if let Some(menu) = popup.nested_menu.as_mut() {
                    rename_target_session(&mut menu.current_target, old_name, new_name);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct PopupOverlayState {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) requester_pid: u32,
    pub(in crate::handler) current_target: Target,
    pub(in crate::handler) rect: OverlayRect,
    pub(in crate::handler) preferred_width: u16,
    pub(in crate::handler) preferred_height: u16,
    pub(in crate::handler) title: String,
    pub(in crate::handler) style: Style,
    pub(in crate::handler) border_style: Style,
    pub(in crate::handler) border_lines: BoxLines,
    pub(in crate::handler) close_on_exit: bool,
    pub(in crate::handler) close_on_zero_exit: bool,
    pub(in crate::handler) close_any_key: bool,
    pub(in crate::handler) no_job: bool,
    pub(in crate::handler) surface: Arc<StdMutex<PopupSurface>>,
    pub(in crate::handler) job: Option<PopupJob>,
    pub(in crate::handler) nested_menu: Option<MenuOverlayState>,
    pub(in crate::handler) dragging: PopupDragMode,
}

impl PopupOverlayState {
    fn render(&self) -> Vec<u8> {
        let popup_frame = render_popup_overlay(&PopupRenderSpec {
            rect: self.rect,
            title: self.title.clone(),
            style: self.style.clone(),
            border_style: self.border_style.clone(),
            border_lines: self.border_lines,
            content_lines: self.surface.lock().expect("popup surface").lines(),
        });
        if let Some(menu) = &self.nested_menu {
            let mut frame = popup_frame;
            frame.extend_from_slice(&menu.render());
            frame
        } else {
            popup_frame
        }
    }

    pub(super) fn content_origin(&self) -> (u16, u16) {
        if self.border_lines.visible() {
            (self.rect.x.saturating_add(1), self.rect.y.saturating_add(1))
        } else {
            (self.rect.x, self.rect.y)
        }
    }

    pub(super) fn content_size(&self) -> TerminalSize {
        let rect = if self.border_lines.visible() {
            OverlayRect {
                x: self.rect.x.saturating_add(1),
                y: self.rect.y.saturating_add(1),
                width: self.rect.width.saturating_sub(2),
                height: self.rect.height.saturating_sub(2),
            }
        } else {
            self.rect
        };
        TerminalSize {
            cols: rect.width,
            rows: rect.height,
        }
    }
}
