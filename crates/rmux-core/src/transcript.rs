//! Bounded pane transcript history and shared capture-range helpers.

use std::collections::VecDeque;
use std::ops::RangeInclusive;

/// tmux-compatible capture bounds over history plus visible rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScreenCaptureRange {
    /// Optional start value relative to the history size.
    pub start: Option<i64>,
    /// Optional end value relative to the history size.
    pub end: Option<i64>,
    /// Whether `start` used the `-` sentinel for absolute history start.
    pub start_is_absolute: bool,
    /// Whether `end` used the `-` sentinel for absolute capture end.
    pub end_is_absolute: bool,
}

impl ScreenCaptureRange {
    /// Creates a range with tmux-style relative defaults.
    #[must_use]
    pub const fn new(start: Option<i64>, end: Option<i64>) -> Self {
        Self {
            start,
            end,
            start_is_absolute: false,
            end_is_absolute: false,
        }
    }
}

/// Per-pane line transcript bounded by the effective `history-limit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    lines: VecDeque<String>,
    limit: usize,
}

impl Transcript {
    /// Creates an empty transcript bounded to `limit` retained lines.
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            limit,
        }
    }

    /// Returns the current retained line limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// Returns the number of retained history lines.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Updates the retained line limit and evicts older lines if needed.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        self.enforce_limit();
    }

    /// Appends one complete logical line to the transcript.
    pub fn append_line(&mut self, line: impl Into<String>) {
        if self.limit == 0 {
            return;
        }

        self.lines.push_back(line.into());
        self.enforce_limit();
    }

    /// Returns the retained lines ordered from oldest to newest.
    #[must_use]
    pub const fn lines(&self) -> &VecDeque<String> {
        &self.lines
    }

    /// Captures an inclusive range of retained lines as newline-delimited bytes.
    ///
    /// Non-negative bounds are zero-based indices into retained history.
    /// Negative bounds count backward from the newest line, where `-1` is the
    /// newest retained line. Missing bounds mean the first or last retained line.
    /// Out-of-range values clamp to the retained transcript. Reversed ranges are
    /// swapped to match tmux's buffer capture behavior.
    #[must_use]
    pub fn capture(&self, start: Option<i64>, end: Option<i64>) -> Vec<u8> {
        let Some(range) = resolve_relative_capture_range(start, end, self.lines.len()) else {
            return Vec::new();
        };

        let mut output = Vec::new();
        for (index, line) in self.lines.iter().enumerate() {
            if index < *range.start() {
                continue;
            }
            if index > *range.end() {
                break;
            }
            output.extend_from_slice(line.as_bytes());
            output.push(b'\n');
        }
        output
    }

    /// Returns the retained history size in bytes including trailing newlines.
    #[must_use]
    pub fn byte_size(&self) -> usize {
        self.lines.iter().map(|line| line.len() + 1).sum()
    }

    fn enforce_limit(&mut self) {
        while self.lines.len() > self.limit {
            self.lines.pop_front();
        }
    }
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new(2000)
    }
}

/// Captures tmux-style screen lines as newline-delimited bytes.
#[cfg_attr(not(test), allow(dead_code))]
#[must_use]
pub fn capture_screen_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    line_count: usize,
    history_size: usize,
    range: ScreenCaptureRange,
) -> Vec<u8> {
    let Some(range) = resolve_screen_capture_range(range, history_size, line_count) else {
        return Vec::new();
    };

    let mut output = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        if index < *range.start() {
            continue;
        }
        if index > *range.end() {
            break;
        }
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
    output
}

/// Reads one `capture-pane -S` or `-E` value after format expansion.
///
/// tmux 3.7b reads the value with `strtonum(value, INT_MIN, SHRT_MAX)`, which
/// defers to `strtoll`: leading C blanks and one sign are accepted, a trailing
/// byte is not. Any failure, including an out-of-range value, falls back to the
/// default bound instead of failing the command (`cmd-capture-pane.c`), which
/// `None` already means here. The `-` sentinel is the caller's to recognise.
#[must_use]
pub fn parse_capture_bound(value: &str) -> Option<i64> {
    let digits = value.trim_start_matches([' ', '\t', '\n', '\u{b}', '\u{c}', '\r']);
    let bound = digits.parse::<i64>().ok()?;
    (i64::from(i32::MIN)..=i64::from(i16::MAX))
        .contains(&bound)
        .then_some(bound)
}

/// Resolves a tmux-compatible screen capture range.
#[must_use]
pub fn resolve_screen_capture_range(
    range: ScreenCaptureRange,
    history_size: usize,
    total_lines: usize,
) -> Option<RangeInclusive<usize>> {
    if total_lines == 0 {
        return None;
    }

    let last_line = total_lines - 1;
    let default_top = history_size.min(last_line);
    let mut top = if range.start_is_absolute {
        0
    } else {
        resolve_screen_bound(range.start, default_top, history_size, last_line)
    };
    let mut bottom = if range.end_is_absolute {
        last_line
    } else {
        resolve_screen_bound(range.end, last_line, history_size, last_line)
    };
    if bottom < top {
        std::mem::swap(&mut top, &mut bottom);
    }
    Some(top..=bottom)
}

fn resolve_screen_bound(
    bound: Option<i64>,
    default: usize,
    history_size: usize,
    last_line: usize,
) -> usize {
    let Some(bound) = bound else {
        return default;
    };
    if bound >= 0 {
        return history_size
            .saturating_add(usize::try_from(bound).unwrap_or(usize::MAX))
            .min(last_line);
    }

    let magnitude = usize::try_from(bound.unsigned_abs()).unwrap_or(usize::MAX);
    if magnitude > history_size {
        0
    } else {
        history_size.saturating_sub(magnitude).min(last_line)
    }
}

fn resolve_relative_capture_range(
    start: Option<i64>,
    end: Option<i64>,
    len: usize,
) -> Option<RangeInclusive<usize>> {
    if len == 0 {
        return None;
    }

    let mut start = resolve_relative_bound(start, len, 0)?;
    let mut end = resolve_relative_bound(end, len, len.saturating_sub(1))?;
    if start > end {
        std::mem::swap(&mut start, &mut end);
    }
    Some(start..=end)
}

fn resolve_relative_bound(bound: Option<i64>, len: usize, default: usize) -> Option<usize> {
    let bound = match bound {
        Some(bound) => bound,
        None => return Some(default),
    };

    if bound >= 0 {
        return usize::try_from(bound).ok().map(|index| index.min(len - 1));
    }

    let from_newest = bound.unsigned_abs();
    let len = u64::try_from(len).ok()?;
    if from_newest > len {
        Some(0)
    } else {
        usize::try_from(len - from_newest).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        capture_screen_lines, parse_capture_bound, resolve_screen_capture_range,
        ScreenCaptureRange, Transcript,
    };

    #[test]
    fn empty_transcript_captures_empty_output() {
        let transcript = Transcript::new(10);

        assert!(transcript.capture(None, None).is_empty());
    }

    #[test]
    fn single_line_capture_includes_trailing_newline() {
        let mut transcript = Transcript::new(10);
        transcript.append_line("only");

        assert_eq!(transcript.capture(None, None), b"only\n");
    }

    #[test]
    fn capture_clamps_out_of_range_boundaries() {
        let transcript = transcript(["zero", "one", "two"]);

        assert_eq!(transcript.capture(Some(-99), Some(99)), b"zero\none\ntwo\n");
    }

    #[test]
    fn negative_indices_count_back_from_newest_line() {
        let transcript = transcript(["zero", "one", "two", "three"]);

        assert_eq!(transcript.capture(Some(-2), Some(-1)), b"two\nthree\n");
        assert_eq!(transcript.capture(Some(-3), Some(-2)), b"one\ntwo\n");
    }

    #[test]
    fn reversed_relative_ranges_are_swapped() {
        let transcript = transcript(["zero", "one", "two"]);

        assert_eq!(transcript.capture(Some(2), Some(1)), b"one\ntwo\n");
        assert_eq!(transcript.capture(Some(-1), Some(-2)), b"one\ntwo\n");
    }

    #[test]
    fn exact_history_limit_evicts_oldest_lines() {
        let mut transcript = Transcript::new(3);
        transcript.append_line("zero");
        transcript.append_line("one");
        transcript.append_line("two");
        transcript.append_line("three");

        assert_eq!(
            transcript.lines().iter().collect::<Vec<_>>(),
            vec!["one", "two", "three"]
        );
        assert_eq!(transcript.capture(None, None), b"one\ntwo\nthree\n");
    }

    #[test]
    fn screen_range_defaults_to_visible_rows() {
        let lines = ["h0", "h1", "v0", "v1"];
        let range = ScreenCaptureRange::default();

        assert_eq!(
            capture_screen_lines(lines.iter().copied(), lines.len(), 2, range),
            b"v0\nv1\n"
        );
    }

    #[test]
    fn screen_range_dash_captures_full_history_and_visible_rows() {
        let lines = ["h0", "h1", "v0", "v1"];
        let range = ScreenCaptureRange {
            start_is_absolute: true,
            end_is_absolute: true,
            ..ScreenCaptureRange::default()
        };

        assert_eq!(
            capture_screen_lines(lines.iter().copied(), lines.len(), 2, range),
            b"h0\nh1\nv0\nv1\n"
        );
    }

    #[test]
    fn screen_range_negative_values_are_relative_to_history_size() {
        let range = resolve_screen_capture_range(ScreenCaptureRange::new(Some(-1), Some(0)), 2, 4)
            .expect("range exists");
        assert_eq!(range, 1..=2);
    }

    #[test]
    fn screen_range_swaps_reversed_bounds() {
        let range = resolve_screen_capture_range(ScreenCaptureRange::new(Some(1), Some(-1)), 2, 4)
            .expect("range exists");
        assert_eq!(range, 1..=3);
    }

    #[test]
    fn capture_bound_follows_tmux_strtonum_range_and_lexis() {
        // Measured on tmux 3.7b: these values select a bound...
        assert_eq!(parse_capture_bound("-10"), Some(-10));
        assert_eq!(parse_capture_bound(" 3"), Some(3));
        assert_eq!(parse_capture_bound("+3"), Some(3));
        assert_eq!(parse_capture_bound("32767"), Some(32_767));
        assert_eq!(parse_capture_bound("-2147483648"), Some(-2_147_483_648));
        // ...and these fall back to the visible default instead of failing.
        for value in [
            "abc",
            "",
            "#",
            "%H",
            "3x",
            "32768",
            "99999",
            "-2147483649",
            "+-3",
        ] {
            assert_eq!(parse_capture_bound(value), None, "value {value:?}");
        }
    }

    fn transcript(lines: impl IntoIterator<Item = &'static str>) -> Transcript {
        let mut transcript = Transcript::new(10);
        for line in lines {
            transcript.append_line(line);
        }
        transcript
    }
}
