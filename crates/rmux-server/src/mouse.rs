#![allow(dead_code)]

use std::time::{Duration, Instant};

use rmux_core::KEYC_DRAGGING;
use rmux_proto::{PaneTarget, SessionId, SessionName};

use crate::copy_mode::{CopyModeLineNumberLayout, CopyModeMouseContext};
use crate::input_keys::MouseForwardEvent;

mod event_keys;
mod hit;
mod layout;
mod projection;
mod scrollbar_geometry;
pub(crate) use projection::MouseCoordinates;
pub(crate) use scrollbar_geometry::pane_mouse_coordinates_for_target;
mod types;

use event_keys::{
    build_classified_event, button_number, is_mouse_move, modifier_bits, mouse_buttons, mouse_drag,
    mouse_release, mouse_wheel, synthesize_mouse_key,
};
use hit::{hit_to_attached_event, resolve_mouse_hit};
pub(crate) use layout::{layout_for_session, layout_for_view};
pub(crate) use scrollbar_geometry::pane_content_geometry_for_target;
#[allow(unused_imports)]
pub(crate) use types::StatusRange;
pub(crate) use types::{
    AttachedMouseEvent, ClassifiedMouseEvent, ClientMouseState, MouseClickTimerToken,
    MouseEventKind, MouseLayout, MouseLocation, PanePassthroughMouseEvent,
};
#[cfg(test)]
pub(crate) use types::{
    BorderControlRange, MouseDragHandler, PaneBorderStatus, PaneMouseTarget, PaneScrollbar,
    PaneScrollbarsMode, ScrollbarPosition,
};
#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use types::{StatusLineLayout, StatusRangeType};

const KEYC_CLICK_TIMEOUT: Duration = Duration::from_millis(300);

const MOUSE_MASK_BUTTONS: u16 = 195;
const MOUSE_MASK_SHIFT: u16 = 4;
const MOUSE_MASK_META: u16 = 8;
const MOUSE_MASK_CTRL: u16 = 16;
const MOUSE_MASK_DRAG: u16 = 32;
const MOUSE_WHEEL_UP: u16 = 64;
const MOUSE_WHEEL_DOWN: u16 = 65;
const MOUSE_BUTTON_1: u16 = 0;
const MOUSE_BUTTON_2: u16 = 1;
const MOUSE_BUTTON_3: u16 = 2;
const MOUSE_BUTTON_6: u16 = 66;
const MOUSE_BUTTON_7: u16 = 67;
const MOUSE_BUTTON_8: u16 = 128;
const MOUSE_BUTTON_9: u16 = 129;
const MOUSE_BUTTON_10: u16 = 130;
const MOUSE_BUTTON_11: u16 = 131;

impl AttachedMouseEvent {
    pub(crate) fn is_wheel(&self) -> bool {
        mouse_wheel(self.raw.b)
    }
}

impl ClientMouseState {
    pub(crate) fn rename_session_targets(
        &mut self,
        old_name: &SessionName,
        session_id: SessionId,
        new_name: &SessionName,
    ) {
        for event in [
            self.click_event.as_mut(),
            self.current_event.as_mut(),
            self.drag_start_event.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            if event.session_id == session_id.as_u32() {
                if let Some(target) = event.pane_target.as_mut() {
                    rename_mouse_pane_target(target, old_name, new_name);
                }
            }
        }
        if let Some(handler) = self.drag_handler.as_mut() {
            let target = match handler {
                types::MouseDragHandler::CopyModeSelection { target }
                | types::MouseDragHandler::CopyModeScrollbar { target } => target,
            };
            rename_mouse_pane_target(target, old_name, new_name);
        }
    }

    pub(crate) fn expire_click_timer(
        &mut self,
        now: Instant,
        layout: &MouseLayout,
    ) -> Option<ClassifiedMouseEvent> {
        let deadline = self.click_deadline?;
        if deadline > now {
            return None;
        }

        let double_click = if self.triple_click_pending {
            self.click_event.as_ref().map(|event| {
                build_classified_event(
                    MouseEventKind::DoubleClick,
                    event.clone(),
                    button_number(mouse_buttons(event.raw.b)),
                    self.drag_handler.is_some(),
                    layout,
                )
            })
        } else {
            None
        };

        self.clear_click_timer_state();
        double_click.flatten()
    }
}

fn rename_mouse_pane_target(
    target: &mut PaneTarget,
    old_name: &SessionName,
    new_name: &SessionName,
) {
    if target.session_name() != old_name {
        return;
    }
    *target = PaneTarget::with_window(new_name.clone(), target.window_index(), target.pane_index());
}

pub(crate) fn classify_mouse_event(
    state: &mut ClientMouseState,
    layout: &MouseLayout,
    raw: MouseForwardEvent,
    now: Instant,
) -> Option<ClassifiedMouseEvent> {
    classify_mouse_events(state, layout, raw, now)
        .into_iter()
        .next()
}

pub(crate) fn classify_mouse_events(
    state: &mut ClientMouseState,
    layout: &MouseLayout,
    raw: MouseForwardEvent,
    now: Instant,
) -> Vec<ClassifiedMouseEvent> {
    let expired = state.expire_click_timer(now, layout);
    let current = classify_current_mouse_event(state, layout, raw, now);
    expired.into_iter().chain(current).collect()
}

/// Maps a raw outer-terminal report to the pane under the cursor without
/// synthesizing tmux click/drag bindings. This is the application passthrough
/// path used when the pane requested mouse tracking but the RMUX `mouse`
/// option is off.
pub(crate) fn mouse_event_for_pane_passthrough(
    layout: &MouseLayout,
    raw: MouseForwardEvent,
) -> Option<PanePassthroughMouseEvent> {
    let hit = resolve_mouse_hit(layout, raw.x, raw.y, false, None);
    let event = hit_to_attached_event(layout, raw, hit, false)?;
    if event.location != MouseLocation::Pane {
        return None;
    }
    let focus_target = mouse_focus_target(layout, &event, is_mouse_move(raw));
    Some(PanePassthroughMouseEvent {
        event,
        focus_target,
    })
}

fn classify_current_mouse_event(
    state: &mut ClientMouseState,
    layout: &MouseLayout,
    raw: MouseForwardEvent,
    now: Instant,
) -> Option<ClassifiedMouseEvent> {
    let (kind, x, y, mut button_bits, ignore) = if is_mouse_move(raw) {
        (MouseEventKind::MouseMove, raw.x, raw.y, 0, false)
    } else if mouse_drag(raw.b) {
        if state.drag_flag != 0 {
            if raw.x == raw.lx && raw.y == raw.ly {
                return None;
            }
            (MouseEventKind::MouseDrag, raw.x, raw.y, raw.b, false)
        } else {
            (MouseEventKind::MouseDrag, raw.lx, raw.ly, raw.lb, false)
        }
    } else if mouse_wheel(raw.b) {
        let kind = if mouse_buttons(raw.b) == MOUSE_WHEEL_UP {
            MouseEventKind::WheelUp
        } else {
            MouseEventKind::WheelDown
        };
        (kind, raw.x, raw.y, raw.b, false)
    } else if mouse_release(raw.b) {
        let button_bits = if raw.sgr_type == 'm' {
            raw.sgr_b
        } else {
            raw.lb
        };
        (MouseEventKind::MouseUp, raw.x, raw.y, button_bits, false)
    } else if state.double_click_pending {
        state.click_deadline = None;
        state.double_click_pending = false;
        state.triple_click_pending = true;
        (MouseEventKind::SecondClick, raw.x, raw.y, raw.b, false)
    } else if state.triple_click_pending {
        state.click_deadline = None;
        state.triple_click_pending = false;
        (MouseEventKind::TripleClick, raw.x, raw.y, raw.b, false)
    } else {
        state.double_click_pending = true;
        state.triple_click_pending = false;
        (MouseEventKind::MouseDown, raw.x, raw.y, raw.b, false)
    };

    let hit = resolve_mouse_hit(
        layout,
        x,
        y,
        state.scrolling_flag,
        state.current_event.as_ref(),
    );
    let slider_mpos = hit.slider_mpos;
    let attached_raw = if matches!(kind, MouseEventKind::MouseDrag)
        && state.drag_flag == 0
        && state.drag_handler.is_some()
    {
        MouseForwardEvent {
            b: button_bits,
            x,
            y,
            ..raw
        }
    } else {
        raw
    };
    let mut attached_event = hit_to_attached_event(layout, attached_raw, hit, ignore)?;

    if matches!(kind, MouseEventKind::MouseDown)
        || (matches!(kind, MouseEventKind::MouseDrag) && state.drag_start_event.is_none())
    {
        state.drag_start_event = Some(attached_event.clone());
    }

    if drag_origin_should_lock_location(kind, state.drag_flag, state.drag_start_event.as_ref()) {
        if let Some(start) = state.drag_start_event.as_ref() {
            apply_drag_origin_location(&mut attached_event, start);
        }
    }

    let mut kind = kind;

    if matches!(
        kind,
        MouseEventKind::MouseDown | MouseEventKind::SecondClick | MouseEventKind::TripleClick
    ) {
        if !matches!(kind, MouseEventKind::MouseDown)
            && (button_bits != state.click_button
                || attached_event.location != state.click_location
                || attached_event.pane_id != state.click_pane)
        {
            kind = MouseEventKind::MouseDown;
            state.triple_click_pending = false;
            state.double_click_pending = true;
        }

        if !matches!(kind, MouseEventKind::TripleClick) {
            state.arm_click_timer(now + KEYC_CLICK_TIMEOUT);
            state.click_button = button_bits;
            state.click_location = attached_event.location;
            state.click_pane = attached_event.pane_id;
            let button_event = AttachedMouseEvent {
                raw: MouseForwardEvent {
                    b: button_bits,
                    ..attached_event.raw
                },
                ..attached_event.clone()
            };
            state.click_event = Some(button_event);
        } else {
            state.click_deadline = None;
            state.click_event = None;
        }
    }

    if !matches!(
        kind,
        MouseEventKind::MouseDrag
            | MouseEventKind::WheelUp
            | MouseEventKind::WheelDown
            | MouseEventKind::DoubleClick
            | MouseEventKind::TripleClick
    ) && state.drag_flag != 0
    {
        kind = MouseEventKind::MouseDragEnd;
        // Preserve modifier bits from the current event's button value while
        // replacing the button identity with the one that started the drag.
        let drag_button = u16::from(state.drag_flag.saturating_sub(1));
        let current_modifiers = button_bits & !(MOUSE_MASK_BUTTONS | MOUSE_MASK_DRAG);
        button_bits = drag_button | current_modifiers;
        state.drag_flag = 0;
        state.scrolling_flag = false;
        state.slider_mpos = -1;
        state.drag_handler = None;
        state.drag_start_event = None;
    }

    let focus_target = mouse_focus_target(
        layout,
        &attached_event,
        matches!(kind, MouseEventKind::MouseMove),
    );

    let key = if matches!(kind, MouseEventKind::MouseMove)
        && attached_event.location == MouseLocation::Pane
    {
        synthesize_mouse_key(kind, 0, attached_event.location)?
    } else if matches!(kind, MouseEventKind::MouseDrag) && state.drag_handler.is_some() {
        KEYC_DRAGGING
    } else {
        let button = button_number(mouse_buttons(button_bits));
        synthesize_mouse_key(kind, button, attached_event.location)?
    } | modifier_bits(button_bits);

    if matches!(kind, MouseEventKind::MouseDrag) {
        state.drag_flag = mouse_buttons(button_bits).saturating_add(1) as u8;
        if !state.scrolling_flag && attached_event.location == MouseLocation::ScrollbarSlider {
            state.scrolling_flag = true;
            let slider_mpos = slider_mpos.unwrap_or(0) as i32;
            state.slider_mpos = if layout.status_at == Some(0) {
                slider_mpos + i32::from(layout.status_lines)
            } else {
                slider_mpos
            };
        }
    }

    attached_event.ignore = ignore;
    state.current_event = Some(attached_event.clone());
    if matches!(kind, MouseEventKind::MouseUp) && state.drag_flag == 0 {
        state.drag_start_event = None;
    }
    Some(ClassifiedMouseEvent {
        key,
        event: attached_event,
        focus_target,
    })
}

fn mouse_focus_target(
    layout: &MouseLayout,
    event: &AttachedMouseEvent,
    is_mouse_move: bool,
) -> Option<rmux_core::PaneId> {
    if is_mouse_move
        && event.location == MouseLocation::Pane
        && layout.focus_follows_mouse
        && event.pane_id != layout.active_pane
    {
        event.pane_id
    } else {
        None
    }
}

fn drag_origin_should_lock_location(
    kind: MouseEventKind,
    drag_flag: u8,
    start: Option<&AttachedMouseEvent>,
) -> bool {
    start.is_some_and(|event| {
        matches!(event.location, MouseLocation::Pane | MouseLocation::Border)
            && (drag_flag != 0
                || matches!(kind, MouseEventKind::MouseDrag | MouseEventKind::MouseUp))
    })
}

fn apply_drag_origin_location(event: &mut AttachedMouseEvent, start: &AttachedMouseEvent) {
    event.session_id = start.session_id;
    event.window_id = start.window_id;
    event.pane_id = start.pane_id;
    event.pane_target = start.pane_target.clone();
    event.coordinates = start.coordinates;
    event.location = start.location;
}

pub(crate) fn copy_mode_mouse_context(
    event: &AttachedMouseEvent,
    pane: impl Into<MouseCoordinates> + Copy,
    slider_mpos: i32,
) -> Option<CopyModeMouseContext> {
    copy_mode_mouse_context_with_line_numbers(event, pane, slider_mpos, None)
}

pub(crate) fn copy_mode_mouse_context_with_line_numbers(
    event: &AttachedMouseEvent,
    pane: impl Into<MouseCoordinates> + Copy,
    slider_mpos: i32,
    line_numbers: Option<CopyModeLineNumberLayout>,
) -> Option<CopyModeMouseContext> {
    event.pane_id?;
    let pane = event.coordinates.unwrap_or_else(|| pane.into());

    let adjusted_y = match event.status_at {
        Some(0) if event.raw.y >= event.status_lines => event.raw.y - event.status_lines,
        _ => event.raw.y,
    };
    if pane.above(adjusted_y) {
        return None;
    }

    let (relative_x, relative_y) = pane.clamped_offset(event.raw.x, adjusted_y);
    let content_x = line_numbers.map_or_else(
        || u32::from(relative_x.min(pane.cols().saturating_sub(1))),
        |layout| layout.mouse_content_x(pane.cols(), relative_x),
    );
    let content_y = relative_y.min(pane.rows().saturating_sub(1));
    let scroll_y = if event.status_at == Some(0) {
        relative_y.saturating_add(event.status_lines)
    } else {
        relative_y
    };

    Some(CopyModeMouseContext {
        content_x,
        content_y,
        selection_anchor: None,
        scroll_y,
        slider_mpos,
        move_cursor_before_command: false,
    })
}

pub(crate) fn copy_mode_mouse_drag_start_context(
    event: &AttachedMouseEvent,
    pane: impl Into<MouseCoordinates> + Copy,
    slider_mpos: i32,
) -> Option<CopyModeMouseContext> {
    copy_mode_mouse_drag_start_context_with_line_numbers(event, pane, slider_mpos, None)
}

pub(crate) fn copy_mode_mouse_drag_start_context_with_line_numbers(
    event: &AttachedMouseEvent,
    pane: impl Into<MouseCoordinates> + Copy,
    slider_mpos: i32,
    line_numbers: Option<CopyModeLineNumberLayout>,
) -> Option<CopyModeMouseContext> {
    const MOUSE_MASK_DRAG: u16 = 32;

    let mut current =
        copy_mode_mouse_context_with_line_numbers(event, pane, slider_mpos, line_numbers)?;
    if event.raw.b & MOUSE_MASK_DRAG == 0 {
        return Some(current);
    }

    let mut anchor_event = event.clone();
    anchor_event.raw.b = event.raw.lb;
    anchor_event.raw.sgr_b = event.raw.lb;
    anchor_event.raw.x = event.raw.lx;
    anchor_event.raw.y = event.raw.ly;
    let anchor =
        copy_mode_mouse_context_with_line_numbers(&anchor_event, pane, slider_mpos, line_numbers)?;
    current.selection_anchor = Some((anchor.content_x, anchor.content_y));
    Some(current)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scene_transform_tests;
