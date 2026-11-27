//! The two spellings of an explicit `new-pane` position.
//!
//! `-X` and `-Y` place the float in the content frame of the window that owns
//! the target, either as a signed cell coordinate or as a percentage of that
//! frame. Like a dimension, a percentage cannot become a coordinate while the
//! arguments are being read, so it keeps this shape until the window is known.
//!
//! A position is not a dimension, and the difference matters at both ends: 0 is
//! a legal coordinate rather than an empty rectangle, and the whole dimension
//! is a legal coordinate too, so the `D - 1` edge that bounds a size must not
//! be applied here. The float's own size is never subtracted from the frame
//! either; the signed lower bound is checked later, against the resolved
//! dimension.

use rmux_proto::RmuxError;

use super::super::values::parse_percentage;
use super::invalid;

/// A `-X` or `-Y` value, before the target's window is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Position {
    Cells(i32),
    Percent(u8),
}

impl Position {
    /// Reads one `-X`/`-Y` value.
    ///
    /// The absolute spelling is unchanged: a sign, decimal digits, and an `i32`
    /// that must not overflow. For a percentage the sign is lexical rather than
    /// a direction — `-0%` is the origin, every other negative percentage is
    /// outside the domain — and the domain itself is still the shared helper's,
    /// so `101%` is refused exactly where a dimension's would be.
    ///
    /// There is at most ONE sign. Whatever follows it has to be decimal digits
    /// here, before the shared helper sees it: that helper parses a `u8` and
    /// would accept a leading `+` of its own, so a value that has already given
    /// up a sign could otherwise present a second one and `++50%` would read as
    /// fifty per cent.
    pub(super) fn parse(flag: &str, value: &str) -> Result<Self, RmuxError> {
        if let Some(percentage) = value.strip_suffix('%') {
            let (negative, digits) = match percentage.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, percentage.strip_prefix('+').unwrap_or(percentage)),
            };
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid("geometry requires decimal integers"));
            }
            let percentage = parse_percentage("new-pane", flag, digits)?;
            if negative && percentage != 0 {
                return Err(invalid("geometry percentage must not be negative"));
            }
            return Ok(Self::Percent(percentage));
        }
        let digits = value.strip_prefix(['-', '+']).unwrap_or(value);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid("geometry requires decimal integers"));
        }
        value
            .parse::<i32>()
            .map(Self::Cells)
            .map_err(|_| invalid("geometry overflow"))
    }

    /// Resolves against one canonical dimension of the target's window.
    ///
    /// Truncating integer arithmetic, the same the dimensions use: 50% of a
    /// 79-column frame is column 39, and 33% of a 25-row frame is row 8. The
    /// product cannot overflow — `window` is a `u16` and the percentage is
    /// bounded to 100, so it is at most 6_553_500.
    pub(super) fn resolve(self, window: u16) -> i32 {
        match self {
            Self::Cells(cells) => cells,
            Self::Percent(percentage) => i32::from(window) * i32::from(percentage) / 100,
        }
    }
}
