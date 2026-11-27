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

use std::rc::Rc;

use crate::utf8::Utf8Config;
mod budget;
mod condition;
mod context;
mod expand;
mod expression;
mod glob;
mod modifiers;
mod path_names;
mod regex_cache;
mod registry;
mod resolve;
mod scan;
mod styled_text;
mod time;
mod transforms;
mod value_modifiers;
mod variable;

use budget::ExpandBudget;
use condition::{format_bool_op, format_conditional};
pub use context::{
    FormatContext, FormatVariables, DEFAULT_DISPLAY_MESSAGE_FORMAT, DEFAULT_LIST_PANES_ALL_FORMAT,
    DEFAULT_LIST_PANES_FORMAT, DEFAULT_LIST_PANES_SESSION_FORMAT, DEFAULT_LIST_PANES_WINDOW_FORMAT,
    DEFAULT_LIST_SESSIONS_FORMAT, DEFAULT_LIST_WINDOWS_ALL_FORMAT, DEFAULT_LIST_WINDOWS_FORMAT,
};
pub use expand::render_status_template;
use expand::{format_expand1, render_output, FORMAT_LOOP_LIMIT};
use expression::format_expression;
use glob::format_fnmatch;
use modifiers::{parse_modifiers, FormatModifier};
use path_names::{format_basename, format_dirname};
pub use registry::{
    display_all_format_variable_descriptors, format_variable_descriptor,
    format_variable_descriptors, is_known_format_variable_name,
    parse_time_format_variable_descriptors, tmux_format_variable_descriptors,
    tmux_time_format_variable_descriptors, FormatVariableContext, FormatVariableContexts,
    FormatVariableDescriptor, FormatVariableKey, FormatVariableProvider, TMUX_FORMAT_TABLE_NAMES,
    TMUX_TIME_FORMAT_VARIABLE_NAMES,
};
use resolve::{resolve_path_variable, resolve_variable};
use scan::format_skip;
pub use scan::format_skip_delimiter;
pub use styled_text::{styled_text_width, truncate_styled_text_to_width};
pub use time::expand_time_tokens;
use time::format_time_string;
use transforms::{
    apply_substitution, format_unescape, shell_quote, style_quote, truncate_left, truncate_right,
};
use value_modifiers::{
    format_ascii_character, format_colour_hex, format_display_width, format_repeat,
};
pub use variable::{FormatVariable, FORMAT_VARIABLES};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Renders a format template against supported format variables.
#[must_use]
pub fn render_template<V>(template: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    render_output(template, variables, false)
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
        status: false,
        loop_depth: 0,
        expand_time: false,
        stop_expansion: false,
        preserve_jobs: true,
        disable_jobs: false,
        budget: Rc::new(ExpandBudget::start()),
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
    /// This expansion emits text directly into the status bar.
    status: bool,
    loop_depth: u32,
    expand_time: bool,
    stop_expansion: bool,
    preserve_jobs: bool,
    disable_jobs: bool,
    /// Shared by every nested state of one top-level expansion.
    budget: Rc<ExpandBudget>,
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
const FORMAT_PADDING_LIMIT: usize = 10_000;

/// Processes the content inside `#{...}`, applying modifiers and returning the
/// expanded result.
fn format_replace<V>(state: &mut ExpandState, key: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    let status = std::mem::take(&mut state.status);
    let (modifiers, body, display_marker) = parse_modifiers(state, key, variables, status);

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
    let mut deferred_loop = None;
    let mut ascii_char = false;
    let mut colour_hex = false;
    let mut display_width = false;
    let mut name_exists: Option<&FormatModifier> = None;
    let mut expression: Option<&FormatModifier> = None;
    let mut search: Option<&FormatModifier> = None;
    let mut bool_unary: Option<&FormatModifier> = None;
    let mut repeat = false;

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
                b'S' | b'W' | b'P' | b'L' => deferred_loop = Some(fm),
                b'N' => name_exists = Some(fm),
                b'e' => expression = Some(fm),
                b'C' => search = Some(fm),
                b'w' => display_width = true,
                b'!' => bool_unary = Some(fm),
                b'R' => repeat = true,
                _ => {}
            }
        } else if fm.modifier.len() == 2 {
            match fm.modifier.as_str() {
                "||" | "&&" => bool_op_n = Some(fm),
                "!!" => bool_unary = Some(fm),
                "==" | "!=" | "<=" | ">=" => cmp = Some(fm),
                _ => {}
            }
        }
    }

    // Forward literal hashes through every text transformation. Only consumers
    // that evaluate operands rather than emit their text need raw values.
    let forward = status
        && flags & MOD_LITERAL == 0
        && cmp.is_none()
        && bool_op_n.is_none()
        && bool_unary.is_none()
        && expression.is_none()
        && name_exists.is_none()
        && search.is_none()
        && (deferred_loop.is_some()
            || flags & (MOD_EXPAND | MOD_EXPAND_TIME) != 0
            || body.starts_with('?')
            || body.contains("#{"));
    // An explicit re-expansion first reads its entire source as ordinary text.
    state.status = forward && flags & (MOD_EXPAND | MOD_EXPAND_TIME) == 0;
    let mut deferred_loop_value = None;
    if let Some(loop_modifier) = deferred_loop {
        let scope = loop_modifier.modifier.as_bytes()[0] as char;
        let (body, current_body) = if matches!(scope, 'S' | 'L') {
            (body, None)
        } else {
            split_loop_body(body)
        };
        let value = {
            let mut expand = |template: &str, variables: &dyn FormatVariables| {
                format_expand1(state, template, variables)
            };
            variables.format_loop_with_args_and_expand(
                scope,
                &loop_modifier.argv,
                body,
                current_body,
                false,
                &mut expand,
            )
        };
        if let Some(value) = value {
            if scope == 'L' {
                deferred_loop_value = Some(value);
            } else {
                return value;
            }
        }
    }
    let deferred_loop_present = deferred_loop_value.is_some();
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
    if flags & MOD_LITERAL != 0 && deferred_loop_value.is_none() {
        return format_unescape(body);
    }

    let value;

    if let Some(loop_value) = deferred_loop_value {
        value = loop_value;
    } else if let Some(op) = bool_unary {
        let truthy = is_truthy(&format_expand1(state, body, variables));
        value = match op.modifier.as_str() {
            "!" => bool_value(!truthy),
            "!!" => bool_value(truthy),
            _ => String::new(),
        };
    } else if repeat {
        value = format_repeat(state, body, variables);
    } else if let Some(op) = bool_op_n {
        // N-ary boolean operator.
        let is_and = op.modifier == "&&";
        value = format_bool_op(state, body, is_and, variables);
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
            // A path modifier reads the lookup that keeps absence; every other
            // body keeps the lookup it always had, whose absent value is the
            // empty string.
            let found = if let Some(text) = expand::status_field(
                status && flags & (MOD_EXPAND | MOD_EXPAND_TIME) == 0,
                body,
                variables,
            ) {
                state.status = true;
                Some(text)
            } else if flags & (MOD_BASENAME | MOD_DIRNAME) != 0 {
                resolve_path_variable(body, variables)
            } else {
                Some(resolve_variable(body, variables))
            };
            // The short name is taken here, on what the lookup found, and not
            // at the end of the pipeline. Taking it last meant a limit cut the
            // path first, so `#{=6;b:pane_title}` answered `work` for both
            // `/work/editor/` and `/work/logs/`. The engine this was measured
            // against takes it during the lookup too, which is why `#{=3;b:…}`
            // answers `edi` and a substitution, a padding or an expand all see
            // the short name. `resolve_variable` itself is untouched: the
            // conditions and the short aliases call it without these flags.
            // The folder follows the short name, on the same value and in the
            // same place: the engine measured applies both during its lookup,
            // before the expand, the substitution, the limit and the padding.
            // Taking the folder last meant a limit cut `/work/editor` out of
            // `/work/editor/main.rs` first, and `#{=12;d:pane_title}` answered
            // `/work`. Written `d;b` the two still run in this order.
            // Both run only on a value that was really FOUND. A title that is
            // there and holds nothing is filed under the current folder, as the
            // engine measured files it, while a name nobody answered has no
            // folder at all - the two were the same string before, which is why
            // a pane with an empty title had no row of its own in a menu of
            // parent folders.
            value = match found {
                Some(found) => {
                    let short = if flags & MOD_BASENAME != 0 {
                        format_basename(&found)
                    } else {
                        found
                    };
                    if flags & MOD_DIRNAME != 0 {
                        format_dirname(&short)
                    } else {
                        short
                    }
                }
                // Nothing answered to this name, so there is no value for
                // either name to be taken from: the answer is what an absent
                // value always gave, and never the current folder.
                None => String::new(),
            };
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
        state.status = forward;
        result = format_expand1(state, &result, variables);
        result = expand_time_tokens(&result);
    } else if flags & MOD_EXPAND_TIME != 0 {
        state.status = forward;
        let previous = state.expand_time;
        state.expand_time = true;
        result = format_expand1(state, &result, variables);
        state.expand_time = previous;
    }

    // Substitutions.
    for sub in &subs {
        if sub.argv.len() >= 2 {
            result = apply_substitution(&result, sub, state.status);
        }
    }

    // Preserve literal hashes from the marker alongside a raw operand.
    if display_marker && !state.status && limit_marker.is_some_and(|m| m.contains('\0')) {
        if let std::borrow::Cow::Owned(text) = expand::raw_text(&result, true) {
            result = text;
        }
        state.status = true;
    }
    if limit > 0 {
        let truncated = truncate_left(&result, limit as usize);
        if let Some(marker) = limit_marker.filter(|_| truncated != result) {
            result = format!("{truncated}{marker}");
        } else {
            result = truncated;
        }
    } else if limit < 0 {
        let truncated = truncate_right(&result, signed_i32_abs_usize(limit));
        if let Some(marker) = limit_marker.filter(|_| truncated != result) {
            result = format!("{marker}{truncated}");
        } else {
            result = truncated;
        }
    }

    // Padding.
    if width > 0 {
        if let Some(w) = bounded_format_padding_width(width) {
            let current_width = styled_text_width(&result, &Utf8Config::default());
            if current_width < w {
                result.push_str(&" ".repeat(w - current_width));
            }
        }
    } else if width < 0 {
        if let Some(w) = bounded_format_padding_width(width) {
            let current_width = styled_text_width(&result, &Utf8Config::default());
            if current_width < w {
                result = format!("{}{result}", " ".repeat(w - current_width));
            }
        }
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
    if flags & MOD_QUOTE_SHELL != 0 && (!is_single_nested_expansion(body) || deferred_loop_present)
    {
        result = shell_quote(&result);
    } else if flags & MOD_QUOTE_STYLE != 0 {
        result = style_quote(&result);
    }

    result
}

fn signed_i32_abs_usize(value: i32) -> usize {
    value.unsigned_abs() as usize
}

fn bounded_format_padding_width(value: i32) -> Option<usize> {
    let width = signed_i32_abs_usize(value);
    (width <= FORMAT_PADDING_LIMIT).then_some(width)
}

fn split_loop_body(body: &str) -> (&str, Option<&str>) {
    format_skip(body.as_bytes(), b",")
        .map(|offset| (&body[..offset], Some(&body[offset + 1..])))
        .unwrap_or((body, None))
}

fn is_single_nested_expansion(body: &str) -> bool {
    body.starts_with("#{") && format_skip(body.as_bytes(), b"}") == Some(body.len() - 1)
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

#[cfg(test)]
#[path = "formats/job_tests.rs"]
mod job_tests;
