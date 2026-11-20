//! Budget of one top-level format expansion.
//!
//! tmux 3.7b bounds an expansion by time as well as by depth: every
//! `format_expand` records a start time, nested states copy it
//! (`format_copy_state`), and `format_check_time` stops the work once
//! `FORMAT_TIME_LIMIT` milliseconds have elapsed. The depth limit alone does
//! not bound fan-out: `@a = '#{E:@a}#{E:@a}'` costs 2^100 expansions.
//!
//! RMUX applies the same time limit, but only once the expansion has scanned
//! more format text than an ordinary format needs. Below that floor the clock
//! is never read for it, so a descheduled thread cannot truncate an ordinary
//! status line or listing; tmux would truncate it whenever 100 ms of wall time
//! pass.
//!
//! Bytes measure the cost of cheap leaves only. A leaf such as `#{C:zzz}`
//! searches the whole history for a few bytes of format, so a fan-out of such
//! leaves could run for seconds below the byte floor. A ceiling of ten times
//! the tmux limit therefore applies whatever the volume, once the expansion
//! has entered more nested expansions than a short format does.

use std::cell::Cell;
use std::time::{Duration, Instant};

/// Time one top-level expansion may take, matching tmux `FORMAT_TIME_LIMIT`.
pub(super) const FORMAT_TIME_LIMIT: Duration = Duration::from_millis(100);

/// Format bytes one expansion may scan before the time limit applies. The
/// default tmux 3.7b status line scans about 1.2 KiB per window (13 KiB for
/// ten windows, 117 KiB for a hundred); past the floor RMUX behaves as tmux.
pub(super) const FORMAT_TIME_FLOOR_BYTES: usize = 256 * 1024;

/// Time no expansion may exceed, however little format text it scans.
pub(super) const FORMAT_TIME_CEILING: Duration = Duration::from_secs(1);

/// Nested expansions one expansion may enter before the ceiling applies.
/// Measured on RMUX: a list line or the rename format enters one to three,
/// the default status line about 16 per window (49 for one window, 193 for
/// ten). Below this floor the clock is not read for the ceiling either.
pub(super) const FORMAT_CEILING_FLOOR_ENTRIES: usize = 256;

/// A time limit that applies once the expansion has done more than `floor`
/// units of work.
#[derive(Debug, Clone, Copy)]
pub(super) struct WorkTimeLimit {
    pub(super) floor: usize,
    pub(super) limit: Duration,
}

impl WorkTimeLimit {
    /// Returns the time limit once `work` is past the floor.
    fn applies_to(self, work: usize) -> Option<Duration> {
        (work > self.floor).then_some(self.limit)
    }
}

/// Start time, limits and work shared by every nested state of one expansion.
#[derive(Debug)]
pub(super) struct ExpandBudget {
    started: Instant,
    /// The tmux limit, measured in format bytes scanned.
    by_bytes: WorkTimeLimit,
    /// The ceiling, measured in `format_expand1` entries.
    by_entries: WorkTimeLimit,
    scanned_bytes: Cell<usize>,
    entries: Cell<usize>,
}

impl ExpandBudget {
    /// Starts the budget of a new top-level expansion.
    pub(super) fn start() -> Self {
        Self::with_limits(
            WorkTimeLimit {
                floor: FORMAT_TIME_FLOOR_BYTES,
                limit: FORMAT_TIME_LIMIT,
            },
            WorkTimeLimit {
                floor: FORMAT_CEILING_FLOOR_ENTRIES,
                limit: FORMAT_TIME_CEILING,
            },
        )
    }

    /// Starts a budget with explicit limits.
    pub(super) fn with_limits(by_bytes: WorkTimeLimit, by_entries: WorkTimeLimit) -> Self {
        Self {
            started: Instant::now(),
            by_bytes,
            by_entries,
            scanned_bytes: Cell::new(0),
            entries: Cell::new(0),
        }
    }

    /// Records that one `format_expand1` call scans `bytes` of format text.
    pub(super) fn charge(&self, bytes: usize) {
        self.scanned_bytes
            .set(self.scanned_bytes.get().saturating_add(bytes));
        self.entries.set(self.entries.get().saturating_add(1));
    }

    /// Returns whether the expansion has used its time, like tmux
    /// `!format_check_time`, once it has done more work than a floor.
    pub(super) fn is_exhausted(&self) -> bool {
        let limit = match (
            self.by_bytes.applies_to(self.scanned_bytes.get()),
            self.by_entries.applies_to(self.entries.get()),
        ) {
            (None, None) => return false,
            (Some(limit), None) | (None, Some(limit)) => limit,
            (Some(by_bytes), Some(by_entries)) => by_bytes.min(by_entries),
        };
        self.started.elapsed() >= limit
    }
}
