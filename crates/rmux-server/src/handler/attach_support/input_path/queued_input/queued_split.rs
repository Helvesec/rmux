//! How queued input splits against the bracketed paste it leaves open: the focus reports of a live
//! frame, which act as they arrive, apart from its ordinary input and from pasted text.

use super::super::QueuedUnit;
use crate::handler::pane_support::take_incomplete_bracketed_paste_segment;

/// The most input the live path retains while a bracketed paste is open
/// (`MAX_RETAINED_ATTACHED_CONTROL_INPUT`): beyond it, it forwards the text so far.
const RETAINED_PASTE_MAX: usize = rmux_proto::DEFAULT_MAX_FRAME_LENGTH;

/// How many bytes at the end of live input are only the start of a sequence its next bytes
/// complete - kept pending by the live input path until they arrive or the escape time finalises
/// them. Given by the input path, which knows the encodings.
pub(in crate::handler) type UnfinishedTail = fn(&[u8]) -> usize;

/// A live frame's focus reports, which tmux applies as they arrive, apart from the ordinary input
/// around them.
pub(super) struct FocusSplit {
    pub(super) focus_reports: Vec<u8>,
    pub(super) ordinary: Vec<u8>,
    /// The start of a focus report the frame ends with, retained until its next bytes arrive.
    pub(super) retained: Vec<u8>,
}

impl FocusSplit {
    const REPORT_LEN: usize = 3;

    /// Input that goes on as it is.
    pub(super) fn whole(input: Vec<u8>) -> Self {
        Self {
            focus_reports: Vec::new(),
            ordinary: input,
            retained: Vec::new(),
        }
    }

    /// Splits live input that continues the queued input where `paste` leaves it, however the
    /// transport cut it: the text of a bracketed paste, delimiters included, stays ordinary input,
    /// and only reports outside a paste act now. A paste this input leaves open, or what
    /// `unfinished` finds at its end outside a paste, stays retained, as the live input path keeps
    /// it.
    pub(super) fn of_live(input: &[u8], paste: &PasteScan, unfinished: UnfinishedTail) -> Self {
        let carried = paste.carry.len();
        let buffer = [paste.carry.as_slice(), input].concat();
        let mut open = paste.open;
        // Where, in `ordinary`, a paste opened in this input and still open begins.
        let mut opened_at = None;
        let mut split = Self {
            focus_reports: Vec::new(),
            ordinary: Vec::with_capacity(input.len()),
            retained: Vec::new(),
        };
        let mut at = 0;
        while at < buffer.len() {
            let delimiter = PasteScan::next_delimiter(open);
            if buffer[at..].starts_with(delimiter) {
                // The carried start of the delimiter was queued with the input before.
                let end = at + delimiter.len();
                opened_at = (!open && at >= carried).then_some(split.ordinary.len());
                split
                    .ordinary
                    .extend_from_slice(&buffer[at.max(carried)..end]);
                open = !open;
                at = end;
                continue;
            }
            let report = &buffer[at..(at + Self::REPORT_LEN).min(buffer.len())];
            if !open && at >= carried && (report == b"\x1b[I" || report == b"\x1b[O") {
                split.focus_reports.extend_from_slice(report);
                at += Self::REPORT_LEN;
                continue;
            }
            if at >= carried {
                split.ordinary.push(buffer[at]);
            }
            at += 1;
        }
        if !open {
            // Kept pending, the escape time finalises it or its next bytes complete it; queued, a
            // later unit could complete it across that deadline.
            let tail = &input[input.len() - unfinished(input).min(input.len())..];
            if split.ordinary.ends_with(tail) {
                split.retained = split.ordinary.split_off(split.ordinary.len() - tail.len());
            }
        } else if let Some(start) = opened_at {
            // A paste this input opened stays pending with the client, as the live path keeps it:
            // it goes on with the next input or ends at the real escape time, not at the end of a
            // binding. Beyond the bound, the live path forwards the text so far and keeps only the
            // opener and what its next bytes complete; queued as whole pastes, that text leaves no
            // paste open for the end of a binding to flush.
            let mut paste = split.ordinary.split_off(start);
            if let Some(text) =
                take_incomplete_bracketed_paste_segment(&mut paste, RETAINED_PASTE_MAX)
            {
                append_whole_pastes(&mut split.ordinary, &text);
            }
            split.retained = paste;
        }
        split
    }
}

/// Appends `text` to `ordinary` as bracketed pastes the replay forwards whole: each within the bound
/// the live path retains, none cutting a UTF-8 scalar.
fn append_whole_pastes(ordinary: &mut Vec<u8>, mut text: &[u8]) {
    let piece_max = RETAINED_PASTE_MAX - PasteScan::START.len() - PasteScan::END.len();
    while !text.is_empty() {
        let mut end = text.len().min(piece_max);
        if end < text.len() {
            // Back off to the first byte of a scalar the cut would split, at most three bytes.
            end = (end.saturating_sub(3)..=end)
                .rev()
                .find(|at| *at > 0 && text[*at] & 0xc0 != 0x80)
                .unwrap_or(end);
        }
        ordinary.extend_from_slice(PasteScan::START);
        ordinary.extend_from_slice(&text[..end]);
        ordinary.extend_from_slice(PasteScan::END);
        text = &text[end..];
    }
}

/// Where a client's queued input leaves a bracketed paste: open or not, and the start of the next
/// delimiter its last bytes may begin, which the next input completes.
#[derive(Debug, Default, Clone)]
pub(in crate::handler) struct PasteScan {
    open: bool,
    carry: Vec<u8>,
}

impl PasteScan {
    const START: &'static [u8] = b"\x1b[200~";
    const END: &'static [u8] = b"\x1b[201~";

    fn next_delimiter(open: bool) -> &'static [u8] {
        if open {
            Self::END
        } else {
            Self::START
        }
    }

    /// Follows a unit queued after the input scanned so far. Input the escape time finalised
    /// cannot begin a delimiter that later input completes.
    pub(super) fn follow(&mut self, unit: &QueuedUnit) {
        match unit {
            QueuedUnit::Live { bytes, .. } => self.advance(bytes),
            QueuedUnit::EscapeFlushed(bytes) => {
                // The flush neutralises a paste it leaves open.
                self.advance(bytes);
                self.open = false;
                self.carry.clear();
            }
            // A decoded event ends any delimiter the bytes before it began.
            QueuedUnit::KeyboardEvent(_) => self.carry.clear(),
        }
    }

    fn advance(&mut self, bytes: &[u8]) {
        let buffer = [std::mem::take(&mut self.carry).as_slice(), bytes].concat();
        let mut at = 0;
        let mut scanned_until = 0;
        while at < buffer.len() {
            let delimiter = Self::next_delimiter(self.open);
            if buffer[at..].starts_with(delimiter) {
                self.open = !self.open;
                at += delimiter.len();
                scanned_until = at;
            } else {
                at += 1;
            }
        }
        let delimiter = Self::next_delimiter(self.open);
        let tail = &buffer[scanned_until..];
        let carried = (1..delimiter.len())
            .rev()
            .find(|len| tail.len() >= *len && delimiter.starts_with(&tail[tail.len() - len..]))
            .unwrap_or(0);
        self.carry = tail[tail.len() - carried..].to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::{append_whole_pastes, PasteScan, RETAINED_PASTE_MAX};

    #[test]
    fn whole_pastes_stay_within_the_retained_bound_and_never_cut_a_character() {
        let piece_max = RETAINED_PASTE_MAX - PasteScan::START.len() - PasteScan::END.len();
        // An e-acute straddles the largest piece the bound allows.
        let mut text = vec![b'x'; piece_max - 1];
        text.extend_from_slice("é".as_bytes());
        text.extend_from_slice(b"yz");
        let mut ordinary = b"k".to_vec();
        append_whole_pastes(&mut ordinary, &text);
        let expected = [
            b"k".as_slice(),
            PasteScan::START,
            &text[..piece_max - 1],
            PasteScan::END,
            PasteScan::START,
            &text[piece_max - 1..],
            PasteScan::END,
        ]
        .concat();
        assert!(
            ordinary == expected,
            "two whole pastes within the bound, the character whole in the second"
        );
    }
}
