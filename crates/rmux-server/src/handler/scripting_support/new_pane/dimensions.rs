//! The two spellings of an explicit `new-pane` dimension.
//!
//! `-x` and `-y` accept a cell count or a percentage of the window that owns
//! the target. A percentage cannot be turned into cells while the arguments are
//! being read, because the window is only known once `-t` has been resolved, so
//! the value keeps this shape until then. `-X` and `-Y` have the same two
//! spellings and live in `positions.rs`, where a coordinate's own bounds
//! differ from a dimension's.

use rmux_proto::RmuxError;

use super::super::values::parse_percentage;
use super::invalid;

/// A `-x` or `-y` value, before the target's window is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dimension {
    Cells(i32),
    Percent(u8),
}

impl Dimension {
    /// Reads one `-x`/`-y` value.
    ///
    /// A trailing `%` marks a percentage, which tmux bounds to 0..=100 before
    /// it computes anything, so `101%` is refused here rather than by the size
    /// it would have produced.
    pub(super) fn parse(flag: &str, value: &str) -> Result<Self, RmuxError> {
        if let Some(percentage) = value.strip_suffix('%') {
            return parse_percentage("new-pane", flag, percentage).map(Self::Percent);
        }
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("geometry requires decimal integers"));
        }
        value
            .parse::<i32>()
            .map(Self::Cells)
            .map_err(|_| invalid("geometry overflow"))
    }

    /// Resolves against one canonical dimension of the target's window.
    ///
    /// tmux truncates — `(D * p) / 100` on integers, with no rounding and no
    /// clamp — so 1% of a 79-column window is 0 columns and 50% of it is 39.
    /// Nothing here rescues a zero: the rectangle that follows requires a
    /// positive size and refuses it before anything is created, which is the
    /// divergence FNP1 chose and this lot keeps.
    ///
    /// The product cannot overflow: `window` is a `u16` and the percentage is
    /// bounded to 100, so it is at most 6_553_500.
    pub(super) fn resolve(self, window: u16) -> i32 {
        match self {
            Self::Cells(cells) => cells,
            Self::Percent(percentage) => i32::from(window) * i32::from(percentage) / 100,
        }
    }
}
