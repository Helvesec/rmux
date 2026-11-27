//! Variable resolution for format expansion: build capabilities first, then
//! the implementor's own lookup.

use crate::terminal_passthrough::SIXEL_SUPPORT;

use super::{
    format_variable_descriptor, FormatVariableKey, FormatVariableProvider, FormatVariables,
};

pub(super) fn resolve_variable<V>(name: &str, variables: &V) -> String
where
    V: FormatVariables + ?Sized,
{
    if let Some(value) = build_capability_value(name) {
        return value.to_owned();
    }
    variables.format_value_by_name(name).unwrap_or_default()
}

/// Resolves a variable for `#{b:…}` and `#{d:…}`, keeping absence.
///
/// The same priority as [`resolve_variable`] - the build capabilities first,
/// then the implementor's own lookup - but the lookup that reports what it
/// really found, so a value holding nothing is a value and a name nobody
/// answered is absent. Only the plain lookup branch with one of those two
/// modifiers reads this; the conditions, the short aliases, the nested bodies
/// and every other modifier keep reading [`resolve_variable`].
pub(super) fn resolve_path_variable<V>(name: &str, variables: &V) -> Option<String>
where
    V: FormatVariables + ?Sized,
{
    if let Some(value) = build_capability_value(name) {
        return Some(value.to_owned());
    }
    variables.format_path_value_by_name(name)
}

/// Resolves the format variables that describe the running binary rather than
/// any session, window, pane, or client.
///
/// tmux serves these from the global `format_table[]`, which `format_find`
/// consults before the per-format tree and before the environment, so they stay
/// defined with no client attached and with no target resolved. Measured on the
/// pinned tmux 3.7b oracle, `#{sixel_support}` answers the same single digit for
/// `display-message`, every `list-* -F` route, `#{S:}`/`#{W:}`/`#{P:}` loops,
/// `#{E:}` re-expansion and `automatic-rename-format`, attached or detached, and
/// does not move with `-T sixel`, `terminal-features` or `terminal-overrides`.
/// Resolving here rather than in one `FormatVariables` implementor keeps every
/// entry path that shares this expander in agreement, including the bare
/// [`FormatContext`](super::FormatContext) used by `list-commands -F`.
fn build_capability_value(name: &str) -> Option<&'static str> {
    let descriptor = format_variable_descriptor(name)?;
    if descriptor.provider() != FormatVariableProvider::BuildCapability {
        return None;
    }

    match descriptor.key() {
        FormatVariableKey::SixelSupport => Some(sixel_support_value(SIXEL_SUPPORT)),
        _ => None,
    }
}

/// Renders a build-time SIXEL capability the way tmux renders `ENABLE_SIXEL`.
///
/// The domain is exactly `1` or `0`; an unsupported build answers `0` and never
/// the empty string, so `#{==:#{sixel_support},0}` and `#{p5:sixel_support}`
/// stay meaningful.
pub(super) const fn sixel_support_value(supported: bool) -> &'static str {
    if supported {
        "1"
    } else {
        "0"
    }
}
