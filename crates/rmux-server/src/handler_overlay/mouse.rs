use rmux_proto::TerminalSize;

use super::popup_job::{PopupDragMode, PopupIoReceipt};
use super::PopupOverlayState;
use crate::input_keys::MouseForwardEvent;

const MOUSE_MASK_BUTTONS: u16 = 195;
const MOUSE_MASK_META: u16 = 8;
const MOUSE_MASK_DRAG: u16 = 32;
const MOUSE_BUTTON_1: u16 = 0;
const MOUSE_BUTTON_3: u16 = 2;

#[derive(Debug)]
pub(super) enum PopupMouseOutcome {
    Ignore,
    Redraw {
        resize: Option<PopupIoReceipt>,
    },
    OpenMenu {
        x: u16,
        y: u16,
    },
    Forward {
        mode: u32,
        event: MouseForwardEvent,
        x: u16,
        y: u16,
    },
}

pub(super) fn popup_handle_mouse(
    popup: &mut PopupOverlayState,
    client_size: TerminalSize,
    raw: MouseForwardEvent,
) -> PopupMouseOutcome {
    let within = raw.x >= popup.rect.x
        && raw.x < popup.rect.x.saturating_add(popup.rect.width)
        && raw.y >= popup.rect.y
        && raw.y < popup.rect.y.saturating_add(popup.rect.height);

    if popup.dragging != PopupDragMode::Off {
        if !mouse_drag(raw.b) {
            popup.dragging = PopupDragMode::Off;
            return PopupMouseOutcome::Redraw { resize: None };
        }
        match popup.dragging {
            PopupDragMode::Move { dx, dy } => {
                let next_x = raw.x.saturating_sub(dx);
                let next_y = raw.y.saturating_sub(dy);
                popup.rect.x = next_x.min(client_size.cols.saturating_sub(popup.rect.width));
                popup.rect.y = next_y.min(client_size.rows.saturating_sub(popup.rect.height));
                return PopupMouseOutcome::Redraw { resize: None };
            }
            PopupDragMode::Resize => {
                let min_w = if popup.border_lines.visible() { 3 } else { 1 };
                let min_h = if popup.border_lines.visible() { 3 } else { 1 };
                popup.rect.width = raw
                    .x
                    .saturating_sub(popup.rect.x)
                    .saturating_add(1)
                    .clamp(min_w, client_size.cols.saturating_sub(popup.rect.x));
                popup.rect.height = raw
                    .y
                    .saturating_sub(popup.rect.y)
                    .saturating_add(1)
                    .clamp(min_h, client_size.rows.saturating_sub(popup.rect.y));
                popup.preferred_width = popup.rect.width;
                popup.preferred_height = popup.rect.height;
                popup
                    .surface
                    .lock()
                    .expect("popup surface")
                    .resize(popup.content_size());
                let content_size = popup.content_size();
                return PopupMouseOutcome::Redraw {
                    resize: popup
                        .job
                        .as_ref()
                        .and_then(|job| job.enqueue_resize(content_size).ok()),
                };
            }
            PopupDragMode::Off => {}
        }
    }

    if !within {
        return PopupMouseOutcome::Ignore;
    }

    let border = if popup.border_lines.visible() {
        if raw.x == popup.rect.x {
            Some(0_u8)
        } else if raw.x
            == popup
                .rect
                .x
                .saturating_add(popup.rect.width.saturating_sub(1))
        {
            Some(1_u8)
        } else if raw.y == popup.rect.y {
            Some(2_u8)
        } else if raw.y
            == popup
                .rect
                .y
                .saturating_add(popup.rect.height.saturating_sub(1))
        {
            Some(3_u8)
        } else {
            None
        }
    } else {
        None
    };

    if mouse_button(raw.b) == MOUSE_BUTTON_3
        && !mouse_drag(raw.b)
        && raw.x == popup.rect.x
        && raw.y == popup.rect.y
    {
        return PopupMouseOutcome::OpenMenu { x: raw.x, y: raw.y };
    }

    if mouse_drag(raw.b) && ((raw.b & MOUSE_MASK_META) == MOUSE_MASK_META || border.is_some()) {
        if mouse_button(raw.lb) == MOUSE_BUTTON_1 {
            popup.dragging = PopupDragMode::Move {
                dx: raw.lx.saturating_sub(popup.rect.x),
                dy: raw.ly.saturating_sub(popup.rect.y),
            };
            return PopupMouseOutcome::Redraw { resize: None };
        }
        if mouse_button(raw.lb) == MOUSE_BUTTON_3 && border.is_some() {
            popup.dragging = PopupDragMode::Resize;
            return PopupMouseOutcome::Redraw { resize: None };
        }
    }

    let (content_x, content_y) = popup.content_origin();
    let content_size = popup.content_size();
    if raw.x < content_x
        || raw.x >= content_x.saturating_add(content_size.cols)
        || raw.y < content_y
        || raw.y >= content_y.saturating_add(content_size.rows)
    {
        return PopupMouseOutcome::Ignore;
    }
    let translated_x = raw.x - content_x;
    let translated_y = raw.y - content_y;
    let mode = popup.surface.lock().expect("popup surface").mode();
    PopupMouseOutcome::Forward {
        mode,
        event: raw,
        x: translated_x,
        y: translated_y,
    }
}

fn mouse_button(b: u16) -> u16 {
    b & MOUSE_MASK_BUTTONS
}

pub(super) fn mouse_drag(b: u16) -> bool {
    (b & MOUSE_MASK_DRAG) != 0
}

pub(super) fn mouse_release(b: u16) -> bool {
    mouse_button(b) == 3 && !mouse_drag(b)
}

pub(super) fn is_mouse_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[M") || bytes.starts_with(b"\x1b[<")
}
