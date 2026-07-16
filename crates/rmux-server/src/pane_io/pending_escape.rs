use std::time::Duration;

use tokio::time::Instant;

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const KITTY_GRAPHICS_APC_START: &[u8] = b"\x1b_G";
const ESCAPE_TERMINAL_STRING_LEADERS: &[u8] = b"]PX^_";
const C1_TERMINAL_STRING_LEADERS: &[u8] = &[0x90, 0x98, 0x9d, 0x9e, 0x9f];
/// Once a variable-length terminal control has an unambiguous opener, it is
/// no longer governed by the keyboard `escape-time`. Network and PTY reads
/// can legitimately pause much longer than that (10 ms by default), so use a
/// separate idle budget and move it only when the retained input grows.
const RETAINED_CONTROL_IDLE_TIMEOUT: Duration = Duration::from_secs(8);
const CONSUMED_OSC_PREFIXES: &[&[u8]] = &[
    b"\x1b]4;",
    b"\x1b]10;",
    b"\x1b]11;",
    b"\x1b]12;",
    b"\x1b]52;",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingEscapeKind {
    /// A short keyboard/control prefix. Its deadline starts at the first
    /// ambiguous fragment and must not move as more parameter bytes arrive.
    Ambiguous,
    /// A recognized variable-length control body. Each new fragment moves a
    /// dedicated, realistic idle deadline so a streaming paste, OSC response,
    /// or Kitty APC is never flushed on the much shorter keyboard escape-time.
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingEscapeProvenance {
    /// A keyboard sequence that has not selected a variable-length control
    /// family yet.
    AmbiguousKeyboard,
    /// The retained input first became schedulable as an unambiguous
    /// variable-length terminal control.
    StreamingControl,
}

#[cfg(test)]
pub(super) fn is_pending_escape(input: &[u8]) -> bool {
    pending_escape_kind(input).is_some()
}

fn pending_escape_kind(input: &[u8]) -> Option<PendingEscapeKind> {
    if is_unterminated_consumed_osc(input)
        || input.starts_with(BRACKETED_PASTE_START)
        || input.starts_with(KITTY_GRAPHICS_APC_START)
    {
        return Some(PendingEscapeKind::Streaming);
    }

    if is_ambiguous_escape_prefix(input) {
        return Some(PendingEscapeKind::Ambiguous);
    }

    if is_unterminated_generic_terminal_string(input) {
        return Some(PendingEscapeKind::Streaming);
    }

    None
}

/// Retained input that is still ambiguous between a keystroke and the start
/// of a control sequence. Keep this grammar aligned with the decoders that can
/// return `Partial`; complete bracketed-paste/APC/OSC openers are classified as
/// streaming above instead.
fn is_ambiguous_escape_prefix(input: &[u8]) -> bool {
    matches!(input, b"\x1b" | b"\x1bO" | b"\x1b\x1b")
        || is_ambiguous_terminal_string_leader(input)
        || is_unterminated_csi_prefix(input)
        || is_partial_x10_mouse(input)
        || is_partial_bracketed_paste_start(input)
        || is_partial_kitty_apc_start(input)
        || is_partial_consumed_osc_prefix(input)
        || is_partial_meta_utf8(input)
}

/// The two-byte 7-bit forms are also ordinary Meta keys, so they keep the
/// keyboard escape deadline until a body byte makes the terminal string
/// unambiguous. A bare C1 leader gets the same short initial budget; once its
/// body starts it is promoted to the streaming idle budget below.
fn is_ambiguous_terminal_string_leader(input: &[u8]) -> bool {
    matches!(input, [b'\x1b', leader] if ESCAPE_TERMINAL_STRING_LEADERS.contains(leader))
        || matches!(input, [leader] if C1_TERMINAL_STRING_LEADERS.contains(leader))
}

/// Modal input keeps incomplete DCS, SOS, OSC, PM, and APC strings opaque so
/// terminal responses cannot be interpreted as prompt/menu keys. Keep the
/// scheduler aligned with both their 7-bit (`ESC` + leader) and C1 forms:
/// after at least one body byte, an abandoned string must have a bounded idle
/// deadline instead of retaining every later keystroke forever.
fn is_unterminated_generic_terminal_string(input: &[u8]) -> bool {
    matches!(input, [b'\x1b', leader, ..] if ESCAPE_TERMINAL_STRING_LEADERS.contains(leader))
        || matches!(input, [leader, ..] if C1_TERMINAL_STRING_LEADERS.contains(leader) && input.len() > 1)
}

fn is_unterminated_csi_prefix(input: &[u8]) -> bool {
    input.starts_with(b"\x1b[") && input[2..].iter().all(|byte| (0x20..=0x3f).contains(byte))
}

fn is_partial_x10_mouse(input: &[u8]) -> bool {
    input.starts_with(b"\x1b[M") && input.len() < 6
}

fn is_partial_bracketed_paste_start(input: &[u8]) -> bool {
    !input.is_empty()
        && input.len() < BRACKETED_PASTE_START.len()
        && BRACKETED_PASTE_START.starts_with(input)
}

fn is_partial_kitty_apc_start(input: &[u8]) -> bool {
    !input.is_empty()
        && input.len() < KITTY_GRAPHICS_APC_START.len()
        && KITTY_GRAPHICS_APC_START.starts_with(input)
}

fn is_partial_consumed_osc_prefix(input: &[u8]) -> bool {
    CONSUMED_OSC_PREFIXES
        .iter()
        .any(|prefix| !input.is_empty() && input.len() < prefix.len() && prefix.starts_with(input))
}

fn is_partial_meta_utf8(input: &[u8]) -> bool {
    let Some((&lead, utf8_tail)) = input.get(1).zip(input.get(1..)) else {
        return false;
    };
    if input.first() != Some(&b'\x1b') {
        return false;
    }

    // `decode_escape_key` retains every two-byte `ESC` + non-ASCII input,
    // including continuation bytes and invalid UTF-8 leads, until one more
    // byte can resolve the ambiguity. Those impossible UTF-8 starts still
    // need the short keyboard deadline; otherwise they remain buffered
    // forever when no third byte arrives.
    if input.len() == 2 && !lead.is_ascii() {
        return true;
    }

    let expected_len = match lead {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return false,
    };
    // This deliberately mirrors `decode_live_attached_key`: until the
    // expected byte count arrives it reports Partial, even if a continuation
    // byte later proves malformed. At the expected length that decoder can
    // resolve the key or reject it, so the timer grammar stops retaining it.
    utf8_tail.len() < expected_len
}

fn is_unterminated_consumed_osc(input: &[u8]) -> bool {
    CONSUMED_OSC_PREFIXES
        .iter()
        .any(|prefix| input.starts_with(prefix))
}

#[derive(Debug, Default)]
pub(super) struct PendingEscapeFlush {
    deadline: Option<Instant>,
    provenance: Option<PendingEscapeProvenance>,
    streaming_len: Option<usize>,
}

impl PendingEscapeFlush {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn clear(&mut self) {
        self.deadline = None;
        self.provenance = None;
        self.streaming_len = None;
    }

    pub(super) fn observe_input_dispatch(
        &mut self,
        retained_before: usize,
        appended: usize,
        pending_input: &[u8],
    ) {
        // A retained sequence is a continuation only when the decoder kept
        // every previously retained byte and every newly appended byte. Any
        // other result means that it consumed or transformed input, so a
        // non-empty suffix belongs to a new retention lifetime even when its
        // grammar kind, length, and contents happen to match the old one.
        let retained_after = pending_input.len();
        let continued_unchanged = retained_before > 0
            && retained_after > 0
            && retained_after == retained_before.saturating_add(appended);
        if !continued_unchanged {
            self.clear();
        }
        // Multiple attach frames may be decoded before the scheduler calls
        // `sync`. Classify every retained state now so the next frame cannot
        // erase whether the lifetime has selected an unambiguous stream or is
        // still a keyboard ambiguity. In particular, a complete `ESC ]52;`
        // or `ESC _G` opener promotes to streaming even when ESC arrived in an
        // earlier frame, and remains streaming if another frame grows it
        // before the first scheduler turn.
        match pending_escape_kind(pending_input) {
            Some(PendingEscapeKind::Ambiguous) => {
                self.provenance = Some(PendingEscapeProvenance::AmbiguousKeyboard);
            }
            Some(PendingEscapeKind::Streaming) => {
                // Once enough bytes select a recognized variable-length
                // control family, framing provenance is no longer ambiguous.
                // Promote even when an earlier read ended after ESC/ESC]/ESC_;
                // otherwise valid OSC/APC input is corrupted solely by a
                // transport boundary at the much shorter keyboard deadline.
                self.provenance = Some(PendingEscapeProvenance::StreamingControl);
            }
            None => self.clear(),
        }
    }

    pub(super) fn sync(&mut self, pending_input: &[u8], escape_time: Duration) {
        match pending_escape_kind(pending_input) {
            Some(PendingEscapeKind::Streaming) => {
                // Only input growth moves a streaming deadline. Output/status
                // wakeups must not keep an abandoned control body alive. The
                // stream idle timeout is intentionally independent from the
                // keyboard escape-time, while respecting a larger explicit
                // escape-time configuration.
                if self.provenance != Some(PendingEscapeProvenance::StreamingControl)
                    || self.streaming_len != Some(pending_input.len())
                {
                    let idle_timeout = std::cmp::max(escape_time, RETAINED_CONTROL_IDLE_TIMEOUT);
                    self.deadline = Some(Instant::now() + idle_timeout);
                    self.streaming_len = Some(pending_input.len());
                }
                self.provenance = Some(PendingEscapeProvenance::StreamingControl);
            }
            Some(PendingEscapeKind::Ambiguous) => {
                self.streaming_len = None;
                let continuing_keyboard_ambiguity = matches!(
                    self.provenance,
                    Some(PendingEscapeProvenance::AmbiguousKeyboard)
                ) && self.deadline.is_some();
                if !continuing_keyboard_ambiguity {
                    self.deadline = Some(Instant::now() + escape_time);
                }
                self.provenance = Some(PendingEscapeProvenance::AmbiguousKeyboard);
            }
            None => self.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use tokio::time::Instant;

    use super::{is_pending_escape, PendingEscapeFlush, RETAINED_CONTROL_IDLE_TIMEOUT};

    #[test]
    fn every_retained_short_escape_family_arms_without_moving_its_deadline() {
        for input in [
            b"\x1b".as_slice(),
            b"\x1b[".as_slice(),
            b"\x1bO".as_slice(),
            b"\x1bP".as_slice(),
            b"\x1bX".as_slice(),
            b"\x1b]".as_slice(),
            b"\x1b^".as_slice(),
            b"\x1b_".as_slice(),
            b"\x1b\x1b".as_slice(),
            b"\x90".as_slice(),
            b"\x98".as_slice(),
            b"\x9d".as_slice(),
            b"\x9e".as_slice(),
            b"\x9f".as_slice(),
            b"\x1b[20".as_slice(),
            b"\x1b[12;".as_slice(),
            b"\x1b[<".as_slice(),
            b"\x1b[M!".as_slice(),
            b"\x1b\xe6".as_slice(),
            b"\x1b\xe6\x97".as_slice(),
            b"\x1b\xf0\x9f".as_slice(),
            b"\x1b\xf0\x9f\x92".as_slice(),
        ] {
            assert!(is_pending_escape(input), "{input:?} must be classified");
            let mut flush = PendingEscapeFlush::default();
            flush.sync(input, Duration::from_millis(500));
            assert!(flush.deadline().is_some(), "{input:?} must arm");
        }

        let mut flush = PendingEscapeFlush::default();
        flush.sync(b"\x1b[", Duration::from_millis(500));
        let first = flush.deadline().expect("CSI opener arms");
        flush.sync(b"\x1b[12", Duration::from_millis(1));
        assert_eq!(
            flush.deadline(),
            Some(first),
            "numeric CSI growth must keep the original ambiguity deadline"
        );

        let mut meta_utf8 = PendingEscapeFlush::default();
        meta_utf8.sync(b"\x1b\xf0", Duration::from_millis(500));
        let first = meta_utf8.deadline().expect("Meta-UTF-8 lead arms");
        meta_utf8.sync(b"\x1b\xf0\x9f\x92", Duration::from_millis(1));
        assert_eq!(
            meta_utf8.deadline(),
            Some(first),
            "Meta-UTF-8 growth must stay classified until its expected length"
        );
    }

    #[test]
    fn invalid_meta_bytes_use_the_keyboard_deadline() {
        for invalid in (0x80..=0xbf).chain(0xf8..=0xff) {
            let input = [b'\x1b', invalid];
            assert!(
                is_pending_escape(&input),
                "ESC + invalid UTF-8 byte {invalid:#04x} is retained by the key decoder"
            );

            let mut flush = PendingEscapeFlush::default();
            let before = Instant::now();
            flush.sync(&input, Duration::from_millis(500));
            let deadline = flush.deadline().expect("retained input must time out");
            assert!(deadline >= before + Duration::from_millis(500));
            assert!(
                deadline < before + Duration::from_secs(1),
                "invalid Meta input must not receive the 8-second stream budget"
            );
        }
    }

    #[test]
    fn streaming_escape_bodies_use_a_long_idle_and_rearm_only_on_growth() {
        for (opener, grown) in [
            (b"\x1b]52;".as_slice(), b"\x1b]52;c;AAAA".as_slice()),
            (b"\x1b_G".as_slice(), b"\x1b_Gi=7;AAAA".as_slice()),
            (b"\x1b[200~".as_slice(), b"\x1b[200~slow body".as_slice()),
        ] {
            let mut flush = PendingEscapeFlush::default();
            let before = Instant::now();
            flush.sync(opener, Duration::from_millis(10));
            let armed = flush.deadline().expect("stream opener must arm");
            assert!(
                armed >= before + RETAINED_CONTROL_IDLE_TIMEOUT,
                "{opener:?} must not inherit the 10 ms keyboard escape-time"
            );
            assert!(
                armed <= Instant::now() + RETAINED_CONTROL_IDLE_TIMEOUT,
                "{opener:?} must still expire after the dedicated idle budget"
            );

            flush.sync(grown, Duration::from_millis(10));
            let rearmed = flush.deadline().expect("stream growth must rearm");
            assert!(rearmed >= armed, "{grown:?} must move the idle deadline");

            flush.sync(grown, Duration::from_millis(1));
            assert_eq!(
                flush.deadline(),
                Some(rearmed),
                "unchanged stream must not move its deadline"
            );
        }
    }

    #[test]
    fn every_modal_terminal_string_body_gets_a_bounded_streaming_deadline() {
        for input in [
            b"\x1bPbody".as_slice(),
            b"\x1bXbody".as_slice(),
            b"\x1b]777;body".as_slice(),
            b"\x1b^body".as_slice(),
            b"\x1b_Qbody".as_slice(),
            b"\x90body".as_slice(),
            b"\x98body".as_slice(),
            b"\x9dbody".as_slice(),
            b"\x9ebody".as_slice(),
            b"\x9fbody".as_slice(),
        ] {
            let mut flush = PendingEscapeFlush::default();
            let before = Instant::now();
            flush.sync(input, Duration::from_millis(10));
            let deadline = flush.deadline().expect("terminal string body must arm");
            assert!(
                deadline >= before + RETAINED_CONTROL_IDLE_TIMEOUT,
                "{input:?} must use the bounded streaming idle budget"
            );
            assert!(
                deadline <= Instant::now() + RETAINED_CONTROL_IDLE_TIMEOUT,
                "{input:?} must not be retained indefinitely"
            );
        }
    }

    #[test]
    fn recognized_terminal_control_openers_promote_to_streaming_deadline() {
        for (prefix, control_like, grown) in [
            (
                b"\x1b]".as_slice(),
                b"\x1b]52;".as_slice(),
                b"\x1b]52;c;AAAA".as_slice(),
            ),
            (
                b"\x1b_".as_slice(),
                b"\x1b_G".as_slice(),
                b"\x1b_Gi=7;AAAA".as_slice(),
            ),
            (
                b"\x1bP".as_slice(),
                b"\x1bPq".as_slice(),
                b"\x1bPquery".as_slice(),
            ),
            (
                b"\x90".as_slice(),
                b"\x90q".as_slice(),
                b"\x90query".as_slice(),
            ),
        ] {
            let mut flush = PendingEscapeFlush::default();
            flush.sync(prefix, Duration::from_millis(500));
            let keyboard_deadline = flush.deadline().expect("Meta prefix must arm");

            let before_stream = Instant::now();
            flush.sync(control_like, Duration::from_millis(500));
            let streaming_deadline = flush.deadline().expect("stream opener must arm");
            assert!(
                streaming_deadline >= before_stream + RETAINED_CONTROL_IDLE_TIMEOUT,
                "{control_like:?} must outlive the Meta-key deadline"
            );
            assert!(streaming_deadline > keyboard_deadline);

            flush.sync(grown, Duration::from_millis(500));
            assert!(
                flush.deadline().expect("grown stream remains armed") >= streaming_deadline,
                "{grown:?} growth must refresh the streaming idle deadline"
            );
        }

        let mut split_prefix = PendingEscapeFlush::default();
        split_prefix.sync(b"\x1b", Duration::from_millis(500));
        let keyboard_deadline = split_prefix.deadline().expect("Escape must arm");
        let before_stream = Instant::now();
        split_prefix.sync(b"\x1b]52;c;AAAA", Duration::from_millis(500));
        assert!(
            split_prefix
                .deadline()
                .is_some_and(|deadline| deadline >= before_stream + RETAINED_CONTROL_IDLE_TIMEOUT),
            "an ESC / ] transport split must promote once the OSC opener is recognized"
        );
        assert!(split_prefix.deadline() > Some(keyboard_deadline));
    }

    #[test]
    fn coalesced_true_osc_and_apc_growth_keep_streaming_provenance() {
        for (opener, grown) in [
            (b"\x1b]52;c;AA".as_slice(), b"\x1b]52;c;AAAA".as_slice()),
            (b"\x1b_Gi=7;PAY".as_slice(), b"\x1b_Gi=7;PAYLOAD".as_slice()),
        ] {
            let mut flush = PendingEscapeFlush::default();
            flush.observe_input_dispatch(0, opener.len(), opener);
            flush.observe_input_dispatch(opener.len(), grown.len() - opener.len(), grown);

            let before = Instant::now();
            flush.sync(grown, Duration::from_millis(500));
            assert!(
                flush
                    .deadline()
                    .is_some_and(|deadline| deadline >= before + RETAINED_CONTROL_IDLE_TIMEOUT),
                "{grown:?} must not acquire Meta provenance while frames coalesce"
            );
        }
    }

    #[test]
    fn validated_paste_and_apc_survive_a_pause_longer_than_escape_time() {
        let escape_time = Duration::from_millis(5);
        let mut paste = PendingEscapeFlush::default();
        let mut apc = PendingEscapeFlush::default();
        paste.sync(b"\x1b[200~slow body", escape_time);
        apc.sync(b"\x1b_Gi=7;slow-body", escape_time);
        let paste_deadline = paste.deadline().expect("paste idle deadline");
        let apc_deadline = apc.deadline().expect("APC idle deadline");

        thread::sleep(Duration::from_millis(15));

        assert!(Instant::now() < paste_deadline);
        assert!(Instant::now() < apc_deadline);
        paste.sync(b"\x1b[200~slow body", escape_time);
        apc.sync(b"\x1b_Gi=7;slow-body", escape_time);
        assert_eq!(paste.deadline(), Some(paste_deadline));
        assert_eq!(apc.deadline(), Some(apc_deadline));
    }

    #[test]
    fn stream_completion_rearms_a_new_ambiguous_suffix_with_escape_time() {
        let mut flush = PendingEscapeFlush::default();
        let paste = b"\x1b[200~body";
        flush.sync(paste, Duration::from_millis(10));
        let streaming_deadline = flush.deadline().expect("paste stream arms");

        flush.observe_input_dispatch(paste.len(), 1, b"\x1b");
        let before_escape = Instant::now();
        flush.sync(b"\x1b", Duration::from_millis(10));
        let escape_deadline = flush.deadline().expect("new Escape suffix arms");
        assert!(escape_deadline >= before_escape + Duration::from_millis(10));
        assert!(escape_deadline < streaming_deadline);

        flush.observe_input_dispatch(1, 3, b"\x1b[12");
        flush.sync(b"\x1b[12", Duration::from_secs(1));
        assert_eq!(
            flush.deadline(),
            Some(escape_deadline),
            "growth of the same keyboard ambiguity keeps its original deadline"
        );

        flush.observe_input_dispatch(4, 3, b"\x1b_G");
        let before_stream = Instant::now();
        flush.sync(b"\x1b_G", Duration::from_millis(10));
        assert!(
            flush.deadline().expect("APC stream arms")
                >= before_stream + RETAINED_CONTROL_IDLE_TIMEOUT,
            "an ambiguous-to-streaming transition restores the long idle budget"
        );
    }

    #[test]
    fn dispatch_observation_distinguishes_continuation_from_replacement() {
        let mut flush = PendingEscapeFlush::default();
        flush.sync(b"\x1b", Duration::from_secs(1));
        let original_deadline = flush.deadline().expect("Escape arms");

        flush.observe_input_dispatch(1, 2, b"\x1b[1");
        flush.sync(b"\x1b[1", Duration::from_secs(3));
        assert_eq!(
            flush.deadline(),
            Some(original_deadline),
            "retaining the entire old prefix plus every appended byte is a continuation"
        );

        flush.observe_input_dispatch(3, 2, b"\x1b");
        assert!(
            flush.deadline().is_none(),
            "consuming input before retaining a suffix starts a new lifetime"
        );
        flush.sync(b"\x1b", Duration::from_secs(3));
        assert!(
            flush.deadline().expect("replacement Escape arms")
                > original_deadline + Duration::from_secs(1),
            "the replacement receives its own deadline"
        );
    }

    #[test]
    fn non_escape_retention_and_complete_sequences_do_not_arm() {
        for input in [
            b"".as_slice(),
            b"plain".as_slice(),
            b"\xe6\x97".as_slice(),
            b"\xf0\x9f\x92".as_slice(),
            b"\x1b\xe6\x97\xa5".as_slice(),
            b"\x1b\xf0\x9f\x92\xa1".as_slice(),
            b"\x1b[A".as_slice(),
            b"\x1b[<a".as_slice(),
        ] {
            let mut flush = PendingEscapeFlush::default();
            flush.sync(input, Duration::from_millis(500));
            assert!(
                flush.deadline().is_none(),
                "{input:?} is not a timed retained escape state"
            );
        }
    }
}
