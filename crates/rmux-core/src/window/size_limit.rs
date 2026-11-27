//! tmux 3.7b window size bounds (`WINDOW_MINIMUM` and `WINDOW_MAXIMUM`).

use rmux_proto::{RmuxError, TerminalSize};

/// Smallest explicit window width or height, tmux 3.7b `WINDOW_MINIMUM`.
pub const WINDOW_MINIMUM: u16 = 1;

/// Largest window width or height, tmux 3.7b `WINDOW_MAXIMUM`.
pub const WINDOW_MAXIMUM: u16 = 10_000;

/// Parses a window dimension before command-specific geometry validation.
/// Preserve tmux's range diagnostic even when the text overflows `i64`.
/// Zero and values above `WINDOW_MAXIMUM` remain available to the caller:
/// new-session caps its geometry, whereas resize-window rejects those values.
pub fn parse_window_dimension(value: &str, label: &str) -> Result<u16, RmuxError> {
    let error = |reason| RmuxError::Server(format!("{label} {reason}"));
    let value = value.trim_start_matches([' ', '\t', '\n', '\r', '\x0b', '\x0c']);
    // strtonum rejects trailing non-digits even when the digit prefix overflows.
    if !value
        .trim_start_matches(['+', '-'])
        .bytes()
        .all(|byte| byte.is_ascii_digit())
    {
        return Err(error("invalid"));
    }
    let parsed = value.parse::<i64>().map_err(|source| {
        error(match source.kind() {
            std::num::IntErrorKind::PosOverflow => "too large",
            std::num::IntErrorKind::NegOverflow => "too small",
            _ => "invalid",
        })
    })?;
    if parsed < 0 {
        return Err(error("too small"));
    }
    u16::try_from(parsed).map_err(|_| error("too large"))
}

/// Caps both axes at [`WINDOW_MAXIMUM`].
///
/// tmux 3.7b applies this cap in `resize_window()` and at the end of
/// `default_window_size()`, so every window geometry source (`new-session
/// -x/-y`, `default-size`, client sizes, relative `resize-window`
/// adjustments) lands inside the bound instead of each parser checking it.
#[must_use]
pub const fn cap_window_size(size: TerminalSize) -> TerminalSize {
    TerminalSize {
        cols: cap_axis(size.cols),
        rows: cap_axis(size.rows),
    }
}

const fn cap_axis(value: u16) -> u16 {
    if value > WINDOW_MAXIMUM {
        WINDOW_MAXIMUM
    } else {
        value
    }
}

/// Rejects an explicit `resize-window -x/-y` outside the window bounds.
///
/// tmux 3.7b parses both flags with `args_strtonum(.., WINDOW_MINIMUM,
/// WINDOW_MAXIMUM, ..)` and reports `width too large`, `height too small`,
/// and so on, checking the width first.
pub fn validate_explicit_window_size(
    width: Option<u16>,
    height: Option<u16>,
) -> Result<(), RmuxError> {
    validate_axis("width", width)?;
    validate_axis("height", height)
}

fn validate_axis(label: &str, value: Option<u16>) -> Result<(), RmuxError> {
    match value {
        Some(value) if value < WINDOW_MINIMUM => {
            Err(RmuxError::Server(format!("{label} too small")))
        }
        Some(value) if value > WINDOW_MAXIMUM => {
            Err(RmuxError::Server(format!("{label} too large")))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Window;

    const OVERSIZED: TerminalSize = TerminalSize {
        cols: u16::MAX,
        rows: u16::MAX,
    };
    const MAXIMUM: TerminalSize = TerminalSize {
        cols: WINDOW_MAXIMUM,
        rows: WINDOW_MAXIMUM,
    };

    #[test]
    fn r3634_window_creation_caps_each_axis_at_window_maximum() {
        let window = Window::new(OVERSIZED);
        assert_eq!(window.size(), MAXIMUM);
        let pane = window.pane(0).expect("initial pane");
        assert_eq!(
            (pane.geometry().cols(), pane.geometry().rows()),
            (WINDOW_MAXIMUM, WINDOW_MAXIMUM)
        );

        let wide = Window::new(TerminalSize {
            cols: WINDOW_MAXIMUM + 1,
            rows: 24,
        });
        assert_eq!(
            wide.size(),
            TerminalSize {
                cols: WINDOW_MAXIMUM,
                rows: 24
            }
        );
    }

    #[test]
    fn r3634_window_resize_caps_explicit_and_constrained_sizes() {
        let mut window = Window::new(TerminalSize { cols: 80, rows: 24 });
        window.set_explicit_size(OVERSIZED);
        assert_eq!(window.size(), MAXIMUM);
        assert_eq!(window.manual_size().size(), MAXIMUM);

        window.set_size(TerminalSize {
            cols: 120,
            rows: WINDOW_MAXIMUM + 1,
        });
        assert_eq!(
            window.size(),
            TerminalSize {
                cols: 120,
                rows: WINDOW_MAXIMUM
            }
        );
    }

    #[test]
    fn r3634_sizes_inside_the_bounds_are_unchanged() {
        for size in [
            TerminalSize { cols: 1, rows: 1 },
            TerminalSize { cols: 80, rows: 24 },
            MAXIMUM,
        ] {
            assert_eq!(cap_window_size(size), size);
            assert_eq!(Window::new(size).size(), size);
        }
    }

    #[test]
    fn r3634_explicit_resize_rejects_out_of_range_axes_width_first() {
        let message = |width, height| match validate_explicit_window_size(width, height) {
            Ok(()) => None,
            Err(RmuxError::Server(message)) => Some(message),
            Err(other) => panic!("unexpected error {other:?}"),
        };
        assert_eq!(message(Some(WINDOW_MAXIMUM), Some(WINDOW_MAXIMUM)), None);
        assert_eq!(message(None, None), None);
        assert_eq!(message(Some(1), Some(1)), None);
        assert_eq!(
            message(Some(WINDOW_MAXIMUM + 1), Some(24)).as_deref(),
            Some("width too large")
        );
        assert_eq!(
            message(Some(80), Some(WINDOW_MAXIMUM + 1)).as_deref(),
            Some("height too large")
        );
        assert_eq!(
            message(Some(20_000), Some(20_000)).as_deref(),
            Some("width too large")
        );
        assert_eq!(
            message(Some(0), Some(24)).as_deref(),
            Some("width too small")
        );
        assert_eq!(message(None, Some(0)).as_deref(), Some("height too small"));
    }
}
