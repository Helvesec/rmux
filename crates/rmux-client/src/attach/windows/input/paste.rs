use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_MENU, VK_RCONTROL, VK_RETURN, VK_RMENU,
    VK_RSHIFT, VK_SHIFT,
};

use super::events::{encode_live_input_batch, focus_bytes, BatchEvent, ConsoleKeyEvent};
use super::keys::{
    alt_gr_pressed, ctrl_pressed, encode_paste_body_key_event, meta_pressed, shift_pressed,
};
use super::mouse::encode_mouse_event;
use super::{AttachInput, BRACKETED_PASTE_END, BRACKETED_PASTE_START};

/// A pasted printable character — or a pasted carriage return, which reaches
/// the console as `VK_RETURN` — is normally an unmodified key-down. Console
/// transports synthesize a clipboard line feed as either Ctrl+J or, for Windows
/// OpenSSH/ConPTY, Ctrl+Enter. Those exact Unicode LF shapes are the only
/// Ctrl-modified exceptions. Other Control and Alt chords force the normal
/// per-key path, except for AltGr on European layouts
/// (`LEFT_CTRL|RIGHT_ALT`), which is legitimate paste text. Records with
/// `unicode_char == 0` (pure modifier key-downs synthesized around shifted
/// characters) do not count as text and are ignored here.
pub(super) fn is_paste_body_text(event: &ConsoleKeyEvent) -> bool {
    if !event.key_down || event.unicode_char == 0 {
        return false;
    }
    let alt_gr = alt_gr_pressed(event.control_key_state);
    let ctrl = ctrl_pressed(event.control_key_state) && !alt_gr;
    let meta = meta_pressed(event.control_key_state) && !alt_gr;
    if meta {
        return false;
    }
    if !ctrl {
        return true;
    }
    is_synthesized_paste_lf(event)
}

pub(super) fn is_synthesized_paste_lf(event: &ConsoleKeyEvent) -> bool {
    let alt_gr = alt_gr_pressed(event.control_key_state);
    let known_virtual_key =
        event.virtual_key_code == u16::from(b'J') || event.virtual_key_code == VK_RETURN;
    event.key_down
        && ctrl_pressed(event.control_key_state)
        && !alt_gr
        && !meta_pressed(event.control_key_state)
        && event.unicode_char == b'\n' as u16
        && known_virtual_key
        && !shift_pressed(event.control_key_state)
}

/// Returns the text count of an ambiguous, non-drained body-only prefix. Two
/// body records already prove a fresh paste without lookahead; fewer may need
/// the next console batch to distinguish an interactive key from a split
/// clipboard burst. A synthesized LF, Tab, or Backspace is body text here: two
/// records are still a burst, while each key remains live when it is the only
/// drained record. A drained prefix is never a lookahead candidate: nothing is
/// queued to resolve it, and ConPTY emits the same records for a clipboard LF
/// as for a physical Ctrl+Enter, so holding it back would only trade one
/// mis-classification for a delayed live key. Once an envelope is open, the
/// continuation path owns the state and must not be deferred.
pub(super) fn paste_lookahead(
    events: &[BatchEvent],
    drained: bool,
    paste_open: bool,
) -> Option<usize> {
    if drained || paste_open {
        return None;
    }

    let text_downs = ambiguous_paste_body_count(events)?;
    (text_downs < 2).then_some(text_downs)
}

pub(super) fn ctrl_modifier_down_after(events: &[BatchEvent], mut ctrl_down: bool) -> bool {
    for event in events {
        let BatchEvent::Key(key) = event else {
            continue;
        };
        if matches!(key.virtual_key_code, VK_CONTROL | VK_LCONTROL | VK_RCONTROL) {
            ctrl_down = key.key_down;
        } else if !ctrl_pressed(key.control_key_state) {
            // A focus transition can lose the dedicated Control key-up record.
            // Every subsequent key record still carries the current modifier
            // snapshot, so reconcile the latch as soon as that snapshot says
            // Control is no longer held.
            ctrl_down = false;
        }
    }
    ctrl_down
}

fn contains_only_synthesized_lf_text(events: &[BatchEvent]) -> bool {
    let mut saw_lf = false;
    for event in events {
        let BatchEvent::Key(key) = event else {
            return false;
        };
        if !key.key_down || is_pure_modifier_key_down(key) {
            continue;
        }
        if !is_synthesized_paste_lf(key) {
            return false;
        }
        saw_lf = true;
    }
    saw_lf
}

pub(super) fn is_held_physical_ctrl_lf_batch(events: &[BatchEvent]) -> bool {
    contains_only_synthesized_lf_text(events)
        && !events.iter().any(|event| {
            matches!(
                event,
                BatchEvent::Key(key)
                    if !key.key_down
                        && matches!(
                            key.virtual_key_code,
                            VK_CONTROL | VK_LCONTROL | VK_RCONTROL
                        )
            )
        })
}

/// Validates a prefix containing only single-repeat paste body records plus
/// key-up/modifier noise. Used before and after a bounded lookahead so a split
/// synthesized LF cannot submit the preceding text before the rest of the
/// clipboard payload arrives.
pub(super) fn ambiguous_paste_body_count(events: &[BatchEvent]) -> Option<usize> {
    let mut text_downs = 0usize;
    for event in events {
        let BatchEvent::Key(key) = event else {
            return None;
        };
        if !key.key_down || is_pure_modifier_key_down(key) {
            continue;
        }
        if key.repeat_count != 1 || !is_paste_body_text(key) {
            return None;
        }
        text_downs += 1;
    }
    (text_downs >= 1).then_some(text_downs)
}

/// A pure-modifier key-down (`unicode_char == 0`) — the shift/ctrl/alt records
/// classic conhost interleaves with pasted printable keys — is neither paste
/// text nor a "real" other key. Treating it as either would mis-classify a
/// paste that contains uppercase letters (VK_SHIFT down/up bracket each
/// shifted character) or a paste under AltGr (VK_LCONTROL down/up around each
/// AltGr character). This keeps burst detection stable under any layout.
pub(super) fn is_pure_modifier_key_down(event: &ConsoleKeyEvent) -> bool {
    event.key_down
        && event.unicode_char == 0
        && matches!(
            event.virtual_key_code,
            VK_SHIFT
                | VK_CONTROL
                | VK_MENU
                | VK_LSHIFT
                | VK_RSHIFT
                | VK_LCONTROL
                | VK_RCONTROL
                | VK_LMENU
                | VK_RMENU
        )
}

/// Removes bracketed-paste markers embedded in pasted content before the burst
/// is wrapped, so crafted clipboard data (a hostile copy button placing a raw
/// `ESC[201~`) cannot break out of the paste envelope and inject live
/// keystrokes into the pane, and a stray `ESC[200~` cannot nest a second
/// envelope. This matches how terminals filter the delimiter out of a paste.
///
/// The scan iterates to a fixed point: content like `\x1b[20 + \x1b[201~ +
/// 1~body` recombines into a live `\x1b[201~body` after a single-pass strip,
/// which the client can no longer detect once the burst is wrapped. Looping
/// until no marker was removed guarantees the returned text is free of both
/// literal and reassembled markers.
pub(super) fn strip_embedded_paste_markers(text: &mut Vec<u8>) {
    if text.len() < BRACKETED_PASTE_END.len().min(BRACKETED_PASTE_START.len()) {
        return;
    }
    loop {
        let before = text.len();
        remove_all_subslices(text, BRACKETED_PASTE_END);
        remove_all_subslices(text, BRACKETED_PASTE_START);
        if text.len() == before {
            return;
        }
    }
}

/// Returns the number of trailing bytes to keep behind when emitting a
/// mid-envelope paste chunk, so a bracketed-paste marker whose bytes are split
/// across the current and next batches cannot slip through per-batch stripping.
///
/// The tail is the longest suffix of `text` that is a proper prefix of either
/// `\x1b[200~` or `\x1b[201~` (up to `marker_len - 1` bytes). If no marker
/// prefix matches, we still hold back up to `marker_len - 1` bytes of a lone
/// trailing ESC to guarantee at least the ESC of a future marker cannot escape
/// carrying-over.
fn marker_straddle_hold_len(text: &[u8]) -> usize {
    // Both markers are 6 bytes and share the first 4 bytes (ESC [ 2 0). The
    // fifth byte splits them (`0`|`1`) and the sixth is `~`, so any proper
    // prefix of length 1..=5 is a common ambiguity except position 5 which is
    // marker-specific. Walk from the longest possible prefix down.
    let max_hold = BRACKETED_PASTE_START.len().saturating_sub(1);
    for k in (1..=max_hold).rev() {
        if text.len() < k {
            continue;
        }
        let tail = &text[text.len() - k..];
        if BRACKETED_PASTE_START.starts_with(tail) || BRACKETED_PASTE_END.starts_with(tail) {
            return k;
        }
    }
    0
}

fn remove_all_subslices(buf: &mut Vec<u8>, needle: &[u8]) {
    if needle.is_empty() || buf.len() < needle.len() {
        return;
    }
    let mut out = Vec::with_capacity(buf.len());
    let mut index = 0;
    while index < buf.len() {
        if buf[index..].starts_with(needle) {
            index += needle.len();
        } else {
            out.push(buf[index]);
            index += 1;
        }
    }
    *buf = out;
}

/// Wraps a detected paste burst in bracketed-paste markers so the daemon can
/// keep or strip them per the pane's `?2004` mode, and otherwise encodes the
/// batch key-by-key exactly as before. Pure over its explicit state so it can be
/// unit-tested with the burst pattern conhost actually delivers.
#[cfg(test)]
pub(super) fn encode_input_batch(
    events: &[BatchEvent],
    drained: bool,
    paste_open: &mut bool,
    paste_carryover: &mut Vec<u8>,
    pending_high_surrogate: &mut Option<u16>,
    last_mouse_button_state: &mut u32,
) -> Vec<AttachInput> {
    encode_input_batch_with_hint(
        events,
        drained,
        PasteEncodingHint::default(),
        paste_open,
        paste_carryover,
        pending_high_surrogate,
        last_mouse_button_state,
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct PasteEncodingHint {
    pub(super) force_ambiguous_lookahead_paste: bool,
    pub(super) suppress_fresh_paste: bool,
}

pub(super) fn encode_input_batch_with_hint(
    events: &[BatchEvent],
    drained: bool,
    hint: PasteEncodingHint,
    paste_open: &mut bool,
    paste_carryover: &mut Vec<u8>,
    pending_high_surrogate: &mut Option<u16>,
    last_mouse_button_state: &mut u32,
) -> Vec<AttachInput> {
    let mut text_downs = 0usize;
    let mut has_other_key = false;
    for event in events {
        if let BatchEvent::Key(key) = event {
            if !key.key_down {
                continue;
            }
            if is_pure_modifier_key_down(key) {
                // Pure modifier records (VK_SHIFT down/up around shifted chars,
                // VK_LCONTROL bracketing AltGr) are neither paste text nor a
                // "real" other key — skip them so they neither block nor
                // fabricate a burst.
                continue;
            }
            if is_paste_body_text(key) {
                text_downs += 1;
            } else {
                has_other_key = true;
            }
        }
    }

    // Two body records establish a burst; LF, Tab, Backspace and focus-shaped
    // clipboard records must then remain literal text inside its envelope.
    // A run already open continues on further pasted text until the input
    // buffer drains.
    // Native MOUSE_EVENT records can be coalesced into the same console read as
    // a clipboard burst. They are not evidence that the key records are live:
    // when the keys form a paste, prefer the paste and suppress those mouse
    // records rather than turning clipboard bytes into interactive input.
    // A non-drained batch can contain only key-up or synthetic modifier noise
    // between two chunks of the same clipboard burst. It carries no live input
    // and is not proof that an already-open envelope ended, so leave the state
    // untouched until a body record, a real key, a mouse event, or drain makes
    // the boundary observable.
    let continuation_noise_only = events.iter().all(|event| match event {
        BatchEvent::Key(key) => !key.key_down || is_pure_modifier_key_down(key),
        BatchEvent::Mouse(_) | BatchEvent::Focus(_) => false,
    });
    if *paste_open && !drained && continuation_noise_only {
        return Vec::new();
    }

    let paste_text_only = !has_other_key;
    let continuing_paste = *paste_open
        && (text_downs >= 1
            || events
                .iter()
                .any(|event| matches!(event, BatchEvent::Focus(_))));
    let fresh_paste = !hint.suppress_fresh_paste
        && (text_downs >= 2 || (hint.force_ambiguous_lookahead_paste && text_downs >= 1));
    let is_paste = paste_text_only && (continuing_paste || fresh_paste);

    let mut inputs = Vec::new();
    if is_paste {
        // Emit the markers and the pasted text as ONE keystroke per batch. If
        // the open marker and the text were separate `AttachInput`s the daemon
        // would process the plain-text keystroke through its fast input path
        // (which bypasses the bracketed-paste decode), delivering the paste
        // body to the pane without the markers.
        let opening = !*paste_open;
        // A fresh clipboard burst starts at its first body record. Focus
        // records before that boundary are live, even if the same console
        // read also contains the burst. An already-open paste has no such
        // prefix: its focus-shaped records remain literal continuation text.
        let body_start = if opening {
            events
                .iter()
                .position(|event| matches!(event, BatchEvent::Key(key) if is_paste_body_text(key)))
                .unwrap_or(events.len())
        } else {
            0
        };
        for event in &events[..body_start] {
            if let BatchEvent::Focus(focused) = event {
                inputs.push(AttachInput::bytes(focus_bytes(*focused).to_vec()));
            }
        }
        *paste_open = true;
        let mut payload = Vec::new();
        if opening {
            payload.extend_from_slice(BRACKETED_PASTE_START);
        }
        let mut text = std::mem::take(paste_carryover);
        for event in &events[body_start..] {
            match event {
                BatchEvent::Key(key) => text
                    .extend_from_slice(&encode_paste_body_key_event(*key, pending_high_surrogate)),
                // ConPTY also converts focus-shaped clipboard bytes into focus
                // records. Within a detected paste they are literal text, at
                // their original position, never an out-of-band focus report.
                BatchEvent::Focus(focused) => text.extend_from_slice(focus_bytes(*focused)),
                BatchEvent::Mouse(_) => {}
            }
        }
        // Mouse records coalesced with clipboard text are suppressed so they
        // cannot make the text live or enter its paste envelope. Still feed
        // them through the state tracker: dropping a coalesced release without
        // advancing state would turn the next move into a phantom drag.
        for event in events {
            if let BatchEvent::Mouse(mouse) = event {
                let _suppressed = encode_mouse_event(*mouse, last_mouse_button_state);
            }
        }
        strip_embedded_paste_markers(&mut text);
        if drained {
            // Envelope closes with this batch — nothing more can straddle a
            // boundary, so emit whatever is left as-is.
            payload.extend_from_slice(&text);
            payload.extend_from_slice(BRACKETED_PASTE_END);
            *paste_open = false;
        } else {
            // A hostile `\x1b[201~` (or `\x1b[200~`) could straddle the batch
            // boundary. Hold back a suffix long enough to see any marker whose
            // last byte might arrive in the NEXT batch, and re-strip once we
            // have both halves together.
            let hold = marker_straddle_hold_len(&text);
            let split = text.len() - hold;
            payload.extend_from_slice(&text[..split]);
            paste_carryover.extend_from_slice(&text[split..]);
        }
        if !payload.is_empty() {
            inputs.push(AttachInput::bytes(payload));
        }
        return inputs;
    }

    if *paste_open {
        // Flush any carry-over from the previous batch, re-stripped now that
        // we know no continuation is coming.
        let mut tail = std::mem::take(paste_carryover);
        strip_embedded_paste_markers(&mut tail);
        tail.extend_from_slice(BRACKETED_PASTE_END);
        inputs.push(AttachInput::bytes(tail));
        *paste_open = false;
    }
    inputs.extend(encode_live_input_batch(
        events,
        pending_high_surrogate,
        last_mouse_button_state,
    ));
    inputs
}
