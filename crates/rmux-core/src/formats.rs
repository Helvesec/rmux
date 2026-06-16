//! tmux-compatible format expansion engine.
//!
//! This module implements the core of tmux's `format_expand1` / `format_replace`
//! pipeline: modifier parsing, nesting-aware delimiter scanning (`format_skip`),
//! comparisons, boolean operators, multi-pair conditionals, quoting, literal mode,
//! expand mode, and recursion-limited re-expansion.
//!
//! Runtime-only features still deferred here are loops `S`/`W`/`P`/`L`,
//! expression arithmetic `e`, search `C`, and `#(cmd)` jobs. Time formatting,
//! regex substitution, `a`/`c`/`w`/`N` modifiers, and tmux single-char aliases
//! are handled in this core expansion layer.

use crate::style::parse_colour;
use crate::utf8::{text_width, Utf8Config};
#[path = "formats/colour.rs"]
mod colour;
#[path = "formats/condition.rs"]
mod condition;
#[path = "formats/context.rs"]
mod context;
#[path = "formats/expand.rs"]
mod expand;
#[path = "formats/expression.rs"]
mod expression;
#[path = "formats/glob.rs"]
mod glob;
#[path = "formats/modifiers.rs"]
mod modifiers;
#[path = "formats/scan.rs"]
mod scan;
#[path = "formats/time.rs"]
mod time;
#[path = "formats/transforms.rs"]
mod transforms;

use condition::{format_bool_op_n, format_conditional};
pub use context::{
    is_known_format_variable_name, FormatContext, FormatVariable, FormatVariables,
    DEFAULT_DISPLAY_MESSAGE_FORMAT, DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_FORMAT,
    DEFAULT_LIST_PANES_SESSION_FORMAT, DEFAULT_LIST_PANES_WINDOW_FORMAT,
    DEFAULT_LIST_SESSIONS_FORMAT, DEFAULT_LIST_WINDOWS_ALL_FORMAT, DEFAULT_LIST_WINDOWS_FORMAT,
    FORMAT_VARIABLES, TMUX_FORMAT_TABLE_NAMES, TMUX_TIME_FORMAT_VARIABLE_NAMES,
};
use expand::{format_expand1, FORMAT_LOOP_LIMIT};
use expression::format_expression;
use glob::format_fnmatch;
use modifiers::{parse_modifiers, FormatModifier};
use scan::format_skip;
pub use scan::format_skip_delimiter;
pub use time::expand_time_tokens;
use time::format_time_string;
use transforms::{
    apply_substitution, format_unescape, shell_quote, style_quote, truncate_left, truncate_right,
};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Renders a format template against supported format variables.
#[must_use]
pub fn render_template<V>(template: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let mut state = ExpandState {
        loop_depth: 0,
        expand_time: false,
        stop_expansion: false,
        preserve_jobs: false,
    };
    format_expand1(&mut state, template, variables)
}

/// Renders a format template while preserving `#(...)` command jobs literally.
///
/// Runtime renderers that can actually execute jobs use this mode to keep jobs
/// introduced by expanded option values, such as `#{T:status-left}`, available
/// for a later execution pass.
#[must_use]
pub fn render_template_preserving_jobs<V>(template: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let mut state = ExpandState {
        loop_depth: 0,
        expand_time: false,
        stop_expansion: false,
        preserve_jobs: true,
    };
    format_expand1(&mut state, template, variables)
}

/// Renders a `list-windows` line using the default format when no format is supplied.
#[must_use]
pub fn render_list_windows_line<V>(variables: &V, format: Option<&str>) -> String
where
    V: FormatVariables + ?Sized,
{
    render_template(format.unwrap_or(DEFAULT_LIST_WINDOWS_FORMAT), variables)
}

/// Renders a `list-sessions` line using the default format when no format is supplied.
#[must_use]
pub fn render_list_sessions_line<V>(variables: &V, format: Option<&str>) -> String
where
    V: FormatVariables + ?Sized,
{
    render_template(format.unwrap_or(DEFAULT_LIST_SESSIONS_FORMAT), variables)
}

/// Renders a `list-panes` line using the default format when no format is supplied.
#[must_use]
pub fn render_list_panes_line<V>(variables: &V, format: Option<&str>) -> String
where
    V: FormatVariables + ?Sized,
{
    render_template(format.unwrap_or(DEFAULT_LIST_PANES_FORMAT), variables)
}

/// Returns whether a conditional format value is truthy.
///
/// Matches tmux `format_true`: non-empty and not exactly `"0"`.
#[must_use]
pub fn is_truthy(value: &str) -> bool {
    !value.is_empty() && value != "0"
}

// ---------------------------------------------------------------------------
// Expansion state
// ---------------------------------------------------------------------------

struct ExpandState {
    loop_depth: u32,
    expand_time: bool,
    stop_expansion: bool,
    preserve_jobs: bool,
}

// ---------------------------------------------------------------------------
// format_choose — split body into left,right on first comma
// ---------------------------------------------------------------------------

/// Splits `body` into left and right operands at the first `,` delimiter
/// (nesting-aware). Both sides are expanded. Returns `None` if no delimiter found.
fn format_choose<V>(state: &mut ExpandState, body: &str, variables: &V) -> Option<(String, String)>
where
    V: FormatVariables + ?Sized,
{
    let bytes = body.as_bytes();
    let pos = format_skip(bytes, b",")?;
    let left_raw = &body[..pos];
    let right_raw = &body[pos + 1..];
    let left = format_expand1(state, left_raw, variables);
    let right = format_expand1(state, right_raw, variables);
    Some((left, right))
}

// ---------------------------------------------------------------------------
// format_replace — the modifier pipeline dispatcher
// ---------------------------------------------------------------------------

/// Bitflags for modifier effects.
const MOD_LITERAL: u32 = 1 << 0;
const MOD_EXPAND: u32 = 1 << 1;
const MOD_QUOTE_SHELL: u32 = 1 << 4;
const MOD_QUOTE_STYLE: u32 = 1 << 5;
const MOD_BASENAME: u32 = 1 << 6;
const MOD_DIRNAME: u32 = 1 << 7;
const MOD_LENGTH: u32 = 1 << 8;
const MOD_EXPAND_TIME: u32 = 1 << 9;

/// Processes the content inside `#{...}`, applying modifiers and returning the
/// expanded result.
fn format_replace<V>(state: &mut ExpandState, key: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let (modifiers, body) = parse_modifiers(state, key, variables);

    // Classify modifiers.
    let mut flags: u32 = 0;
    let mut cmp: Option<&FormatModifier> = None;
    let mut bool_op_n: Option<&FormatModifier> = None;
    let mut limit: i32 = 0;
    let mut limit_marker: Option<&str> = None;
    let mut width: i32 = 0;
    let mut subs: Vec<&FormatModifier> = Vec::new();
    let mut time_string = false;
    let mut time_pretty = false;
    let mut time_format: Option<String> = None;
    let mut deferred_loop_scope = None;
    let mut ascii_char = false;
    let mut colour_hex = false;
    let mut display_width = false;
    let mut name_exists: Option<&FormatModifier> = None;
    let mut expression: Option<&FormatModifier> = None;
    let mut search: Option<&FormatModifier> = None;

    for fm in &modifiers {
        if fm.modifier.len() == 1 {
            match fm.modifier.as_bytes()[0] {
                b'm' | b'<' | b'>' => cmp = Some(fm),
                b'a' => ascii_char = true,
                b'c' => colour_hex = true,
                b's' if fm.argv.len() >= 2 => {
                    subs.push(fm);
                }
                b'=' => {
                    if let Some(arg) = fm.argv.first() {
                        limit = arg.parse::<i32>().unwrap_or(0);
                        if fm.argv.len() >= 2 {
                            limit_marker = fm.argv.get(1).map(String::as_str);
                        }
                    }
                }
                b'p' => {
                    if let Some(arg) = fm.argv.first() {
                        width = arg.parse::<i32>().unwrap_or(0);
                    }
                }
                b'l' => flags |= MOD_LITERAL,
                b'b' => flags |= MOD_BASENAME,
                b'd' => flags |= MOD_DIRNAME,
                b'n' => flags |= MOD_LENGTH,
                b'q' => {
                    if fm.argv.is_empty() {
                        flags |= MOD_QUOTE_SHELL;
                    } else if let Some(arg) = fm.argv.first() {
                        if arg.contains('e') || arg.contains('h') {
                            flags |= MOD_QUOTE_STYLE;
                        }
                    }
                }
                b'E' => flags |= MOD_EXPAND,
                b'T' => flags |= MOD_EXPAND_TIME,
                b't' => {
                    time_string = true;
                    if let Some(arg) = fm.argv.first() {
                        if arg.contains('p') {
                            time_pretty = true;
                        } else if arg.contains('f') {
                            time_format = fm.argv.get(1).cloned();
                        }
                    }
                }
                b'S' | b'W' | b'P' | b'L' => {
                    deferred_loop_scope = Some(fm.modifier.as_bytes()[0] as char)
                }
                b'N' => name_exists = Some(fm),
                b'e' => expression = Some(fm),
                b'C' => search = Some(fm),
                b'w' => display_width = true,
                // Runtime-deferred: R
                _ => {}
            }
        } else if fm.modifier.len() == 2 {
            match fm.modifier.as_str() {
                "||" | "&&" => bool_op_n = Some(fm),
                "==" | "!=" | "<=" | ">=" => cmp = Some(fm),
                _ => {}
            }
        }
    }

    if let Some(scope) = deferred_loop_scope {
        let (body, current_body) = if scope == 'S' {
            (body, None)
        } else {
            split_loop_body(body)
        };
        if let Some(value) = variables.format_loop(scope, body, current_body, false) {
            return value;
        }
    }
    if let Some(modifier) = name_exists {
        let scope = match modifier.argv.first().map(String::as_str) {
            None | Some("") | Some("w") => None,
            Some("s") => Some('s'),
            Some(_) => return String::new(),
        };
        let operand = format_expand1(state, body, variables);
        return variables
            .format_name_exists(scope, &operand)
            .map(bool_value)
            .unwrap_or_default();
    }
    if let Some(modifier) = search {
        let options = modifier
            .argv
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        let pattern = format_expand1(state, body, variables);
        return variables
            .format_search(options, &pattern)
            .unwrap_or_default();
    }

    // --- Dispatch with classified modifiers ---

    // Literal.
    if flags & MOD_LITERAL != 0 {
        return format_unescape(body);
    }

    let value;

    if let Some(op) = bool_op_n {
        // N-ary boolean operator.
        let is_and = op.modifier == "&&";
        value = format_bool_op_n(state, body, is_and, variables);
    } else if let Some(cmp_mod) = cmp {
        // Comparison.
        value = match format_choose(state, body, variables) {
            Some((left, right)) => {
                let result = match cmp_mod.modifier.as_str() {
                    "==" => left == right,
                    "!=" => left != right,
                    "<" => left < right,
                    "<=" => left <= right,
                    ">" => left > right,
                    ">=" => left >= right,
                    "m" => format_fnmatch(&left, &right, cmp_mod),
                    _ => false,
                };
                if result { "1" } else { "0" }.to_owned()
            }
            None => String::new(),
        };
    } else if let Some(cond_body) = body.strip_prefix('?') {
        // Multi-pair conditional.
        value = format_conditional(state, cond_body, variables);
    } else if let Some(expression) = expression {
        value = format_expression(state, body, expression, variables);
    } else {
        // Variable lookup.
        if body.contains("#{") {
            value = format_expand1(state, body, variables);
        } else {
            value = resolve_variable(body, variables);
        }
    }

    // Post-processing pipeline.
    let mut result = value;

    if time_string {
        result =
            format_time_string(&result, time_pretty, time_format.as_deref()).unwrap_or_default();
    }

    // Expand modifier (re-expand the resolved value).
    if flags & MOD_EXPAND != 0 {
        result = format_expand1(state, &result, variables);
        result = expand_time_tokens(&result);
    } else if flags & MOD_EXPAND_TIME != 0 {
        let previous = state.expand_time;
        state.expand_time = true;
        result = format_expand1(state, &result, variables);
        state.expand_time = previous;
    }

    // Substitutions.
    for sub in &subs {
        if sub.argv.len() >= 2 {
            result = apply_substitution(&result, sub);
        }
    }

    // Truncation.
    if limit > 0 {
        let truncated = truncate_left(&result, limit as usize);
        if truncated != result {
            if let Some(marker) = limit_marker {
                result = format!("{truncated}{marker}");
            } else {
                result = truncated;
            }
        } else {
            result = truncated;
        }
    } else if limit < 0 {
        let truncated = truncate_right(&result, (-limit) as usize);
        if truncated != result {
            if let Some(marker) = limit_marker {
                result = format!("{marker}{truncated}");
            } else {
                result = truncated;
            }
        } else {
            result = truncated;
        }
    }

    // Padding.
    if width > 0 {
        let w = width as usize;
        let current_width = text_width(&result, &Utf8Config::default());
        if current_width < w {
            result.push_str(&" ".repeat(w - current_width));
        }
    } else if width < 0 {
        let w = (-width) as usize;
        let current_width = text_width(&result, &Utf8Config::default());
        if current_width < w {
            result = format!("{}{result}", " ".repeat(w - current_width));
        }
    }

    // Basename.
    if flags & MOD_BASENAME != 0 {
        if let Some(pos) = result.rfind('/') {
            result = result[pos + 1..].to_owned();
        }
    }

    // Dirname.
    if flags & MOD_DIRNAME != 0 {
        result = format_dirname(&result);
    }

    // Length.
    if flags & MOD_LENGTH != 0 {
        result = result.len().to_string();
    }

    if display_width {
        result = format_display_width(&result, body, state, variables);
    }

    if ascii_char {
        result = format_ascii_character(&result, body, state, variables);
    }

    if colour_hex {
        result = format_colour_hex(&result, body, state, variables);
    }

    // Quoting.
    if flags & MOD_QUOTE_SHELL != 0 && !is_single_nested_expansion(body) {
        result = shell_quote(&result);
    } else if flags & MOD_QUOTE_STYLE != 0 {
        result = style_quote(&result);
    }

    result
}

fn split_loop_body(body: &str) -> (&str, Option<&str>) {
    format_skip(body.as_bytes(), b",")
        .map(|offset| (&body[..offset], Some(&body[offset + 1..])))
        .unwrap_or((body, None))
}

fn is_single_nested_expansion(body: &str) -> bool {
    body.starts_with("#{") && format_skip(body.as_bytes(), b"}") == Some(body.len() - 1)
}

fn format_ascii_character<V>(
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

fn format_colour_hex<V>(_result: &str, body: &str, state: &mut ExpandState, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let operand = format_expand1(state, body, variables);
    parse_colour(&operand)
        .ok()
        .and_then(colour::tmux_colour_to_rgb)
        .map(|(red, green, blue)| format!("{red:02x}{green:02x}{blue:02x}"))
        .unwrap_or_default()
}

fn format_display_width<V>(
    result: &str,
    _body: &str,
    _state: &mut ExpandState,
    _variables: &V,
) -> String
where
    V: FormatVariables + ?Sized,
{
    text_width(result, &Utf8Config::default()).to_string()
}

fn format_dirname(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value.chars().all(|character| character == '/') {
        return value.to_owned();
    }

    let trimmed = value.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) => "/".to_owned(),
        Some(position) => trimmed[..position].to_owned(),
        None => ".".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Variable resolution
// ---------------------------------------------------------------------------

fn resolve_variable<V>(name: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    variables.format_value_by_name(name).unwrap_or_default()
}

fn window_raw_flags(active: bool, last: bool) -> &'static str {
    if active {
        "*"
    } else if last {
        "-"
    } else {
        ""
    }
}

fn bool_value(value: bool) -> String {
    if value {
        "1".to_owned()
    } else {
        "0".to_owned()
    }
}

#[cfg(test)]
#[path = "formats/tests.rs"]
mod tests;
