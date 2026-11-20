//! The background behind everything: `fill-character`.
//!
//! Every cell of the client terminal that no pane draws on carries a glyph —
//! the margins left by a window smaller than its terminal, and whatever a
//! float has vacated. That is the only thing this layer paints. It runs before
//! the frames and the status line and after a zoom clear, so a pane body, a
//! border, a pane-border-status row, a scrollbar and an overlay all keep their
//! own rendering on top of it.
//!
//! The bytes go straight to the client. Nothing here reaches a `Screen`, so no
//! transcript and no `capture-pane` ever sees this background — which is also
//! why it has to be read off a real client terminal to be believed.

use rmux_core::formats::FormatContext;
use rmux_core::style::Style;
use rmux_core::{text_width, OptionStore, Utf8Config};
use rmux_proto::OptionName;

use crate::format_runtime::RuntimeFormatContext;

use super::borders::PaneBorderLineStyle;
use super::projection::{OutsideRun, TerminalSceneProjection};
use super::{apply_runtime_style_overlay, cursor_position_bytes, style_sgr_bytes};

/// Paints the cells outside every visible pane, and nothing else.
pub(super) fn render_outside_fill(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
) -> Vec<u8> {
    let runs = TerminalSceneProjection::resolve(session, options).outside_runs();
    if runs.is_empty() {
        return Vec::new();
    }
    let window_index = session.active_window_index();
    render_runs(
        &runs,
        outside_glyph(session, options, window_index),
        &outside_style(session, options, window_index),
    )
}

/// Addresses each run once and then repeats its glyph.
///
/// The whole layer shares one style, so the escape that selects it is written
/// once; the cursor is saved and restored around it because the pane bodies
/// that follow expect to find it where they left it.
fn render_runs(runs: &[OutsideRun], glyph: char, style: &Style) -> Vec<u8> {
    let mut encoded = [0_u8; 4];
    let encoded = glyph.encode_utf8(&mut encoded).as_bytes();
    let mut frame = Vec::new();
    frame.extend_from_slice(b"\x1b[s");
    frame.extend_from_slice(b"\x1b[0m");
    frame.extend_from_slice(style_sgr_bytes(style, false).as_slice());
    for run in runs {
        frame.extend_from_slice(cursor_position_bytes(run.y, run.x).as_slice());
        for _ in 0..run.cols {
            frame.extend_from_slice(encoded);
        }
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    frame
}

/// The glyph every outside cell carries.
///
/// `fill-character` names it when the option is admissible; otherwise the cell
/// falls back to the outside glyph of `pane-border-lines`, which is what tmux
/// draws when no fill character is configured. An empty option is the default,
/// so the fallback is the ordinary case rather than an error path.
fn outside_glyph(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    window_index: u32,
) -> char {
    let fallback = PaneBorderLineStyle::from_option(options.resolve_for_window(
        session.name(),
        window_index,
        OptionName::PaneBorderLines,
    ))
    .outside_glyph();
    options
        .resolve_for_window(session.name(), window_index, OptionName::FillCharacter)
        .and_then(|value| fill_character(value, &Utf8Config::from_options(options)))
        .unwrap_or(fallback)
}

/// tmux's `window_set_fill_character`, with its C string semantics spelled out.
///
/// The option contributes its *first* codepoint and only that one: the rest of
/// the string is never drawn, the string is never repeated across cells, and
/// nothing is recomposed — `e` followed by a combining acute draws `e`. The
/// whole string still has to be admissible, so `X` followed by a control byte
/// yields no fill character at all, and a first codepoint that is not exactly
/// one cell wide is refused.
fn fill_character(value: &str, utf8: &Utf8Config) -> Option<char> {
    // The C source reads a NUL-terminated string, so nothing from the first NUL
    // onwards is part of the value. A Rust `String` can carry one, so bound here
    // rather than assume the byte cannot arrive.
    let value = value.split('\0').next().unwrap_or_default();
    let first = value.chars().next()?;
    if !value.chars().all(is_admissible) {
        return None;
    }
    // Admissibility has already bounded a single-byte head to printable ASCII,
    // and tmux gives that head a width of exactly one without ever asking the
    // width cache: `utf8_fromcstr` hands a lone byte to `utf8_set`, whose
    // constant record fixes size, have and width to 1. Only a multibyte head
    // reaches `utf8_width`, so only a multibyte head is governed by
    // `codepoint-widths`.
    if first.len_utf8() == 1 {
        return Some(first);
    }
    let mut encoded = [0_u8; 4];
    (text_width(first.encode_utf8(&mut encoded), utf8) == 1).then_some(first)
}

/// Whether a codepoint keeps `utf8_isvalid` happy.
///
/// That function walks bytes: a well-formed multibyte sequence always passes,
/// and a single byte has to be printable ASCII. A Rust `&str` cannot hold a
/// malformed sequence, so only the ASCII test is left — and it is a test on
/// bytes, not on the Unicode notion of a control character, so a multibyte
/// codepoint is judged by its width alone.
const fn is_admissible(ch: char) -> bool {
    ch.len_utf8() > 1 || matches!(ch, ' '..='~')
}

/// The style of an outside cell: the window's `pane-border-style` over the
/// default cell, resolved with a window format context and no pane.
///
/// No pane owns these cells, so `pane-active-border-style` never applies to
/// them and no pane reaches the format context.
fn outside_style(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    window_index: u32,
) -> Style {
    let context = FormatContext::new()
        .with_managed_session(session)
        .with_window(window_index, session.window(), true, false);
    let runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(window_index, session.window());
    apply_runtime_style_overlay(
        &Style::default(),
        options.resolve_for_window(session.name(), window_index, OptionName::PaneBorderStyle),
        &runtime,
    )
}
