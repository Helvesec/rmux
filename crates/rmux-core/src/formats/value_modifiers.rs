//! Modifiers that compute a new value from their operand: repeat (`R`),
//! ASCII character (`a`), colour hex (`c`) and display width (`w`).

use crate::style::parse_colour;
use crate::utf8::Utf8Config;

use super::{
    format_choose, format_expand1, styled_text_width, ExpandState, FormatVariables,
    FORMAT_PADDING_LIMIT,
};

const FORMAT_REPEAT_BYTES_LIMIT: usize = 5 * 1024 * 1024;

pub(super) fn format_repeat<V>(state: &mut ExpandState, body: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let Some((unit, count)) = format_choose(state, body, variables) else {
        state.stop_expansion = true;
        return String::new();
    };
    let Ok(count) = count.parse::<usize>() else {
        return String::new();
    };
    if count == 0 || count > FORMAT_PADDING_LIMIT || unit.is_empty() {
        return String::new();
    }
    let Some(capacity) = unit.len().checked_mul(count) else {
        return String::new();
    };
    if capacity > FORMAT_REPEAT_BYTES_LIMIT {
        return String::new();
    }
    let mut repeated = String::with_capacity(capacity);
    for _ in 0..count {
        repeated.push_str(&unit);
    }
    repeated
}

pub(super) fn format_ascii_character<V>(
    _result: &str,
    body: &str,
    state: &mut ExpandState,
    variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    let operand = format_expand1(state, body, variables);
    operand
        .parse::<u32>()
        .ok()
        .and_then(|value| u8::try_from(value).ok())
        .map(char::from)
        .map(|character| character.to_string())
        .unwrap_or_default()
}

pub(super) fn format_colour_hex<V>(
    _result: &str,
    body: &str,
    state: &mut ExpandState,
    variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    let operand = format_expand1(state, body, variables);
    parse_colour(&operand)
        .ok()
        .and_then(crate::input::colour_to_rgb)
        .map(|(red, green, blue)| format!("{red:02x}{green:02x}{blue:02x}"))
        .unwrap_or_default()
}

pub(super) fn format_display_width<V>(
    result: &str,
    _body: &str,
    _state: &mut ExpandState,
    _variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    styled_text_width(result, &Utf8Config::default()).to_string()
}
