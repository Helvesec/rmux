use std::sync::{Arc, Mutex as StdMutex};

use rmux_core::{BoxLines, Style};
use rmux_proto::{Target, TerminalSize};

use super::super::scripting_support::rename_target_session;
use crate::renderer::{
    render_popup_cursor, render_popup_overlay, OverlayRect, PopupContent, PopupCursor,
    PopupRenderSpec,
};

use super::completion::PopupCompletion;
use super::identity::OverlayIdentity;
use super::menu::MenuOverlayState;
use super::popup_job::{PopupDragMode, PopupJob, PopupSurface, PopupSurfaceCursor};
use super::scrollable::ScrollablePopupText;

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

    pub(in crate::handler) fn intersects(&self, region: OverlayRect) -> bool {
        let overlaps = |rect: OverlayRect| {
            rect.width > 0
                && rect.height > 0
                && region.width > 0
                && region.height > 0
                && rect.x < region.x.saturating_add(region.width)
                && region.x < rect.x.saturating_add(rect.width)
                && rect.y < region.y.saturating_add(region.height)
                && region.y < rect.y.saturating_add(rect.height)
        };
        match self {
            Self::Menu(menu) => overlaps(menu.rect),
            Self::Popup(popup) => {
                overlaps(popup.rect)
                    || popup
                        .nested_menu
                        .as_ref()
                        .is_some_and(|menu| overlaps(menu.rect))
            }
        }
    }

    pub(in crate::handler) fn render(&self) -> Vec<u8> {
        match self {
            Self::Menu(menu) => menu.render(),
            Self::Popup(popup) => popup.render(),
        }
    }

    pub(super) fn identity(&self) -> &OverlayIdentity {
        match self {
            Self::Menu(menu) => &menu.identity,
            Self::Popup(popup) => &popup.identity,
        }
    }

    pub(super) fn current_target(&self) -> &Target {
        match self {
            Self::Menu(menu) => &menu.current_target,
            Self::Popup(popup) => &popup.current_target,
        }
    }

    pub(in crate::handler) fn rename_session_targets(
        &mut self,
        old_name: &rmux_proto::SessionName,
        new_name: &rmux_proto::SessionName,
    ) {
        match self {
            Self::Menu(menu) => {
                menu.identity.rename_session(old_name, new_name);
                rename_target_session(&mut menu.current_target, old_name, new_name);
                menu.command_context
                    .rename_session_targets(old_name, new_name);
            }
            Self::Popup(popup) => {
                popup.identity.rename_session(old_name, new_name);
                rename_target_session(&mut popup.current_target, old_name, new_name);
                if let Some(menu) = popup.nested_menu.as_mut() {
                    menu.identity.rename_session(old_name, new_name);
                    rename_target_session(&mut menu.current_target, old_name, new_name);
                    menu.command_context
                        .rename_session_targets(old_name, new_name);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct PopupOverlayState {
    pub(in crate::handler) id: u64,
    pub(in crate::handler) identity: OverlayIdentity,
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
    pub(in crate::handler) scrollable_text: Option<ScrollablePopupText>,
    pub(in crate::handler) job: Option<PopupJob>,
    pub(in crate::handler) nested_menu: Option<MenuOverlayState>,
    pub(in crate::handler) dragging: PopupDragMode,
    pub(super) last_mouse: Option<crate::input_keys::MouseForwardEvent>,
    /// Present when a command waits for this popup to leave its client.
    pub(in crate::handler) completion: Option<PopupCompletion>,
}

impl PopupOverlayState {
    #[cfg(test)]
    pub(in crate::handler) fn begin_resize_for_test(&mut self) {
        self.dragging = PopupDragMode::Resize;
    }

    fn render(&self) -> Vec<u8> {
        let (content, surface_cursor) = if let Some(text) = &self.scrollable_text {
            (
                PopupContent::Text(text.visible_lines(self.content_size().rows)),
                None,
            )
        } else {
            let surface = self
                .surface
                .lock()
                .expect("popup surface")
                .frame(&self.style);
            (PopupContent::Surface(surface.rows), Some(surface.cursor))
        };
        let cursor = self.project_cursor(surface_cursor);
        let mut frame = render_popup_overlay(&PopupRenderSpec {
            rect: self.rect,
            title: self.title.clone(),
            style: self.style.clone(),
            border_style: self.border_style.clone(),
            border_lines: self.border_lines,
            content,
        });
        if let Some(menu) = &self.nested_menu {
            frame.extend_from_slice(&menu.render());
        }
        frame.extend_from_slice(render_popup_cursor(cursor).as_slice());
        frame
    }

    fn project_cursor(&self, cursor: Option<PopupSurfaceCursor>) -> PopupCursor {
        let size = self.content_size();
        if self.no_job
            || self.scrollable_text.is_some()
            || self.nested_menu.is_some()
            || size.cols == 0
            || size.rows == 0
        {
            return PopupCursor::Hidden;
        }
        let Some(cursor) = cursor else {
            return PopupCursor::Hidden;
        };
        let (origin_x, origin_y) = self.content_origin();
        PopupCursor::Projected {
            x: origin_x.saturating_add(cursor.x.min(size.cols.saturating_sub(1))),
            y: origin_y.saturating_add(cursor.y.min(size.rows.saturating_sub(1))),
            visible: cursor.visible,
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
