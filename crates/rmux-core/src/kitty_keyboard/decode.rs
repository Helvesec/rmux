use super::event::{Error, Event, Key, Kind, Text, FRAME_LIMIT, TEXT_LIMIT};
use super::state::{Apply, Control};

/// Parses one already-delimited complete Kitty keyboard event frame.
///
/// This function accepts only 7-bit `ESC [` framing. Stream classification,
/// incremental buffering, timeout, recovery, and foreign CSI routing belong to
/// the caller that identified the complete frame.
pub fn parse_event_frame(frame: &[u8]) -> Result<Event, Error> {
    validate_frame_prefix(frame)?;
    let final_byte = *frame.last().ok_or(Error::InvalidFrame)?;
    if !b"u~ABCDEFHPQS".contains(&final_byte) {
        return Err(Error::InvalidFrame);
    }

    let payload = frame_payload(frame)?;
    let fields: Vec<_> = payload.split(';').collect();
    if fields.len() > if final_byte == b'u' { 3 } else { 2 } {
        return Err(Error::Fields);
    }

    let keys: Vec<_> = fields[0].split(':').collect();
    if keys.len() > if final_byte == b'u' { 3 } else { 1 } {
        return Err(Error::Fields);
    }
    let number = parse_decimal(keys[0], (!b"u~".contains(&final_byte)).then_some(1))?;
    let key = Key::from_form(number, final_byte)?;

    let modifier_fields: Vec<_> = fields.get(1).unwrap_or(&"1").split(':').collect();
    if modifier_fields.len() > 2 {
        return Err(Error::Fields);
    }
    let wire_modifiers = parse_decimal(modifier_fields[0], Some(1))?;
    if !(1..=256).contains(&wire_modifiers) {
        return Err(Error::ModifierRange);
    }
    let kind = Kind::try_from(parse_decimal(
        modifier_fields.get(1).unwrap_or(&"1"),
        Some(1),
    )?)?;

    let text = parse_text(fields.get(2))?;
    let event = Event {
        key,
        modifiers: (wire_modifiers - 1) as u8,
        kind,
        shifted: parse_alternate(keys.get(1))?,
        base: parse_alternate(keys.get(2))?,
        text,
    };
    event.validate()?;
    Ok(event)
}

/// Parses one already-delimited complete Kitty keyboard control frame.
///
/// The accepted forms are query (`CSI ? u`), set (`CSI = flags ; mode u`),
/// push (`CSI > flags u`), and pop (`CSI < count u`).
pub fn parse_control_frame(frame: &[u8]) -> Result<Control, Error> {
    validate_frame_prefix(frame)?;
    if frame.len() < 4 || frame.last() != Some(&b'u') {
        return Err(Error::InvalidFrame);
    }

    let operation = frame[2];
    let body = std::str::from_utf8(&frame[3..frame.len() - 1]).map_err(|_| Error::InvalidFrame)?;
    if operation == b'?' {
        return if body.is_empty() {
            Ok(Control::Query)
        } else {
            Err(Error::Fields)
        };
    }

    let mut fields = body.split(';');
    let value = parse_decimal(
        fields.next().unwrap_or(""),
        Some(if operation == b'<' { 1 } else { 0 }),
    )?;
    let mode = fields.next();
    if fields.next().is_some() || (operation != b'=' && mode.is_some()) {
        return Err(Error::Fields);
    }

    match operation {
        b'>' => Ok(Control::Push(value)),
        b'<' => Ok(Control::Pop(value)),
        b'=' => {
            let mode = match parse_decimal(mode.unwrap_or("1"), Some(1))? {
                1 => Apply::Replace,
                2 => Apply::Add,
                3 => Apply::Remove,
                _ => return Err(Error::InvalidFrame),
            };
            Ok(Control::Set { flags: value, mode })
        }
        _ => Err(Error::InvalidFrame),
    }
}

fn validate_frame_prefix(frame: &[u8]) -> Result<(), Error> {
    if frame.len() > FRAME_LIMIT {
        return Err(Error::FrameLimit);
    }
    if !frame.starts_with(b"\x1b[") || frame.len() < 3 {
        return Err(Error::InvalidFrame);
    }
    Ok(())
}

fn frame_payload(frame: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(&frame[2..frame.len() - 1]).map_err(|_| Error::InvalidFrame)
}

fn parse_decimal(raw: &str, default: Option<u32>) -> Result<u32, Error> {
    if raw.is_empty() {
        return default.ok_or(Error::MissingKey);
    }
    raw.bytes().try_fold(0_u32, |value, byte| {
        if !byte.is_ascii_digit() {
            return Err(Error::Decimal);
        }
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(byte - b'0')))
            .ok_or(Error::DecimalOverflow)
    })
}

fn parse_alternate(raw: Option<&&str>) -> Result<Option<char>, Error> {
    match raw {
        None | Some(&"") => Ok(None),
        Some(raw) => char::from_u32(parse_decimal(raw, None)?)
            .map(Some)
            .ok_or(Error::Scalar),
    }
}

fn parse_text(raw: Option<&&str>) -> Result<Text, Error> {
    match raw {
        None => Ok(Text::Unavailable),
        Some(&"") => Text::known(Vec::new()),
        Some(raw) => {
            let mut values = Vec::new();
            for field in raw.split(':') {
                if values.len() == TEXT_LIMIT {
                    return Err(Error::TextLimit);
                }
                let scalar = char::from_u32(parse_decimal(field, None)?).ok_or(Error::Scalar)?;
                values.push(scalar);
            }
            Text::known(values)
        }
    }
}
