//! X11 report colour syntax, distinct from tmux's style colour syntax.
mod component;
mod names;

fn c_space(character: char) -> bool {
    matches!(character, ' ' | '\t'..='\r')
}

pub(super) fn unsigned(input: &mut &[u8], radix: u32, width: usize) -> Option<u32> {
    while input.first().is_some_and(|byte| c_space(char::from(*byte))) {
        *input = &input[1..];
    }
    let field = &input[..input.len().min(width)];
    let mut at = 0;
    let negative = field.first() == Some(&b'-');
    if matches!(field.first(), Some(b'+' | b'-')) {
        at += 1;
    }
    if radix == 16
        && field
            .get(at..at + 2)
            .is_some_and(|s| s.eq_ignore_ascii_case(b"0x"))
    {
        at += 2;
    }
    let start = at;
    let mut value = 0u32;
    while let Some(digit) = field.get(at).and_then(|b| char::from(*b).to_digit(radix)) {
        value = value.checked_mul(radix)?.checked_add(digit)?;
        at += 1;
    }
    if at == start {
        return None;
    }
    *input = &input[at..];
    Some(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

fn triple(
    mut input: &[u8],
    radix: u32,
    width: usize,
    separator: Option<u8>,
    shift: u32,
) -> Option<[u8; 3]> {
    let mut rgb = [0; 3];
    for (index, channel) in rgb.iter_mut().enumerate() {
        if index != 0 {
            if let Some(separator) = separator {
                input = input.strip_prefix(&[separator])?;
            }
        }
        *channel = (unsigned(&mut input, radix, width)? >> shift) as u8;
    }
    Some(rgb)
}

pub(super) fn parse(value: &str) -> Option<[u8; 3]> {
    let bytes = value.as_bytes();
    let hex = match bytes.len() {
        12 => bytes
            .strip_prefix(b"rgb:")
            .and_then(|v| triple(v, 16, 2, Some(b'/'), 0)),
        7 => bytes
            .strip_prefix(b"#")
            .and_then(|v| triple(v, 16, 2, None, 0)),
        _ => None,
    };
    if hex.is_some() {
        return hex;
    }
    if let Some(rgb) = triple(bytes, 10, usize::MAX, Some(b','), 0) {
        return Some(rgb);
    }
    let hex = match bytes.len() {
        18 => bytes
            .strip_prefix(b"rgb:")
            .and_then(|v| triple(v, 16, 4, Some(b'/'), 8)),
        13 => bytes
            .strip_prefix(b"#")
            .and_then(|v| triple(v, 16, 4, None, 8)),
        _ => None,
    };
    if hex.is_some() {
        return hex;
    }
    let cmy = value
        .strip_prefix("cmy:")
        .map(|s| (s, false))
        .or_else(|| value.strip_prefix("cmyk:").map(|s| (s, true)));
    if let Some((mut rest, has_black)) = cmy {
        let cyan = component::parse(&mut rest)?;
        rest = rest.strip_prefix('/')?;
        let magenta = component::parse(&mut rest)?;
        rest = rest.strip_prefix('/')?;
        let yellow = component::parse(&mut rest)?;
        let black = if has_black {
            rest = rest.strip_prefix('/')?;
            component::parse(&mut rest)?
        } else {
            0.0
        };
        return Some([cyan, magenta, yellow].map(|c| ((1.0 - c) * (1.0 - black) * 255.0) as u8));
    }
    let name = value.trim_matches(' ');
    if name
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("grey") || s.eq_ignore_ascii_case("gray"))
    {
        let suffix = &name[4..];
        if suffix.is_empty() {
            return Some([190; 3]);
        }
        let number = suffix.trim_start_matches(c_space).parse::<u8>().ok()?;
        return (number <= 100).then(|| [(2.55 * f64::from(number)).round() as u8; 3]);
    }
    names::COLOURS
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, rgb)| [(rgb >> 16) as u8, (rgb >> 8) as u8, *rgb as u8])
}
