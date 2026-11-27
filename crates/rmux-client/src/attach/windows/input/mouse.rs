use windows_sys::Win32::System::Console::{
    FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED, FROM_LEFT_3RD_BUTTON_PRESSED,
    LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED,
    MOUSE_WHEELED, RIGHTMOST_BUTTON_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
};

// SGR mouse button codes (xterm). Modifiers are OR-ed in; drag adds 32.
const SGR_BTN_LEFT: u16 = 0;
const SGR_BTN_MIDDLE: u16 = 1;
const SGR_BTN_RIGHT: u16 = 2;
const SGR_BTN_NONE: u16 = 3;
const SGR_WHEEL_UP: u16 = 64;
const SGR_WHEEL_DOWN: u16 = 65;
const SGR_WHEEL_LEFT: u16 = 66;
const SGR_WHEEL_RIGHT: u16 = 67;
const SGR_MOD_SHIFT: u16 = 4;
const SGR_MOD_ALT: u16 = 8;
const SGR_MOD_CTRL: u16 = 16;
const SGR_DRAG_FLAG: u16 = 32;

/// Encode a Win32 console `MOUSE_EVENT_RECORD` into SGR mouse sequences
/// (`\x1b[<b;x;yM` press/wheel, `\x1b[<b;x;ym` release). The rmux server gates these
/// against the active mouse mode, so we always emit and let the server decide.
pub(super) fn encode_mouse_event(
    event: MOUSE_EVENT_RECORD,
    last_button_state: &mut u32,
) -> Vec<Vec<u8>> {
    let x = u16::try_from(event.dwMousePosition.X.max(0)).unwrap_or(0) + 1;
    let y = u16::try_from(event.dwMousePosition.Y.max(0)).unwrap_or(0) + 1;
    let buttons = event.dwButtonState;
    let modifiers = sgr_modifier_bits(event.dwControlKeyState);

    if event.dwEventFlags & MOUSE_WHEELED != 0 {
        // High word of dwButtonState is the signed wheel delta; positive = up.
        let delta = (buttons >> 16) as i16;
        let base = if delta >= 0 {
            SGR_WHEEL_UP
        } else {
            SGR_WHEEL_DOWN
        };
        return vec![format_sgr_mouse(base | modifiers, x, y, 'M')];
    }
    if event.dwEventFlags & MOUSE_HWHEELED != 0 {
        // High word of dwButtonState is the signed horizontal wheel delta;
        // Windows reports positive = right, while SGR 66/67 are left/right.
        let delta = (buttons >> 16) as i16;
        let base = if delta >= 0 {
            SGR_WHEEL_RIGHT
        } else {
            SGR_WHEEL_LEFT
        };
        return vec![format_sgr_mouse(base | modifiers, x, y, 'M')];
    }

    // Ignore bare double-click flags; movement is handled below.
    let previous = *last_button_state;
    let pressed = buttons & !previous;
    let released = previous & !buttons;
    *last_button_state = buttons;

    let is_move = event.dwEventFlags & MOUSE_MOVED != 0;

    if pressed == 0 && released == 0 {
        if is_move {
            // Drag (motion with a button held) or bare hover motion.
            if let Some(button) = sgr_button_for_state(buttons) {
                return vec![format_sgr_mouse(
                    button | SGR_DRAG_FLAG | modifiers,
                    x,
                    y,
                    'M',
                )];
            }
            return vec![format_sgr_mouse(
                SGR_BTN_NONE | SGR_DRAG_FLAG | modifiers,
                x,
                y,
                'M',
            )];
        }
        return Vec::new();
    }

    let mut output = Vec::new();
    for button in sgr_buttons_for_state(released) {
        output.push(format_sgr_mouse(button | modifiers, x, y, 'm'));
    }
    for button in sgr_buttons_for_state(pressed) {
        output.push(format_sgr_mouse(button | modifiers, x, y, 'M'));
    }
    output
}

fn sgr_button_for_state(buttons: u32) -> Option<u16> {
    if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
        Some(SGR_BTN_LEFT)
    } else if buttons & RIGHTMOST_BUTTON_PRESSED != 0 {
        Some(SGR_BTN_RIGHT)
    } else if buttons & (FROM_LEFT_2ND_BUTTON_PRESSED | FROM_LEFT_3RD_BUTTON_PRESSED) != 0 {
        Some(SGR_BTN_MIDDLE)
    } else {
        None
    }
}

fn sgr_buttons_for_state(buttons: u32) -> Vec<u16> {
    let mut encoded = Vec::new();
    if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
        encoded.push(SGR_BTN_LEFT);
    }
    if buttons & RIGHTMOST_BUTTON_PRESSED != 0 {
        encoded.push(SGR_BTN_RIGHT);
    }
    if buttons & (FROM_LEFT_2ND_BUTTON_PRESSED | FROM_LEFT_3RD_BUTTON_PRESSED) != 0 {
        encoded.push(SGR_BTN_MIDDLE);
    }
    encoded
}

fn sgr_modifier_bits(control_key_state: u32) -> u16 {
    let mut bits = 0;
    if control_key_state & SHIFT_PRESSED != 0 {
        bits |= SGR_MOD_SHIFT;
    }
    if control_key_state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        bits |= SGR_MOD_ALT;
    }
    if control_key_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        bits |= SGR_MOD_CTRL;
    }
    bits
}

fn format_sgr_mouse(button: u16, x: u16, y: u16, terminator: char) -> Vec<u8> {
    format!("\x1b[<{button};{x};{y}{terminator}").into_bytes()
}
