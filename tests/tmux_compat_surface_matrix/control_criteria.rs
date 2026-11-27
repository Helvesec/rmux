use super::support::*;

// Executed case metadata: every entry is used by its behavioral test, including
// the existing real-pipe pause and lossless-reader scenarios. No helper is a case.
pub(super) struct Criterion {
    pub name: &'static str,
    pub test: &'static str,
    pub argv: &'static [&'static str],
    pub oracle: &'static str,
    pub wait: &'static str,
    pub timeout: Duration,
}

impl Criterion {
    pub fn report(&self, tmux: &Path) {
        eprintln!(
            "control criterion={} test={} argv={:?} oracle={} ({}) wait={} deadline={:?}",
            self.name,
            self.test,
            self.argv,
            tmux.display(),
            self.oracle,
            self.wait,
            self.timeout,
        );
    }
}

pub(super) const FRAMING: Criterion = Criterion {
    name: "framing",
    test:
        "control_acceptance::tmux_compat_control_queued_unclosed_quote_preserves_order_through_eof",
    argv: &["-C", "new-session"],
    oracle: "paired begin/end tuples, initial flag 0, stdin flag 1, monotonic numbers",
    wait: "stdout EOF after three complete replies",
    timeout: Duration::from_secs(15),
};
pub(super) const EOF: Criterion = Criterion {
    name: "EOF",
    test: "control_acceptance::tmux_compat_control_eof_and_empty_line_terminate_after_complete_commands",
    argv: &["-C", "attach-session", "-t", "alpha"],
    oracle: "bare exit; ignore unterminated final input; empty line stops queued input",
    wait: "native client exit and both stdout/stderr collectors",
    timeout: Duration::from_secs(15),
};
pub(super) const ORDER: Criterion = Criterion {
    name: "order",
    test: "control_acceptance::tmux_compat_control_group_replies_keep_individual_frames_and_order",
    argv: &["-C", "attach-session", "-t", "alpha"],
    oracle: "ONE/end, TWO/end, unknown-command/error, THREE/end, exit",
    wait: "native EOF and matched guard tuples",
    timeout: Duration::from_secs(15),
};
pub(super) const NOTIFICATIONS: Criterion = Criterion {
    name: "notifications",
    test: "control_acceptance::tmux_compat_control_window_notifications_follow_mutations_once",
    argv: &["-C", "attach-session", "-t", "alpha"],
    oracle: "window-renamed, unlinked-window-add, unlinked-window-close with exact IDs",
    wait: "READY reply, queued mutations, all notifications and final reply then EOF",
    timeout: Duration::from_secs(15),
};
pub(super) const QUEUED: Criterion = Criterion {
    name: "queued commands",
    test: "control_acceptance::tmux_compat_control_blocked_queue_recovers_after_command_error",
    argv: &["-C", "attach-session", "-t", "alpha"],
    oracle: "wait-for holds later replies; signal releases success/error/success in order",
    wait:
        "wait-for end while later replies are blocked, independent signal, LAST reply, native EOF",
    timeout: Duration::from_secs(15),
};
pub(super) const PAUSE_RESUME: Criterion = Criterion {
    name: "pause/resume",
    test: "control_backpressure::tmux_compat_control_pause_resume_retains_backpressure_output",
    argv: &["-C", "attach-session", "-t", "alpha"],
    oracle: "pause/continue exactly once; paused bytes absent, next burst lossless",
    wait: "producer done files, framed drain markers, real-pipe age pause",
    timeout: Duration::from_secs(15),
};
pub(super) const BACKPRESSURE: Criterion = Criterion {
    name: "backpressure",
    test: "control_before_age::tmux_compat_control_slow_reader_is_lossless_before_pause_age",
    argv: &["-C", "attach-session", "-t", "alpha"],
    oracle: "8 MiB exact for fast/slow peers below pause age, no pause/exit",
    wait: "producer done and framed D54-COMPLETE, before existing 8500 ms deadline",
    timeout: Duration::from_millis(8500),
};
