use rmux_core::input::mode;

const MOUSE_PARAM_MAX: u16 = 0xff;
const MOUSE_PARAM_UTF8_MAX: u16 = 0x7ff;
const MOUSE_PARAM_BTN_OFF: u16 = 0x20;
const MOUSE_PARAM_POS_OFF: u16 = 0x21;
// Three maximal u16 fields plus separators and a terminator need 21 bytes.
// Keep a little syntax headroom, but never rescan an attacker-sized retained
// decimal on every input fragment. Once this bound is reached, discard the
// current malformed input batch so its tail cannot become prompt, menu, or
// key-binding input after the control opener is removed.
pub(crate) const MAX_SGR_MOUSE_FRAME_BYTES: usize = 32;

const MOUSE_MASK_BUTTONS: u16 = 195;
const MOUSE_MASK_DRAG: u16 = 32;
const MOUSE_WHEEL_UP: u16 = 64;
const MOUSE_WHEEL_DOWN: u16 = 65;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MouseForwardEvent {
    pub(crate) b: u16,
    pub(crate) lb: u16,
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) lx: u16,
    pub(crate) ly: u16,
    pub(crate) sgr_b: u16,
    pub(crate) sgr_type: char,
    pub(crate) ignore: bool,
}

impl MouseForwardEvent {
    #[cfg(test)]
    pub(super) fn button_event(b: u16, x: u16, y: u16) -> Self {
        Self {
            b,
            lb: b,
            x,
            y,
            lx: x,
            ly: y,
            sgr_b: b,
            sgr_type: ' ',
            ignore: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MouseDecode {
    Invalid,
    Partial,
    /// The syntactic mouse prefix is intact, but a decimal field has already
    /// exceeded the representable range. Keep it on the timed control path
    /// until it terminates or reaches the fixed syntax-consumption bound.
    Overlong,
    Discard {
        size: usize,
    },
    Matched {
        size: usize,
        event: MouseForwardEvent,
    },
}

pub(crate) fn encode_mouse_event(
    pane_mode: u32,
    event: &MouseForwardEvent,
    x: u16,
    y: u16,
) -> Option<Vec<u8>> {
    if event.ignore || (pane_mode & mode::ALL_MOUSE_MODES) == 0 {
        return None;
    }

    if mouse_drag(event.b) && (pane_mode & motion_mouse_modes()) == 0 {
        return None;
    }
    if sgr_release_discard(event, pane_mode) {
        return None;
    }

    // Only use SGR encoding when both the application requested it AND the
    // source event was SGR format. A legacy mouse release cannot be converted
    // to SGR because the released button identity is unknown.
    if event.sgr_type != ' ' && (pane_mode & mode::MODE_MOUSE_SGR) != 0 {
        return Some(
            format!(
                "\x1b[<{};{};{}{}",
                event.sgr_b,
                x + 1,
                y + 1,
                event.sgr_type
            )
            .into_bytes(),
        );
    }

    if (pane_mode & mode::MODE_MOUSE_UTF8) != 0 {
        if event.b > MOUSE_PARAM_UTF8_MAX - MOUSE_PARAM_BTN_OFF
            || x > MOUSE_PARAM_UTF8_MAX - MOUSE_PARAM_POS_OFF
            || y > MOUSE_PARAM_UTF8_MAX - MOUSE_PARAM_POS_OFF
        {
            return None;
        }
        let mut bytes = b"\x1b[M".to_vec();
        append_utf8_mouse_value(event.b + MOUSE_PARAM_BTN_OFF, &mut bytes);
        append_utf8_mouse_value(x + MOUSE_PARAM_POS_OFF, &mut bytes);
        append_utf8_mouse_value(y + MOUSE_PARAM_POS_OFF, &mut bytes);
        return Some(bytes);
    }

    if event.b + MOUSE_PARAM_BTN_OFF > MOUSE_PARAM_MAX {
        return None;
    }

    let mut bytes = b"\x1b[M".to_vec();
    bytes.push((event.b + MOUSE_PARAM_BTN_OFF) as u8);
    bytes.push((x + MOUSE_PARAM_POS_OFF).min(MOUSE_PARAM_MAX) as u8);
    bytes.push((y + MOUSE_PARAM_POS_OFF).min(MOUSE_PARAM_MAX) as u8);
    Some(bytes)
}

pub(crate) fn decode_mouse(input: &[u8], last: Option<MouseForwardEvent>) -> MouseDecode {
    if input.first() != Some(&0x1b) {
        return MouseDecode::Invalid;
    }
    if input.len() == 1 {
        return MouseDecode::Partial;
    }
    if input[1] != b'[' {
        return MouseDecode::Invalid;
    }
    if input.len() == 2 {
        return MouseDecode::Partial;
    }

    if input[2] == b'M' {
        if input.len() < 6 {
            return MouseDecode::Partial;
        }
        let b = u16::from(input[3]);
        let x = u16::from(input[4]);
        let y = u16::from(input[5]);
        if b < MOUSE_PARAM_BTN_OFF || x < MOUSE_PARAM_POS_OFF || y < MOUSE_PARAM_POS_OFF {
            return MouseDecode::Discard { size: 6 };
        }
        let previous = last.unwrap_or(MouseForwardEvent {
            b: 0,
            lb: 0,
            x: 0,
            y: 0,
            lx: 0,
            ly: 0,
            sgr_b: 0,
            sgr_type: ' ',
            ignore: false,
        });
        return MouseDecode::Matched {
            size: 6,
            event: MouseForwardEvent {
                lx: previous.x,
                ly: previous.y,
                lb: previous.b,
                b: b - MOUSE_PARAM_BTN_OFF,
                x: x - MOUSE_PARAM_POS_OFF,
                y: y - MOUSE_PARAM_POS_OFF,
                sgr_b: 0,
                sgr_type: ' ',
                ignore: false,
            },
        };
    }

    if input[2] != b'<' {
        return MouseDecode::Invalid;
    }

    let sgr_input = &input[..input.len().min(MAX_SGR_MOUSE_FRAME_BYTES)];
    let (b, offset_after_b, mut overflowed) = match parse_mouse_decimal(sgr_input, 3, b";") {
        MouseDecimalDecode::Matched {
            value,
            end,
            overflowed,
        } => (value, end, overflowed),
        MouseDecimalDecode::Partial => return incomplete_sgr_mouse(input.len(), false),
        MouseDecimalDecode::Overlong => return incomplete_sgr_mouse(input.len(), true),
        MouseDecimalDecode::Invalid => return MouseDecode::Invalid,
    };
    let (x, offset_after_x, x_overflowed) =
        match parse_mouse_decimal(sgr_input, offset_after_b + 1, b";") {
            MouseDecimalDecode::Matched {
                value,
                end,
                overflowed,
            } => (value, end, overflowed),
            MouseDecimalDecode::Partial => {
                return incomplete_sgr_mouse(input.len(), overflowed);
            }
            MouseDecimalDecode::Overlong => return incomplete_sgr_mouse(input.len(), true),
            MouseDecimalDecode::Invalid => return MouseDecode::Invalid,
        };
    overflowed |= x_overflowed;
    let (y, offset_after_y, y_overflowed) =
        match parse_mouse_decimal(sgr_input, offset_after_x + 1, b"Mm") {
            MouseDecimalDecode::Matched {
                value,
                end,
                overflowed,
            } => (value, end, overflowed),
            MouseDecimalDecode::Partial => {
                return incomplete_sgr_mouse(input.len(), overflowed);
            }
            MouseDecimalDecode::Overlong => return incomplete_sgr_mouse(input.len(), true),
            MouseDecimalDecode::Invalid => return MouseDecode::Invalid,
        };
    overflowed |= y_overflowed;
    let terminator = input[offset_after_y];
    if overflowed {
        return MouseDecode::Discard {
            size: offset_after_y + 1,
        };
    }
    if x < 1 || y < 1 {
        return MouseDecode::Discard {
            size: offset_after_y + 1,
        };
    }
    if terminator == b'm' && mouse_wheel(b) {
        return MouseDecode::Discard {
            size: offset_after_y + 1,
        };
    }

    let previous = last.unwrap_or(MouseForwardEvent {
        b: 0,
        lb: 0,
        x: 0,
        y: 0,
        lx: 0,
        ly: 0,
        sgr_b: 0,
        sgr_type: ' ',
        ignore: false,
    });
    let sgr_b = b;
    let b = if terminator == b'm' { 3 } else { b };
    MouseDecode::Matched {
        size: offset_after_y + 1,
        event: MouseForwardEvent {
            lx: previous.x,
            ly: previous.y,
            lb: previous.b,
            b,
            x: x - 1,
            y: y - 1,
            sgr_b,
            sgr_type: terminator as char,
            ignore: false,
        },
    }
}

fn incomplete_sgr_mouse(input_len: usize, overflowed: bool) -> MouseDecode {
    if input_len >= MAX_SGR_MOUSE_FRAME_BYTES {
        return MouseDecode::Discard { size: input_len };
    }
    if overflowed {
        MouseDecode::Overlong
    } else {
        MouseDecode::Partial
    }
}

fn motion_mouse_modes() -> u32 {
    mode::MODE_MOUSE_BUTTON | mode::MODE_MOUSE_ALL
}

fn sgr_release_discard(event: &MouseForwardEvent, pane_mode: u32) -> bool {
    if event.sgr_type != ' ' {
        return mouse_drag(event.sgr_b)
            && mouse_release(event.sgr_b)
            && (pane_mode & mode::MODE_MOUSE_ALL) == 0;
    }
    mouse_drag(event.b)
        && mouse_release(event.b)
        && mouse_release(event.lb)
        && (pane_mode & mode::MODE_MOUSE_ALL) == 0
}

fn append_utf8_mouse_value(value: u16, output: &mut Vec<u8>) {
    if value <= 0x7f {
        output.push(value as u8);
        return;
    }

    output.push((0b1100_0000 | ((value >> 6) as u8)) & 0b1101_1111);
    output.push(0b1000_0000 | (value as u8 & 0b0011_1111));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseDecimalDecode {
    Partial,
    Overlong,
    Invalid,
    Matched {
        value: u16,
        end: usize,
        overflowed: bool,
    },
}

fn parse_mouse_decimal(input: &[u8], start: usize, terminators: &[u8]) -> MouseDecimalDecode {
    let mut index = start;
    let mut value = 0_u16;
    let mut has_digit = false;
    let mut overflowed = false;
    while let Some(&byte) = input.get(index) {
        if terminators.contains(&byte) {
            return if has_digit {
                MouseDecimalDecode::Matched {
                    value,
                    end: index,
                    overflowed,
                }
            } else {
                MouseDecimalDecode::Invalid
            };
        }
        if !byte.is_ascii_digit() {
            return MouseDecimalDecode::Invalid;
        }
        has_digit = true;
        if !overflowed {
            match value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u16::from(byte - b'0')))
            {
                Some(next) => value = next,
                None => overflowed = true,
            }
        }
        index += 1;
    }
    if overflowed {
        MouseDecimalDecode::Overlong
    } else {
        MouseDecimalDecode::Partial
    }
}

fn mouse_wheel(button: u16) -> bool {
    let buttons = mouse_buttons(button);
    buttons == MOUSE_WHEEL_UP || buttons == MOUSE_WHEEL_DOWN
}

fn mouse_drag(button: u16) -> bool {
    (button & MOUSE_MASK_DRAG) != 0
}

fn mouse_release(button: u16) -> bool {
    mouse_buttons(button) == 3
}

fn mouse_buttons(button: u16) -> u16 {
    button & MOUSE_MASK_BUTTONS
}
