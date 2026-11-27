//! Cursor-style validation and effective-state resolution.

use crate::input::mode;

/// Returns whether `style` is a valid DECSCUSR parameter.
#[must_use]
pub const fn cursor_style_is_valid(style: u32) -> bool {
    style <= 6
}

/// Maps the tmux `cursor-style` option to its DECSCUSR parameter.
#[must_use]
pub fn cursor_style_from_option(value: Option<&str>) -> u32 {
    match value.unwrap_or("default") {
        "blinking-block" => 1,
        "block" => 2,
        "blinking-underline" => 3,
        "underline" => 4,
        "blinking-bar" => 5,
        "bar" => 6,
        _ => 0,
    }
}

/// Effective pane cursor presentation without changing the raw wire style.
///
/// A nonzero style carries its blink state in DECSCUSR parity. When both the
/// application style and configured default are zero, the terminal owns the
/// shape, so an explicit DEC mode 12 request is retained separately instead
/// of guessing a block cursor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResolvedCursorStyle {
    style: u32,
    default_blink: Option<bool>,
}

impl ResolvedCursorStyle {
    /// Builds a presentation whose blink is encoded by a DECSCUSR style.
    #[must_use]
    pub const fn from_style(style: u32) -> Self {
        Self {
            style: if cursor_style_is_valid(style) {
                style
            } else {
                0
            },
            default_blink: None,
        }
    }

    /// Returns the effective DECSCUSR parameter, or zero for terminal default.
    #[must_use]
    pub const fn style(self) -> u32 {
        self.style
    }

    /// Returns explicit DEC mode 12 state when the terminal owns the shape.
    #[must_use]
    pub const fn default_blink(self) -> Option<bool> {
        self.default_blink
    }
}

impl From<u32> for ResolvedCursorStyle {
    fn from(style: u32) -> Self {
        Self::from_style(style)
    }
}

/// Resolves the full cursor presentation used by renderers and snapshots.
#[must_use]
pub const fn resolve_cursor_style_state(
    style: u32,
    mode_bits: u32,
    default_style: u32,
) -> ResolvedCursorStyle {
    let style = if cursor_style_is_valid(style) {
        style
    } else {
        0
    };
    let default_style = if cursor_style_is_valid(default_style) {
        default_style
    } else {
        0
    };
    let inherited = if style == 0 { default_style } else { style };
    if inherited == 0 {
        return ResolvedCursorStyle {
            style: 0,
            default_blink: if mode_bits & mode::MODE_CURSOR_BLINKING_SET != 0 {
                Some(mode_bits & mode::MODE_CURSOR_BLINKING != 0)
            } else {
                None
            },
        };
    }
    if mode_bits & mode::MODE_CURSOR_BLINKING_SET == 0 {
        return ResolvedCursorStyle::from_style(inherited);
    }

    let steady = if inherited % 2 == 0 {
        inherited
    } else {
        inherited.saturating_add(1)
    };
    ResolvedCursorStyle::from_style(if mode_bits & mode::MODE_CURSOR_BLINKING != 0 {
        steady.saturating_sub(1)
    } else {
        steady
    })
}

/// Resolves the cursor style visible to a renderer or state query.
///
/// A zero application style inherits `default_style`. An explicit DECSET or
/// DECRST 12 changes only the blink parity while preserving the selected
/// block, underline, or bar shape. Invalid values defensively collapse to the
/// terminal default; normal parser mutation rejects them before storage.
#[must_use]
pub const fn resolve_cursor_style(style: u32, mode_bits: u32, default_style: u32) -> u32 {
    resolve_cursor_style_state(style, mode_bits, default_style).style()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_preserves_shape_while_blink_changes() {
        let blinking = mode::MODE_CURSOR_BLINKING | mode::MODE_CURSOR_BLINKING_SET;
        let steady = mode::MODE_CURSOR_BLINKING_SET;

        for (blinking_style, steady_style) in [(1, 2), (3, 4), (5, 6)] {
            assert_eq!(
                resolve_cursor_style(steady_style, blinking, 0),
                blinking_style
            );
            assert_eq!(
                resolve_cursor_style(blinking_style, steady, 0),
                steady_style
            );
        }
    }

    #[test]
    fn zero_style_inherits_configured_default_before_blink_resolution() {
        let blinking = mode::MODE_CURSOR_BLINKING | mode::MODE_CURSOR_BLINKING_SET;
        assert_eq!(resolve_cursor_style(0, blinking, 6), 5);
        assert_eq!(resolve_cursor_style(0, blinking, 0), 0);
        assert_eq!(
            resolve_cursor_style_state(0, blinking, 0).default_blink(),
            Some(true)
        );
    }
}
