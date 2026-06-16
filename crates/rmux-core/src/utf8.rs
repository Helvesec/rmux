//! tmux-aligned UTF-8 width, combining, and truncation rules.

use crate::OptionStore;
use rmux_proto::OptionName;
use unicode_width::UnicodeWidthChar;

const UTF8_ZWJ: char = '\u{200D}';
const UTF8_VS16: char = '\u{FE0F}';
const HANGUL_FILLER: char = '\u{3164}';
const MAX_COMBINED_BYTES: usize = 21;

const DEFAULT_WIDTH_OVERRIDES: &[WidthOverride] = &[
    WidthOverride::single(0x261D, 2),
    WidthOverride::single(0x26F9, 2),
    WidthOverride::new(0x270A, 0x270D, 2),
    WidthOverride::new(0x1F1E6, 0x1F1FF, 1),
    WidthOverride::single(0x1F385, 2),
    WidthOverride::new(0x1F3C2, 0x1F3C4, 2),
    WidthOverride::single(0x1F3C7, 2),
    WidthOverride::new(0x1F3CA, 0x1F3CC, 2),
    WidthOverride::new(0x1F3FB, 0x1F3FF, 2),
    WidthOverride::new(0x1F442, 0x1F443, 2),
    WidthOverride::new(0x1F446, 0x1F450, 2),
    WidthOverride::new(0x1F466, 0x1F469, 2),
    WidthOverride::new(0x1F46B, 0x1F46E, 2),
    WidthOverride::new(0x1F470, 0x1F478, 2),
    WidthOverride::single(0x1F47C, 2),
    WidthOverride::new(0x1F481, 0x1F483, 2),
    WidthOverride::new(0x1F485, 0x1F487, 2),
    WidthOverride::single(0x1F48F, 2),
    WidthOverride::single(0x1F491, 2),
    WidthOverride::single(0x1F4AA, 2),
    WidthOverride::new(0x1F574, 0x1F575, 2),
    WidthOverride::single(0x1F57A, 2),
    WidthOverride::single(0x1F590, 2),
    WidthOverride::new(0x1F595, 0x1F596, 2),
    WidthOverride::new(0x1F645, 0x1F647, 2),
    WidthOverride::new(0x1F64B, 0x1F64F, 2),
    WidthOverride::single(0x1F6A3, 2),
    WidthOverride::new(0x1F6B4, 0x1F6B6, 2),
    WidthOverride::single(0x1F6C0, 2),
    WidthOverride::single(0x1F6CC, 2),
    WidthOverride::single(0x1F90C, 2),
    WidthOverride::single(0x1F90F, 2),
    WidthOverride::new(0x1F918, 0x1F91F, 2),
    WidthOverride::single(0x1F926, 2),
    WidthOverride::new(0x1F930, 0x1F939, 2),
    WidthOverride::new(0x1F93D, 0x1F93E, 2),
    WidthOverride::single(0x1F977, 2),
    WidthOverride::new(0x1F9B5, 0x1F9B6, 2),
    WidthOverride::new(0x1F9B8, 0x1F9B9, 2),
    WidthOverride::single(0x1F9BB, 2),
    WidthOverride::new(0x1F9CD, 0x1F9CF, 2),
    WidthOverride::new(0x1F9D1, 0x1F9DD, 2),
    WidthOverride::new(0x1FAC3, 0x1FAC5, 2),
    WidthOverride::new(0x1FAF0, 0x1FAF8, 2),
];

/// tmux-compatible runtime width configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Utf8Config {
    variation_selector_always_wide: bool,
    overrides: Vec<WidthOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WidthOverride {
    start: u32,
    end: u32,
    width: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextCell {
    text: String,
    width: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CombineResult {
    Standalone { width: u8 },
    Combined { text: String, width: u8 },
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HangulJamoState {
    NotComposable,
    Choseong,
    Composable,
    NotHangulJamo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HangulJamoClass {
    NotHangulJamo,
    Choseong,
    Jungseong,
    Jongseong,
}

impl Default for Utf8Config {
    fn default() -> Self {
        Self {
            variation_selector_always_wide: true,
            overrides: DEFAULT_WIDTH_OVERRIDES.to_vec(),
        }
    }
}

impl Utf8Config {
    /// Resolves the current tmux-style width configuration from options.
    #[must_use]
    pub fn from_options(options: &OptionStore) -> Self {
        let mut config = Self {
            variation_selector_always_wide: options
                .resolve(None, OptionName::VariationSelectorAlwaysWide)
                .map(option_flag_is_on)
                .unwrap_or(true),
            ..Self::default()
        };
        for entry in options.resolve_array_values(None, OptionName::CodepointWidths) {
            if let Some(width_override) = parse_width_override(&entry) {
                config.overrides.push(width_override);
            }
        }
        config
    }

    pub(crate) fn width(&self, ch: char) -> u8 {
        let codepoint = u32::from(ch);
        if let Some(width_override) = self
            .overrides
            .iter()
            .rev()
            .find(|override_| override_.contains(codepoint))
        {
            return width_override.width;
        }
        fallback_width(ch)
    }
}

impl WidthOverride {
    const fn new(start: u32, end: u32, width: u8) -> Self {
        Self { start, end, width }
    }

    const fn single(codepoint: u32, width: u8) -> Self {
        Self::new(codepoint, codepoint, width)
    }

    const fn contains(self, codepoint: u32) -> bool {
        self.start <= codepoint && codepoint <= self.end
    }
}

/// Returns the tmux-style display width of a string.
#[must_use]
pub fn text_width(value: &str, config: &Utf8Config) -> usize {
    fold_text_cells(value, config)
        .iter()
        .map(|cell| usize::from(cell.width))
        .sum()
}

/// Truncates a string to the requested display width.
#[must_use]
pub fn truncate_to_width(value: &str, width: usize, config: &Utf8Config) -> String {
    let mut output = String::new();
    let mut used = 0_usize;

    for cell in fold_text_cells(value, config) {
        let cell_width = usize::from(cell.width);
        if cell_width != 0 && used.saturating_add(cell_width) > width {
            break;
        }
        output.push_str(&cell.text);
        used = used.saturating_add(cell_width);
    }

    output
}

/// Truncates a string from the left, keeping the rightmost text cells that fit.
#[must_use]
pub fn truncate_right_to_width(value: &str, width: usize, config: &Utf8Config) -> String {
    let cells = fold_text_cells(value, config);
    let mut used = 0_usize;
    let mut start = cells.len();

    for (index, cell) in cells.iter().enumerate().rev() {
        let cell_width = usize::from(cell.width);
        if cell_width != 0 && used.saturating_add(cell_width) > width {
            break;
        }
        used = used.saturating_add(cell_width);
        start = index;
    }

    cells[start..]
        .iter()
        .map(|cell| cell.text.as_str())
        .collect()
}

pub(crate) fn combine_char(
    previous: Option<(&str, u8)>,
    ch: char,
    config: &Utf8Config,
) -> CombineResult {
    if ch == HANGUL_FILLER {
        return CombineResult::Discard;
    }

    let width = config.width(ch);
    let zero_width = ch == UTF8_ZWJ || ch == UTF8_VS16 || width == 0;

    if ch.len_utf8() < 2 {
        return CombineResult::Standalone { width };
    }

    let Some((previous_text, previous_width)) = previous else {
        return if zero_width {
            CombineResult::Discard
        } else {
            CombineResult::Standalone { width }
        };
    };
    if previous_width == 0 || previous_text.is_empty() {
        return if zero_width {
            CombineResult::Discard
        } else {
            CombineResult::Standalone { width }
        };
    }

    let mut force_wide = false;
    if !zero_width {
        match hanguljamo_check_state(previous_text, ch) {
            HangulJamoState::NotComposable => return CombineResult::Discard,
            HangulJamoState::Choseong => return CombineResult::Standalone { width },
            HangulJamoState::Composable => {}
            HangulJamoState::NotHangulJamo => {
                let should_force_wide = single_codepoint(previous_text)
                    .is_some_and(|previous_ch| utf8_should_combine(previous_ch, ch));
                if should_force_wide {
                    force_wide = true;
                } else if !utf8_has_zwj(previous_text) {
                    return CombineResult::Standalone { width };
                }
            }
        }
    } else if ch == UTF8_VS16 && config.variation_selector_always_wide {
        force_wide = true;
    }

    if previous_text.len().saturating_add(ch.len_utf8()) > MAX_COMBINED_BYTES {
        return CombineResult::Standalone { width };
    }

    let mut text = previous_text.to_owned();
    text.push(ch);

    let width = if previous_width == 1 && force_wide {
        2
    } else {
        previous_width
    };

    CombineResult::Combined { text, width }
}

fn fold_text_cells(value: &str, config: &Utf8Config) -> Vec<TextCell> {
    let mut cells: Vec<TextCell> = Vec::new();

    for ch in value.chars() {
        let previous = cells.last().map(|cell| (cell.text.as_str(), cell.width));
        match combine_char(previous, ch, config) {
            CombineResult::Standalone { width } => {
                cells.push(TextCell {
                    text: ch.to_string(),
                    width,
                });
            }
            CombineResult::Combined { text, width } => {
                if let Some(cell) = cells.last_mut() {
                    cell.text = text;
                    cell.width = width;
                }
            }
            CombineResult::Discard => {}
        }
    }

    cells
}

fn option_flag_is_on(value: &str) -> bool {
    matches!(value, "on" | "1")
}

fn parse_width_override(value: &str) -> Option<WidthOverride> {
    let (codepoint_text, width_text) = value.rsplit_once('=')?;
    let width = width_text.parse::<u8>().ok()?;
    if width > 2 {
        return None;
    }

    if let Some((start, end)) = parse_uplus_range(codepoint_text) {
        return Some(WidthOverride::new(start, end, width));
    }

    let mut chars = codepoint_text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(WidthOverride::single(u32::from(ch), width))
}

fn parse_uplus_range(value: &str) -> Option<(u32, u32)> {
    let parse_hex = |text: &str| u32::from_str_radix(text, 16).ok();

    let (start, end) = match value.split_once('-') {
        Some((start, end)) => (start, end),
        None => (value, value),
    };
    let start = start.strip_prefix("U+")?;
    let end = end.strip_prefix("U+")?;
    let start = parse_hex(start)?;
    let end = parse_hex(end)?;
    if start == 0 || end == 0 || start > end {
        return None;
    }
    Some((start, end))
}

fn fallback_width(ch: char) -> u8 {
    if hanguljamo_class(ch) != HangulJamoClass::NotHangulJamo {
        return 2;
    }
    match UnicodeWidthChar::width(ch) {
        Some(width) => u8::try_from(width).unwrap_or(1),
        None if is_c1_control(ch) => 0,
        None => 1,
    }
}

fn is_c1_control(ch: char) -> bool {
    let codepoint = u32::from(ch);
    (0x80..=0x9F).contains(&codepoint)
}

fn utf8_has_zwj(value: &str) -> bool {
    value.ends_with(UTF8_ZWJ)
}

fn single_codepoint(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(ch)
}

fn utf8_should_combine(with: char, add: char) -> bool {
    let with = u32::from(with);
    let add = u32::from(add);

    if is_regional_indicator(add) && is_regional_indicator(with) {
        return true;
    }

    emoji_accepts_skin_tone(with) && is_skin_tone_modifier(add)
}

fn is_regional_indicator(codepoint: u32) -> bool {
    (0x1F1E6..=0x1F1FF).contains(&codepoint)
}

fn is_skin_tone_modifier(codepoint: u32) -> bool {
    (0x1F3FB..=0x1F3FF).contains(&codepoint)
}

fn emoji_accepts_skin_tone(codepoint: u32) -> bool {
    matches!(
        codepoint,
        0x1F44B
            | 0x1F44C
            | 0x1F44D
            | 0x1F44E
            | 0x1F44F
            | 0x1F450
            | 0x1F466
            | 0x1F467
            | 0x1F468
            | 0x1F469
            | 0x1F46E
            | 0x1F470
            | 0x1F471
            | 0x1F472
            | 0x1F473
            | 0x1F474
            | 0x1F475
            | 0x1F476
            | 0x1F477
            | 0x1F478
            | 0x1F47C
            | 0x1F481
            | 0x1F482
            | 0x1F483
            | 0x1F485
            | 0x1F486
            | 0x1F487
            | 0x1F4AA
            | 0x1F575
            | 0x1F57A
            | 0x1F590
            | 0x1F595
            | 0x1F596
            | 0x1F645
            | 0x1F646
            | 0x1F647
            | 0x1F64B
            | 0x1F64C
            | 0x1F64D
            | 0x1F64E
            | 0x1F64F
            | 0x1F6B4
            | 0x1F6B5
            | 0x1F6B6
            | 0x1F926
            | 0x1F937
            | 0x1F938
            | 0x1F939
            | 0x1F93D
            | 0x1F93E
            | 0x1F9B5
            | 0x1F9B6
            | 0x1F9B8
            | 0x1F9B9
            | 0x1F9CD
            | 0x1F9CE
            | 0x1F9CF
            | 0x1F9D1
            | 0x1F9D2
            | 0x1F9D3
            | 0x1F9D4
            | 0x1F9D5
            | 0x1F9D6
            | 0x1F9D7
            | 0x1F9D8
            | 0x1F9D9
            | 0x1F9DA
            | 0x1F9DB
            | 0x1F9DC
            | 0x1F9DD
            | 0x1F9DE
            | 0x1F9DF
    )
}

fn hanguljamo_check_state(previous_text: &str, ch: char) -> HangulJamoState {
    if ch.len_utf8() != 3 {
        return HangulJamoState::NotHangulJamo;
    }

    match hanguljamo_class(ch) {
        HangulJamoClass::Choseong => HangulJamoState::Choseong,
        HangulJamoClass::Jungseong => match previous_text.chars().last() {
            Some(last)
                if last.len_utf8() == 3 && hanguljamo_class(last) == HangulJamoClass::Choseong =>
            {
                HangulJamoState::Composable
            }
            _ => HangulJamoState::NotComposable,
        },
        HangulJamoClass::Jongseong => match previous_text.chars().last() {
            Some(last)
                if last.len_utf8() == 3 && hanguljamo_class(last) == HangulJamoClass::Jungseong =>
            {
                HangulJamoState::Composable
            }
            _ => HangulJamoState::NotComposable,
        },
        HangulJamoClass::NotHangulJamo => HangulJamoState::NotHangulJamo,
    }
}

fn hanguljamo_class(ch: char) -> HangulJamoClass {
    let codepoint = u32::from(ch);
    if matches!(
        codepoint,
        0x1100..=0x115E | 0x115F | 0xA960..=0xA97C
    ) {
        HangulJamoClass::Choseong
    } else if matches!(
        codepoint,
        0x1160 | 0x1161..=0x11A7 | 0xD7B0..=0xD7C6
    ) {
        HangulJamoClass::Jungseong
    } else if matches!(codepoint, 0x11A8..=0x11FF | 0xD7CB..=0xD7FB) {
        HangulJamoClass::Jongseong
    } else {
        HangulJamoClass::NotHangulJamo
    }
}

#[cfg(test)]
#[path = "utf8/tests.rs"]
mod tests;
