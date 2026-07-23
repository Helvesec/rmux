use rmux_core::{
    key_string_lookup_string, KeyCode, KEYC_CTRL, KEYC_DRAGGING, KEYC_META, KEYC_SHIFT,
};

use crate::input_keys::MouseForwardEvent;

use super::types::{AttachedMouseEvent, ClassifiedMouseEvent, MouseEventKind, MouseLocation};
use super::{
    MouseLayout, MOUSE_BUTTON_1, MOUSE_BUTTON_10, MOUSE_BUTTON_11, MOUSE_BUTTON_2, MOUSE_BUTTON_3,
    MOUSE_BUTTON_6, MOUSE_BUTTON_7, MOUSE_BUTTON_8, MOUSE_BUTTON_9, MOUSE_MASK_BUTTONS,
    MOUSE_MASK_CTRL, MOUSE_MASK_DRAG, MOUSE_MASK_META, MOUSE_MASK_SHIFT, MOUSE_WHEEL_DOWN,
    MOUSE_WHEEL_UP,
};

pub(super) fn is_mouse_move(raw: MouseForwardEvent) -> bool {
    (raw.sgr_type != ' ' && mouse_drag(raw.sgr_b) && mouse_release(raw.sgr_b))
        || (raw.sgr_type == ' '
            && mouse_drag(raw.b)
            && mouse_release(raw.b)
            && mouse_release(raw.lb))
}

pub(super) fn build_classified_event(
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

pub(super) fn synthesize_mouse_key(
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
            ));
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

pub(super) fn modifier_bits(button: u16) -> KeyCode {
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

pub(super) fn button_number(button: u16) -> u64 {
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

pub(super) fn mouse_buttons(button: u16) -> u16 {
    button & MOUSE_MASK_BUTTONS
}

pub(super) fn mouse_wheel(button: u16) -> bool {
    let button = mouse_buttons(button);
    button == MOUSE_WHEEL_UP || button == MOUSE_WHEEL_DOWN
}

pub(super) fn mouse_drag(button: u16) -> bool {
    (button & MOUSE_MASK_DRAG) != 0
}

pub(super) fn mouse_release(button: u16) -> bool {
    mouse_buttons(button) == 3
}
