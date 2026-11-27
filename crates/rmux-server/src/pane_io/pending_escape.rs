use std::time::Duration;

use rmux_core::kitty_keyboard::{AcquiredDecoder, FRAME_LIMIT};
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
const CONSUMED_OSC_PREFIXES: &[&[u8]] = &[b"\x1b]10;", b"\x1b]11;", b"\x1b]12;", b"\x1b]52;"];
/// tmux 3.7b `tty_keys_palette` reports an unterminated OSC 4 as a partial
/// key: it waits for the keyboard escape-time, then `first_key` reads it as
/// M-] and the rest as typed text. Its body is short and never streams.
const PALETTE_RESPONSE_PREFIX: &[u8] = b"\x1b]4;";
/// tmux 3.7b raises the partial-key delay to this floor while a query sent to
/// the client terminal is unanswered (tty-keys.c:979-985), so a response split
/// across reads is still read whole instead of as typed keys.
const ACTIVE_QUERY_ESCAPE_TIME: Duration = Duration::from_millis(500);

/// The keyboard deadline a new retention lifetime arms: `escape-time`, raised
/// to tmux's active-query floor while a correlated palette query waits.
pub(super) fn keyboard_escape_time(escape_time: Duration, active_query: bool) -> Duration {
    if active_query {
        escape_time.max(ACTIVE_QUERY_ESCAPE_TIME)
    } else {
        escape_time
    }
}

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
        || is_partial_escape_prefixed_key(input)
        || is_ambiguous_terminal_string_leader(input)
        || is_unterminated_csi_prefix(input)
        || is_quarantined_keyboard_frame(input)
        || is_partial_x10_mouse(input)
        || is_partial_bracketed_paste_start(input)
        || is_partial_kitty_apc_start(input)
        || is_partial_consumed_osc_prefix(input)
        || is_unterminated_palette_response(input)
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

/// `decode_escape_key` waits after Escape and the start of a CSI or SS3 key,
/// tmux's Meta prefix, until the key completes or escape-time expires.
fn is_partial_escape_prefixed_key(input: &[u8]) -> bool {
    match input {
        [b'\x1b', b'\x1b', b'O'] => true,
        [b'\x1b', b'\x1b', b'[', parameters @ ..] => parameters
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b';'),
        _ => false,
    }
}

fn is_unterminated_csi_prefix(input: &[u8]) -> bool {
    input.starts_with(b"\x1b[") && input[2..].iter().all(|byte| (0x20..=0x3f).contains(byte))
}

fn is_quarantined_keyboard_frame(input: &[u8]) -> bool {
    // Admission can retain an invalid numeric candidate past a final-looking
    // byte. Keep its keyboard deadline, using the same framing authority.
    if !input.starts_with(b"\x1b[") {
        return false;
    }
    let mut decoder = rmux_core::kitty_keyboard::AcquiredDecoder::new();
    decoder.feed(input, |_| {});
    decoder.has_pending_keyboard_frame()
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

/// Retained `ESC ] 4 ;` input, its partial openers included. A complete
/// response leaves retention through the decoders, so whatever is still held
/// is unterminated and keeps the keyboard deadline of its first byte.
fn is_unterminated_palette_response(input: &[u8]) -> bool {
    !input.is_empty()
        && (input.starts_with(PALETTE_RESPONSE_PREFIX)
            || PALETTE_RESPONSE_PREFIX.starts_with(input))
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

/// Observe framing before admission can compact an overlong keyboard frame.
pub(super) struct PendingInputDispatch {
    retained_before: usize,
    appended: usize,
    continued_bounded_keyboard: bool,
}

impl PendingInputDispatch {
    pub(super) fn capture(pending: &[u8], bytes: &[u8]) -> Self {
        let mut continued_bounded_keyboard = false;
        if pending.starts_with(b"\x1b")
            && !pending.starts_with(BRACKETED_PASTE_START)
            && pending.len().saturating_add(bytes.len()) > FRAME_LIMIT
        {
            let mut decoder = AcquiredDecoder::new();
            decoder.feed(pending, |_| {});
            if decoder.has_pending_keyboard_frame() || matches!(pending, b"\x1b" | b"\x1b[") {
                let mut crossed_boundary = false;
                decoder.feed(bytes, |_| crossed_boundary = true);
                continued_bounded_keyboard =
                    !crossed_boundary && decoder.has_pending_keyboard_frame();
            }
        }
        Self {
            retained_before: pending.len(),
            appended: bytes.len(),
            continued_bounded_keyboard,
        }
    }
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
        dispatch: PendingInputDispatch,
        pending_input: &[u8],
    ) {
        // An unbounded retained sequence is a continuation when the decoder kept
        // every previously retained byte and every newly appended byte. Any
        // other result means that it consumed or transformed input, so a
        // non-empty suffix belongs to a new retention lifetime even when its
        // grammar kind, length, and contents happen to match the old one.
        let PendingInputDispatch {
            retained_before,
            appended,
            continued_bounded_keyboard,
        } = dispatch;
        let retained_after = pending_input.len();
        let continued_unchanged = retained_before > 0
            && retained_after > 0
            && retained_after == retained_before.saturating_add(appended);
        // The keyboard decoder deliberately caps storage. Its framing result,
        // not stored length or prefix equality, distinguishes continuation from
        // a final/interruption followed by a new (possibly identical) suffix.
        let continued_quarantine =
            continued_bounded_keyboard && is_quarantined_keyboard_frame(pending_input);
        if !continued_unchanged && !continued_quarantine {
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
mod tests;
