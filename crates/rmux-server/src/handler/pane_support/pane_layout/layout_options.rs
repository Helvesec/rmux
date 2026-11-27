//! Window options a named layout reads: main-pane sizes and the tiled column bound.

use rmux_proto::{OptionName, WindowTarget};

use crate::pane_terminals::HandlerState;

/// The option values a named layout selection reads, resolved for the window
/// the command addresses at the moment the command runs.
#[derive(Clone, Copy, Default)]
pub(super) struct LayoutOptionValues {
    pub(super) main_width: Option<u16>,
    pub(super) main_height: Option<u16>,
    pub(super) tiled_max_columns: Option<u16>,
}

pub(super) fn layout_option_values_for_select_target(
    state: &HandlerState,
    target: &rmux_proto::SelectLayoutTarget,
) -> LayoutOptionValues {
    match target {
        rmux_proto::SelectLayoutTarget::Session(session_name) => state
            .sessions
            .session(session_name)
            .map_or_else(LayoutOptionValues::default, |session| {
                layout_option_values_for_window(state, session_name, session.active_window_index())
            }),
        rmux_proto::SelectLayoutTarget::Window(target) => {
            layout_option_values_for_window_target(state, target)
        }
    }
}

pub(super) fn layout_option_values_for_window_target(
    state: &HandlerState,
    target: &WindowTarget,
) -> LayoutOptionValues {
    layout_option_values_for_window(state, target.session_name(), target.window_index())
}

/// `resolve_for_window` is the existing window-option owner lookup: an explicit
/// window value wins, otherwise the global window value, otherwise the default.
/// No option propagation is added here.
///
/// A percentage main-pane size needs the axis it is a percentage of. That axis
/// is the canonical window's own size, read through the same link resolution
/// the mutation itself uses, so a linked or inactive window and a selection
/// queued after a resize all measure the window this command addresses rather
/// than a target tile or a client viewport.
fn layout_option_values_for_window(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) -> LayoutOptionValues {
    let window_size = state
        .sessions
        .window(&WindowTarget::with_window(
            session_name.clone(),
            window_index,
        ))
        .ok()
        .map(rmux_core::Window::size);
    LayoutOptionValues {
        main_width: main_dimension_with_other(
            option_main_dimension(
                state.options.resolve_for_window(
                    session_name,
                    window_index,
                    OptionName::MainPaneWidth,
                ),
                window_size.map(|size| size.cols),
            ),
            registry_default_main(OptionName::MainPaneWidth),
            state.options.resolve_for_window(
                session_name,
                window_index,
                OptionName::OtherPaneWidth,
            ),
            window_size.map(|size| size.cols),
        ),
        main_height: main_dimension_with_other(
            option_main_dimension(
                state.options.resolve_for_window(
                    session_name,
                    window_index,
                    OptionName::MainPaneHeight,
                ),
                window_size.map(|size| size.rows),
            ),
            registry_default_main(OptionName::MainPaneHeight),
            state.options.resolve_for_window(
                session_name,
                window_index,
                OptionName::OtherPaneHeight,
            ),
            window_size.map(|size| size.rows),
        ),
        tiled_max_columns: option_tiled_max_columns(state.options.resolve_for_window(
            session_name,
            window_index,
            OptionName::TiledLayoutMaxColumns,
        )),
    }
}

/// Reads `main-pane-width` or `main-pane-height`, which the registry stores as
/// a string and which tmux also accepts as a percentage.
///
/// The absolute spelling is unchanged. A percentage follows
/// `args_string_percentage` as the pinned source calls it from
/// `layout-set.c:233/335/437/539`, with `minval = 0` and both `maxval` and
/// `curval` equal to the axis minus the one cell taken off for the separating
/// border: the last character must be `%`, the rest must read as a number in
/// `0..=100`, and the result is `floor(available * percent / 100)`.
///
/// Anything the source would reject resolves to `None`, which keeps the
/// existing fallback to the layout's own default. Nothing is rejected at
/// `set-option`, and the stored string is untouched.
fn option_main_dimension(value: Option<&str>, axis: Option<u16>) -> Option<u16> {
    let value = value?;
    // tmux tests the raw last character, so `50% ` is not a percentage at all
    // and falls through to the absolute spelling.
    let Some(percent) = value.strip_suffix('%') else {
        return value.parse::<u16>().ok();
    };
    let percent = parse_percentage_part(percent)?;
    let available = u32::from(axis?).saturating_sub(1);
    u16::try_from(available * u32::from(percent) / 100).ok()
}

/// Mirrors `strtonum(copy, 0, 100, &errstr)` on the part before the `%`.
///
/// `strtonum` defers to `strtoll`, so leading blanks and one optional sign are
/// accepted while trailing characters are not, and the value has to land in
/// `0..=100`. Measured on the pinned tmux 3.7b at 121x41 for `main-pane-width`:
/// `50%`, `" 50%"`, `+50%` and `050%` all resolve to 60, `-0%` and `0%` to 0,
/// `100%` to the whole axis, and `-5%`, `101%`, `50%%`, `5%0` and `%` all fall
/// back to 80.
fn parse_percentage_part(value: &str) -> Option<u16> {
    parse_bounded_number(value, 100)
}

/// Mirrors `strtonum(value, 0, maximum, &errstr)`.
///
/// `strtonum` defers to `strtoll`, so leading blanks and one optional sign are
/// accepted while a trailing character is not, and the value has to land in
/// `0..=maximum`.
fn parse_bounded_number(value: &str, maximum: u16) -> Option<u16> {
    let digits = value.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let parsed = digits.parse::<i64>().ok()?;
    (0..=i64::from(maximum))
        .contains(&parsed)
        .then(|| u16::try_from(parsed).ok())?
}

/// The one cell tmux takes off an axis for the separating border, and the
/// smallest extent it keeps for a pane (`PANE_MINIMUM`).
const LAYOUT_BORDER_CELLS: u16 = 1;
const LAYOUT_PANE_MINIMUM: u16 = 1;

/// The registry's own default for a main-pane option, with no override in play.
///
/// An empty store resolves to exactly the declared default, which is what the
/// layout falls back to when the configured value does not resolve. Reading it
/// here avoids a second table of constants, and it deliberately ignores any
/// overridden value: a global main must not stand in for the default of a local
/// main that did not resolve.
fn registry_default_main(option: OptionName) -> u16 {
    let defaults = rmux_core::OptionStore::new();
    let session = rmux_proto::SessionName::new("rmux").expect("valid session name");
    option_main_dimension(defaults.resolve_for_window(&session, 0, option), None).unwrap_or(0)
}

/// Folds `other-pane-width` or `other-pane-height` into the main dimension the
/// named layout receives.
///
/// tmux does not carry the secondary band as its own size: it turns a valid
/// other into a main (`layout-set.c:439-455`). With `A` the axis minus its
/// border and `M` the resolved main, a main that already leaves no room keeps
/// the existing clamp untouched; otherwise a valid `O` with `A - O >= M` makes
/// the main `A - O`, so the core's own `D - main - 1` band becomes exactly `O`.
/// Zero, an unreadable value and a conflict all leave the main alone, which is
/// how the source restores `other = A - M`.
fn main_dimension_with_other(
    main: Option<u16>,
    main_fallback: u16,
    other: Option<&str>,
    axis: Option<u16>,
) -> Option<u16> {
    let Some(axis) = axis else {
        return main;
    };
    let available = axis.saturating_sub(LAYOUT_BORDER_CELLS);
    let resolved_main = main.unwrap_or(main_fallback);
    if resolved_main.saturating_add(LAYOUT_PANE_MINIMUM) >= available {
        // The small-axis branch of the source, which the core clamp already owns.
        return main;
    }
    let Some(other) = option_other_dimension(other, available) else {
        return main;
    };
    if other == 0 || other > available || available.saturating_sub(other) < resolved_main {
        return main;
    }
    Some(available - other)
}

/// Reads `other-pane-width` or `other-pane-height` against the available axis.
///
/// The source calls `args_string_percentage(s, 0, A, A)` for this option, so
/// both spellings go through the same `strtonum` lexis: a trailing `%` over a
/// part in `0..=100` gives `floor(A * percent / 100)`, and an absolute has to
/// land in `0..=A`. A doubled sign, a trailing character, an overflow or a
/// value past the axis is unreadable and leaves the main alone. Nothing is
/// rejected at `set-option` and the stored string is untouched.
fn option_other_dimension(value: Option<&str>, available: u16) -> Option<u16> {
    let value = value?;
    let Some(percent) = value.strip_suffix('%') else {
        return parse_bounded_number(value, available);
    };
    let percent = parse_percentage_part(percent)?;
    u16::try_from(u32::from(available) * u32::from(percent) / 100).ok()
}

/// Reads `tiled-layout-max-columns`, where `0` and an absent value both mean
/// no bound.
///
/// tmux stores this option up to `USHRT_MAX`, while the RMUX registry declares
/// it only as a non-negative number and so already accepts larger values. A
/// larger accepted value saturates at `u16::MAX` on the way to the layout
/// parameter: wrapping would invent a small bound and falling back to `0` would
/// silently mean unbounded. Tightening the registry itself is out of scope here.
fn option_tiled_max_columns(value: Option<&str>) -> Option<u16> {
    let value = value?;
    let columns = match value.parse::<u16>() {
        Ok(columns) => columns,
        Err(_) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => u16::MAX,
        Err(_) => return None,
    };
    (columns != 0).then_some(columns)
}
