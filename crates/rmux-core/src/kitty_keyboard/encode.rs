use super::event::{Error, Event, Key, Kind, Text, FRAME_LIMIT};
use super::state::effective_flags;

/// Encodes an event using the canonical selected Kitty keyboard profile.
///
/// `requested` is the requested pane flag value; bit 16 is treated as
/// ineffective without bit 8 while the requested state itself remains intact.
/// The runtime uses this profile for negotiated events and its existing
/// terminfo/xterm encoder when the effective pane flags are zero.
pub fn encode_event(event: &Event, requested: u8) -> Result<Vec<u8>, Error> {
    event.validate()?;
    let flags = effective_flags(requested);
    let all_keys = flags & 8 != 0;
    let report_events = flags & 2 != 0;
    let release = event.kind == Kind::Release;

    if release && (!report_events || (event.key.is_c0_exception() && !all_keys)) {
        return Ok(Vec::new());
    }

    let plain_text_key = matches!(event.key, Key::Scalar(scalar) if !scalar.is_control())
        || event.key == Key::TextOnly;
    if release && !all_keys && plain_text_key && event.modifiers & !(1 | 64 | 128) == 0 {
        return Ok(Vec::new());
    }

    if let Key::Function(function) = event.key {
        let name = function.name();
        let modifier_key = name.contains("SHIFT")
            || name.contains("CONTROL")
            || name.contains("SUPER")
            || name.contains("HYPER")
            || name.contains("META")
            || name.ends_with("_ALT");
        if modifier_key && !all_keys {
            return Ok(Vec::new());
        }
    }

    if !all_keys && !event.text.values().is_empty() {
        return if release {
            Ok(Vec::new())
        } else {
            Ok(event.text.values().iter().collect::<String>().into_bytes())
        };
    }

    let (number, final_byte) = event.key.preferred();
    let c0_exception = event.key.is_c0_exception();
    let tab_erratum = number == 9 && final_byte == b'u' && event.modifiers == 5 && flags & 1 != 0;
    let event_suffix = report_events && event.kind != Kind::Press;

    if c0_exception && !all_keys && !tab_erratum && !event_suffix {
        return encode_legacy_c0(number, event.modifiers);
    }

    if !all_keys && !event_suffix && final_byte != b'u' && event.modifiers == 0 {
        if let Key::Function(function) = event.key {
            if let Some(suffix) = match function.name() {
                "F1" => Some(b'P'),
                "F2" => Some(b'Q'),
                "F3" => Some(b'R'),
                "F4" => Some(b'S'),
                _ => None,
            } {
                return Ok(vec![0x1b, b'O', suffix]);
            }
        }
    }

    if !all_keys && !event_suffix && final_byte == b'u' && !c0_exception {
        if number == 27 && flags & 1 == 0 {
            return encode_legacy_c0(number, event.modifiers);
        }
        if let Key::Scalar(scalar) = event.key {
            if flags & 1 == 0 && matches!(event.modifiers, 0..=4 | 6) {
                if matches!(event.modifiers, 0 | 1) && matches!(event.text, Text::Unavailable) {
                    return Err(Error::UnavailableText);
                }
                if matches!(event.modifiers, 2 | 3) && !scalar.is_control() {
                    // Alternate reporting changes an already-required CSI;
                    // it must not turn an ordinary Alt key into CSI-u.
                    let scalar = if event.modifiers & 1 != 0 {
                        event
                            .shifted
                            .or_else(|| {
                                scalar
                                    .is_ascii_alphabetic()
                                    .then(|| scalar.to_ascii_uppercase())
                            })
                            .ok_or(Error::UnavailableText)?
                    } else {
                        scalar
                    };
                    let mut bytes = vec![27];
                    bytes.extend_from_slice(scalar.encode_utf8(&mut [0; 4]).as_bytes());
                    return Ok(bytes);
                }
                if scalar.is_ascii() && event.modifiers & 4 != 0 {
                    let byte = legacy_control_byte(scalar as u8);
                    return Ok(if event.modifiers & 2 != 0 {
                        vec![27, byte]
                    } else {
                        vec![byte]
                    });
                }
            }
        }
    }

    let mut output = String::from("\x1b[");
    let text = if flags & 16 != 0 && !release {
        event.text.values()
    } else {
        &[]
    };
    let has_modifier = event.modifiers != 0 || event_suffix || !text.is_empty();

    if final_byte == b'u' || final_byte == b'~' || has_modifier {
        output.push_str(&number.to_string());
    }
    if final_byte == b'u' && flags & 4 != 0 && (event.shifted.is_some() || event.base.is_some()) {
        output.push(':');
        if let Some(shifted) = event.shifted {
            output.push_str(&(shifted as u32).to_string());
        }
        if let Some(base) = event.base {
            output.push(':');
            output.push_str(&(base as u32).to_string());
        }
    }
    if has_modifier {
        output.push(';');
        output.push_str(&(u16::from(event.modifiers) + 1).to_string());
        if event_suffix {
            output.push(':');
            output.push_str(&(event.kind as u8).to_string());
        }
    }
    if !text.is_empty() {
        if final_byte != b'u' {
            return Err(Error::UnknownIdentity);
        }
        output.push(';');
        for (index, scalar) in text.iter().enumerate() {
            if index != 0 {
                output.push(':');
            }
            output.push_str(&(*scalar as u32).to_string());
        }
    }
    output.push(char::from(final_byte));

    if output.len() > FRAME_LIMIT {
        return Err(Error::FrameLimit);
    }
    Ok(output.into_bytes())
}

fn encode_legacy_c0(number: u32, modifiers: u8) -> Result<Vec<u8>, Error> {
    if modifiers > 7 {
        return Err(Error::UnknownIdentity);
    }
    let mut output = if modifiers & 2 != 0 {
        vec![27]
    } else {
        Vec::new()
    };
    match number {
        9 if modifiers & 1 != 0 => output.extend_from_slice(b"\x1b[Z"),
        127 if modifiers & 4 != 0 => output.push(8),
        9 | 13 | 27 | 127 => output.push(number as u8),
        _ => return Err(Error::UnknownIdentity),
    }
    Ok(output)
}

fn legacy_control_byte(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => byte - b'a' + 1,
        b' ' | b'@' | b'2' => 0,
        b'3' | b'[' => 27,
        b'4' | b'\\' => 28,
        b'5' | b']' => 29,
        b'6' | b'^' | b'~' => 30,
        b'7' | b'/' | b'_' => 31,
        b'8' | b'?' => 127,
        _ => byte,
    }
}
