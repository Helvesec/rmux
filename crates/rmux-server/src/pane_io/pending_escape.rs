use std::time::Duration;

use tokio::time::Instant;

const CONSUMED_OSC_PREFIXES: &[&[u8]] = &[
    b"\x1b]4;",
    b"\x1b]10;",
    b"\x1b]11;",
    b"\x1b]12;",
    b"\x1b]52;",
];

pub(super) fn is_pending_escape(input: &[u8]) -> bool {
    is_ambiguous_escape_prefix(input)
        || is_unterminated_consumed_osc(input)
        || is_unterminated_kitty_apc(input)
}

/// Retained input that is still ambiguous between a keystroke and the start
/// of a control sequence: a lone ESC, a CSI/SS3 opener, or a proper prefix of
/// one of the consumed OSC responses (`\x1b]5` could become `\x1b]52;` or be
/// M-] followed by a typed `5`).
fn is_ambiguous_escape_prefix(input: &[u8]) -> bool {
    matches!(input, b"\x1b" | b"\x1b[" | b"\x1bO" | b"\x1b_")
        || CONSUMED_OSC_PREFIXES.iter().any(|prefix| {
            !input.is_empty() && input.len() < prefix.len() && prefix.starts_with(input)
        })
}

/// Retained input that reached a full consumed-OSC prefix whose body has no
/// terminator yet. The body scan only ends on BEL or ST, which ordinary
/// typing never produces, so this state must keep a flush deadline armed or
/// a typed `M-] 5 2 ;` would swallow every subsequent keystroke until the
/// user happens to send `C-g`.
fn is_unterminated_consumed_osc(input: &[u8]) -> bool {
    CONSUMED_OSC_PREFIXES
        .iter()
        .any(|prefix| input.starts_with(prefix))
}

/// Retained input that reached the kitty-graphics APC opener (`\x1b_G`) with
/// no ST yet. Like the consumed-OSC body, the APC scan only ends on `ESC \`,
/// which ordinary typing never produces, so this state must keep a flush
/// deadline armed or a typed `M-_ G` would swallow every subsequent
/// keystroke.
fn is_unterminated_kitty_apc(input: &[u8]) -> bool {
    input.starts_with(b"\x1b_G")
}

#[derive(Debug, Default)]
pub(super) struct PendingEscapeFlush {
    deadline: Option<Instant>,
    unterminated_len: Option<usize>,
}

impl PendingEscapeFlush {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn clear(&mut self) {
        self.deadline = None;
        self.unterminated_len = None;
    }

    pub(super) fn sync(&mut self, pending_input: &[u8], escape_time: Duration) {
        if is_unterminated_consumed_osc(pending_input) || is_unterminated_kitty_apc(pending_input) {
            // Most likely a streaming terminal response or kitty graphics
            // payload: re-arm only when a new fragment extends the retained
            // input. Output-only wakeups must not postpone an abandoned
            // prefix indefinitely.
            if self.unterminated_len != Some(pending_input.len()) {
                self.deadline = Some(Instant::now() + escape_time);
                self.unterminated_len = Some(pending_input.len());
            }
            return;
        }

        self.unterminated_len = None;

        if !is_ambiguous_escape_prefix(pending_input) {
            self.clear();
            return;
        }

        if self.deadline.is_none() {
            self.deadline = Some(Instant::now() + escape_time);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::PendingEscapeFlush;

    #[test]
    fn pending_escape_arms_once_and_clears_on_other_input() {
        let mut flush = PendingEscapeFlush::default();

        flush.sync(b"\x1b", Duration::from_millis(500));
        let first = flush.deadline().expect("escape should arm a deadline");
        flush.sync(b"\x1b", Duration::from_millis(1));

        assert_eq!(flush.deadline(), Some(first));
        flush.sync(b"\x1b[", Duration::from_millis(500));
        assert!(flush.deadline().is_some());
        flush.sync(b"\x1bO", Duration::from_millis(500));
        assert!(flush.deadline().is_some());
        flush.sync(b"\x1b_", Duration::from_millis(500));
        assert!(flush.deadline().is_some());
        flush.sync(b"\x1b]52", Duration::from_millis(500));
        assert!(flush.deadline().is_some());
        flush.sync(b"\x1b[12", Duration::from_millis(500));
        assert!(flush.deadline().is_none());
    }

    #[test]
    fn unterminated_consumed_osc_rearms_only_when_input_grows() {
        // A full consumed-OSC prefix without a terminator previously cleared
        // the deadline while the decoder kept retaining the buffer, so a typed
        // `M-] 5 2 ;` left the attach input swallowed forever. The deadline
        // must stay armed and push forward as body fragments stream in, so a
        // live terminal response is never flushed mid-body.
        let mut flush = PendingEscapeFlush::default();

        flush.sync(b"\x1b]52;", Duration::from_millis(500));
        let armed = flush
            .deadline()
            .expect("unterminated consumed OSC must keep a flush deadline");

        flush.sync(b"\x1b]52;c;AAAA", Duration::from_millis(500));
        let rearmed = flush
            .deadline()
            .expect("body fragments must keep the deadline armed");
        assert!(
            rearmed >= armed,
            "new body fragments must push the deadline forward"
        );

        flush.sync(b"\x1b]52;c;AAAA", Duration::from_millis(1));
        assert_eq!(
            flush.deadline(),
            Some(rearmed),
            "output-only wakeups must not postpone an unchanged pending OSC"
        );

        flush.sync(b"", Duration::from_millis(500));
        assert!(flush.deadline().is_none());
    }

    #[test]
    fn unterminated_kitty_apc_keeps_the_deadline_armed_and_rearms_per_fragment() {
        // A retained `\x1b_G` prefix previously cleared the deadline while
        // the kitty APC decoder kept holding the buffer (its scan only ends
        // on ST, which typing never produces), so a typed `M-_ G` swallowed
        // every subsequent attach keystroke — the same class as the fixed
        // consumed-OSC `M-] 5 2 ;` swallow. The deadline must stay armed and
        // push forward as payload fragments stream in, so a live kitty
        // graphics transfer is never flushed mid-body.
        let mut flush = PendingEscapeFlush::default();

        flush.sync(b"\x1b_G", Duration::from_millis(500));
        let armed = flush
            .deadline()
            .expect("unterminated kitty APC must keep a flush deadline");

        flush.sync(b"\x1b_Gi=7;AAAA", Duration::from_millis(500));
        let rearmed = flush
            .deadline()
            .expect("payload fragments must keep the deadline armed");
        assert!(
            rearmed >= armed,
            "new payload fragments must push the deadline forward"
        );

        flush.sync(b"\x1b_Gi=7;AAAA", Duration::from_millis(1));
        assert_eq!(
            flush.deadline(),
            Some(rearmed),
            "output-only wakeups must not postpone an unchanged pending APC"
        );

        flush.sync(b"", Duration::from_millis(500));
        assert!(flush.deadline().is_none());
    }
}
