//! Byte scanners that skip terminal strings, UTF-8 and partial controls in ignored input.

#[derive(Debug)]
pub(super) struct IgnoredUtf8 {
    pub(super) remaining: u8,
    next_min: u8,
    next_max: u8,
}

impl IgnoredUtf8 {
    pub(super) fn from_lead(byte: u8) -> Option<Self> {
        let (remaining, next_min, next_max) = match byte {
            0xc2..=0xdf => (1, 0x80, 0xbf),
            0xe0 => (2, 0xa0, 0xbf),
            0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
            0xed => (2, 0x80, 0x9f),
            0xf0 => (3, 0x90, 0xbf),
            0xf1..=0xf3 => (3, 0x80, 0xbf),
            0xf4 => (3, 0x80, 0x8f),
            _ => return None,
        };
        Some(Self {
            remaining,
            next_min,
            next_max,
        })
    }

    pub(super) fn consume(&mut self, byte: u8) -> bool {
        if !(self.next_min..=self.next_max).contains(&byte) {
            return false;
        }
        self.remaining -= 1;
        self.next_min = 0x80;
        self.next_max = 0xbf;
        true
    }
}

#[derive(Debug)]
pub(super) struct IgnoredTerminalString {
    bell_terminated: bool,
    previous_was_escape: bool,
    utf8: Option<IgnoredUtf8>,
}

impl IgnoredTerminalString {
    pub(super) fn from_prefix(prefix: &[u8]) -> Option<Self> {
        let bell_terminated = if prefix.starts_with(b"\x1b]") {
            true
        } else if prefix.starts_with(b"\x1bP")
            || prefix.starts_with(b"\x1bX")
            || prefix.starts_with(b"\x1b^")
            || prefix.starts_with(b"\x1b_")
        {
            false
        } else {
            match prefix.first().copied() {
                Some(0x9d) => true,
                Some(0x90 | 0x98 | 0x9e | 0x9f) => false,
                _ => return None,
            }
        };
        Some(Self {
            bell_terminated,
            previous_was_escape: prefix.last() == Some(&b'\x1b'),
            utf8: None,
        })
    }

    pub(super) fn consume(&mut self, byte: u8) -> bool {
        if let Some(utf8) = self.utf8.as_mut() {
            if utf8.consume(byte) {
                if utf8.remaining == 0 {
                    self.utf8 = None;
                }
                self.previous_was_escape = false;
                return false;
            }
            self.utf8 = None;
        }
        if let Some(utf8) = IgnoredUtf8::from_lead(byte) {
            self.utf8 = Some(utf8);
            self.previous_was_escape = false;
            return false;
        }
        let complete = byte == 0x9c
            || (self.bell_terminated && byte == b'\x07')
            || (self.previous_was_escape && byte == b'\\');
        self.previous_was_escape = byte == b'\x1b';
        complete
    }
}

pub(super) fn retain_longest_candidate_prefix(prefix: &mut Vec<u8>, candidates: &[&[u8]]) {
    let keep_from = (0..prefix.len())
        .find(|start| {
            candidates
                .iter()
                .any(|candidate| candidate.starts_with(&prefix[*start..]))
        })
        .unwrap_or(prefix.len());
    if keep_from > 0 {
        prefix.drain(..keep_from);
    }
}

pub(super) fn retain_possible_csi_suffix(prefix: &mut Vec<u8>) {
    let keep_from = prefix
        .iter()
        .rposition(|byte| *byte == b'\x1b')
        .filter(|start| {
            let suffix = &prefix[*start..];
            suffix == b"\x1b"
                || suffix == b"\x1b["
                || (suffix.starts_with(b"\x1b[")
                    && suffix[2..].iter().all(|byte| (0x20..=0x3f).contains(byte)))
        });
    match keep_from {
        Some(start) if start > 0 => {
            prefix.drain(..start);
        }
        Some(_) => {}
        None => prefix.clear(),
    }
}
