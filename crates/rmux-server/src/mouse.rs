#![allow(dead_code)]

use std::time::{Duration, Instant};

use rmux_core::{
    key_string_lookup_string, KeyCode, PaneGeometry, KEYC_CTRL, KEYC_DRAGGING, KEYC_META,
    KEYC_SHIFT,
};
use rmux_proto::{OptionName, PaneTarget, SessionName};

use crate::copy_mode::CopyModeMouseContext;
use crate::input_keys::MouseForwardEvent;
use crate::pane_terminals::HandlerState;

mod hit;
mod types;

use hit::{hit_to_attached_event, resolve_mouse_hit};
#[allow(unused_imports)]
pub(crate) use types::StatusRange;
pub(crate) use types::{
    AttachedMouseEvent, ClassifiedMouseEvent, ClientMouseState, MouseEventKind, MouseLayout,
    MouseLocation, PaneBorderStatus, PaneMouseTarget, PaneScrollbar, PaneScrollbarsMode,
    ScrollbarPosition,
};
#[cfg(test)]
pub(crate) use types::{BorderControlRange, MouseDragHandler};
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

impl ClientMouseState {
    pub(crate) fn click_deadline(&self) -> Option<Instant> {
        self.click_deadline
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

        self.click_deadline = None;
        self.double_click_pending = false;
        self.triple_click_pending = false;
        self.click_event = None;
        double_click.flatten()
    }
}

pub(crate) fn layout_for_session(
    state: &HandlerState,
    session_name: &SessionName,
    attached_count: usize,
) -> Option<MouseLayout> {
    let session = state.sessions.session(session_name)?;
    let window_index = session.active_window_index();
    let window = session.window_at(window_index)?;
    let status_enabled = window.size().cols != 0
        && window.size().rows != 0
        && !matches!(
            state
                .options
                .resolve(Some(session_name), OptionName::Status),
            Some("off")
        );
    let (status_at, status_lines) = if status_enabled {
        match state
            .options
            .resolve(Some(session_name), OptionName::StatusPosition)
        {
            Some("top") => (Some(0), 1),
            _ => (Some(window.size().rows.saturating_sub(1)), 1),
        }
    } else {
        (None, 0)
    };
    let pane_border_status = parse_pane_border_status(state.options.resolve_for_window(
        session_name,
        window_index,
        OptionName::PaneBorderStatus,
    ));
    let scrollbar_mode = parse_pane_scrollbars_mode(state.options.resolve_for_window(
        session_name,
        window_index,
        OptionName::PaneScrollbars,
    ));
    let scrollbar_position = parse_scrollbar_position(state.options.resolve_for_window(
        session_name,
        window_index,
        OptionName::PaneScrollbarsPosition,
    ));
    let focus_follows_mouse = state
        .options
        .resolve(Some(session_name), OptionName::FocusFollowsMouse)
        .is_some_and(|value| value == "on");
    let panes = if window.is_zoomed() {
        window.active_pane().into_iter().collect::<Vec<_>>()
    } else {
        window.panes().iter().collect::<Vec<_>>()
    };

    Some(MouseLayout {
        session_id: session.id().as_u32(),
        status_at,
        status_lines,
        status: crate::renderer::status_line_layout(session, &state.options, attached_count, None),
        pane_border_status,
        focus_follows_mouse,
        active_pane: window.active_pane().map(|pane| pane.id()),
        panes: panes
            .into_iter()
            .map(|pane| {
                let (scrollbar_width, scrollbar_pad) =
                    parse_scrollbar_style(state.options.resolve_for_pane(
                        session_name,
                        window_index,
                        pane.index(),
                        OptionName::PaneScrollbarsStyle,
                    ));
                let history_size = state
                    .pane_history_size_stats(session_name, pane.id())
                    .map(|stats| stats.size)
                    .unwrap_or_default();
                let alternate_on = state
                    .pane_screen_state(session_name, pane.id())
                    .map(|screen| screen.alternate_on)
                    .unwrap_or(false);
                let copy_mode_offset = state
                    .pane_copy_mode_summary(session_name, pane.id())
                    .map(|summary| summary.scroll_position);
                let scrollbar = PaneScrollbar::from_view(
                    pane.geometry().rows(),
                    history_size,
                    alternate_on,
                    scrollbar_mode,
                    scrollbar_position,
                    scrollbar_width,
                    scrollbar_pad,
                    copy_mode_offset,
                );
                PaneMouseTarget {
                    pane_id: pane.id(),
                    pane_target: Some(PaneTarget::with_window(
                        session_name.clone(),
                        window_index,
                        pane.index(),
                    )),
                    window_id: window.id().as_u32(),
                    geometry: pane.geometry(),
                    scrollbar,
                    border_controls: Vec::new(),
                }
            })
            .collect(),
    })
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
            state.click_deadline = Some(now + KEYC_CLICK_TIMEOUT);
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
    }

    let focus_target = if matches!(kind, MouseEventKind::MouseMove)
        && attached_event.location == MouseLocation::Pane
        && layout.focus_follows_mouse
        && attached_event.pane_id != layout.active_pane
    {
        attached_event.pane_id
    } else {
        None
    };

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
    Some(ClassifiedMouseEvent {
        key,
        event: attached_event,
        focus_target,
    })
}

pub(crate) fn copy_mode_mouse_context(
    event: &AttachedMouseEvent,
    pane: PaneGeometry,
    slider_mpos: i32,
) -> Option<CopyModeMouseContext> {
    event.pane_id?;

    let adjusted_y = match event.status_at {
        Some(0) if event.raw.y >= event.status_lines => event.raw.y - event.status_lines,
        _ => event.raw.y,
    };
    if adjusted_y < pane.y() {
        return None;
    }

    let relative_x = event.raw.x.saturating_sub(pane.x());
    let relative_y = adjusted_y.saturating_sub(pane.y());
    let content_x = u32::from(relative_x.min(pane.cols().saturating_sub(1)));
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
    })
}

pub(crate) fn copy_mode_mouse_drag_start_context(
    event: &AttachedMouseEvent,
    pane: PaneGeometry,
    slider_mpos: i32,
) -> Option<CopyModeMouseContext> {
    const MOUSE_MASK_DRAG: u16 = 32;

    let mut current = copy_mode_mouse_context(event, pane, slider_mpos)?;
    if event.raw.b & MOUSE_MASK_DRAG == 0 {
        return Some(current);
    }

    let mut anchor_event = event.clone();
    anchor_event.raw.b = event.raw.lb;
    anchor_event.raw.sgr_b = event.raw.lb;
    anchor_event.raw.x = event.raw.lx;
    anchor_event.raw.y = event.raw.ly;
    let anchor = copy_mode_mouse_context(&anchor_event, pane, slider_mpos)?;
    current.selection_anchor = Some((anchor.content_x, anchor.content_y));
    Some(current)
}

fn is_mouse_move(raw: MouseForwardEvent) -> bool {
    (raw.sgr_type != ' ' && mouse_drag(raw.sgr_b) && mouse_release(raw.sgr_b))
        || (raw.sgr_type == ' '
            && mouse_drag(raw.b)
            && mouse_release(raw.b)
            && mouse_release(raw.lb))
}

fn build_classified_event(
    kind: MouseEventKind,
    event: AttachedMouseEvent,
    button: u64,
    dragging: bool,
    _layout: &MouseLayout,
) -> Option<ClassifiedMouseEvent> {
    let key = if matches!(kind, MouseEventKind::MouseDrag) && dragging {
        KEYC_DRAGGING
    } else {
        synthesize_mouse_key(kind, button, event.location)? | modifier_bits(event.raw.b)
    };
    Some(ClassifiedMouseEvent {
        key,
        event,
        focus_target: None,
    })
}

fn synthesize_mouse_key(
    kind: MouseEventKind,
    button: u64,
    location: MouseLocation,
) -> Option<KeyCode> {
    let suffix = match location {
        MouseLocation::Pane => "Pane",
        MouseLocation::Status => "Status",
        MouseLocation::StatusLeft => "StatusLeft",
        MouseLocation::StatusRight => "StatusRight",
        MouseLocation::StatusDefault => "StatusDefault",
        MouseLocation::ScrollbarUp => "ScrollbarUp",
        MouseLocation::ScrollbarSlider => "ScrollbarSlider",
        MouseLocation::ScrollbarDown => "ScrollbarDown",
        MouseLocation::Border => "Border",
        MouseLocation::Control(value) => {
            return key_string_lookup_string(&format!(
                "{}{}Control{}",
                mouse_prefix(kind),
                button_string(kind, button),
                value
            ))
        }
        MouseLocation::Nowhere => return None,
    };
    key_string_lookup_string(&format!(
        "{}{}{}",
        mouse_prefix(kind),
        button_string(kind, button),
        suffix
    ))
}

fn button_string(kind: MouseEventKind, button: u64) -> String {
    if matches!(
        kind,
        MouseEventKind::MouseMove | MouseEventKind::WheelDown | MouseEventKind::WheelUp
    ) {
        String::new()
    } else {
        button.to_string()
    }
}

fn mouse_prefix(kind: MouseEventKind) -> &'static str {
    match kind {
        MouseEventKind::MouseMove => "MouseMove",
        MouseEventKind::MouseDown => "MouseDown",
        MouseEventKind::MouseUp => "MouseUp",
        MouseEventKind::MouseDrag => "MouseDrag",
        MouseEventKind::MouseDragEnd => "MouseDragEnd",
        MouseEventKind::WheelDown => "WheelDown",
        MouseEventKind::WheelUp => "WheelUp",
        MouseEventKind::SecondClick => "SecondClick",
        MouseEventKind::DoubleClick => "DoubleClick",
        MouseEventKind::TripleClick => "TripleClick",
    }
}

fn modifier_bits(button: u16) -> KeyCode {
    let mut key = 0;
    if (button & MOUSE_MASK_META) != 0 {
        key |= KEYC_META;
    }
    if (button & MOUSE_MASK_CTRL) != 0 {
        key |= KEYC_CTRL;
    }
    if (button & MOUSE_MASK_SHIFT) != 0 {
        key |= KEYC_SHIFT;
    }
    key
}

fn button_number(button: u16) -> u64 {
    match button {
        MOUSE_BUTTON_1 => 1,
        MOUSE_BUTTON_2 => 2,
        MOUSE_BUTTON_3 => 3,
        MOUSE_BUTTON_6 => 6,
        MOUSE_BUTTON_7 => 7,
        MOUSE_BUTTON_8 => 8,
        MOUSE_BUTTON_9 => 9,
        MOUSE_BUTTON_10 => 10,
        MOUSE_BUTTON_11 => 11,
        _ => 0,
    }
}

fn mouse_buttons(button: u16) -> u16 {
    button & MOUSE_MASK_BUTTONS
}

fn mouse_wheel(button: u16) -> bool {
    let button = mouse_buttons(button);
    button == MOUSE_WHEEL_UP || button == MOUSE_WHEEL_DOWN
}

fn mouse_drag(button: u16) -> bool {
    (button & MOUSE_MASK_DRAG) != 0
}

fn mouse_release(button: u16) -> bool {
    mouse_buttons(button) == 3
}

fn parse_pane_border_status(value: Option<&str>) -> PaneBorderStatus {
    match value {
        Some("top") => PaneBorderStatus::Top,
        Some("bottom") => PaneBorderStatus::Bottom,
        _ => PaneBorderStatus::Off,
    }
}

fn parse_pane_scrollbars_mode(value: Option<&str>) -> PaneScrollbarsMode {
    match value {
        Some("modal") => PaneScrollbarsMode::Modal,
        Some("on") => PaneScrollbarsMode::On,
        _ => PaneScrollbarsMode::Off,
    }
}

fn parse_scrollbar_position(value: Option<&str>) -> ScrollbarPosition {
    match value {
        Some("left") => ScrollbarPosition::Left,
        _ => ScrollbarPosition::Right,
    }
}

fn parse_scrollbar_style(value: Option<&str>) -> (u16, u16) {
    let mut width = 1;
    let mut pad = 0;
    for token in value.unwrap_or_default().split(',').map(str::trim) {
        if let Some(parsed) = token.strip_prefix("width=") {
            if let Ok(parsed) = parsed.parse::<u16>() {
                width = parsed;
            }
        } else if let Some(parsed) = token.strip_prefix("pad=") {
            if let Ok(parsed) = parsed.parse::<u16>() {
                pad = parsed;
            }
        }
    }
    (width, pad)
}

#[cfg(test)]
mod tests;
