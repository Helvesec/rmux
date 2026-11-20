use super::{parse_event_frame, Error, Event, FRAME_LIMIT};

const KEYBOARD_FINALS: &[u8] = b"u~ABCDEFHPQS";
const PRESERVED_FOREIGN_FINALS: &[u8] = b"mMIORZhlpcnty";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlString {
    Osc,
    StTerminated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForeignCsiEnvelope {
    Complete,
    AmbiguousKeyboardPrefix,
    Invalid,
}

/// One ordered result produced by [`AcquiredDecoder`].
///
/// `Other` borrows either the current input slice or bounded decoder storage
/// and is valid only for the duration of the callback invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum AcquiredItem<'a> {
    /// One validated semantic Kitty keyboard event.
    Event(Event),
    /// Bytes that do not belong to an acquired Kitty keyboard event.
    Other(&'a [u8]),
    /// A discarded candidate and the number of bytes it occupied.
    Rejected(Rejection),
}

/// An acquired frame rejected without exposing its bytes as key input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejection {
    /// The complete-frame or incremental framing failure.
    pub error: Error,
    /// Total candidate bytes discarded, including bytes beyond retained storage.
    pub bytes: usize,
}

/// Bounded incremental decoder for terminal-to-RMUX Kitty key events.
///
/// This decoder does not parse application controls, own a timer, route an
/// event, or activate Kitty handling. It preserves unrelated bytes for the
/// caller and shields bracketed-paste bodies from key interpretation.
#[derive(Debug)]
pub struct AcquiredDecoder {
    pending: Vec<u8>,
    pending_len: usize,
    pending_error: Option<Error>,
    in_foreign_csi: bool,
    control_string: Option<ControlString>,
    control_st_prefix: bool,
    in_bracketed_paste: bool,
    paste_end_match: usize,
}

impl AcquiredDecoder {
    /// Creates an empty acquired-event decoder with bounded retained storage.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Vec::with_capacity(FRAME_LIMIT),
            pending_len: 0,
            pending_error: None,
            in_foreign_csi: false,
            control_string: None,
            control_st_prefix: false,
            in_bracketed_paste: false,
            paste_end_match: 0,
        }
    }

    /// Returns the retained prefix of the current candidate.
    ///
    /// At most [`FRAME_LIMIT`] bytes are retained even when [`Self::pending_len`]
    /// reports a larger quarantined candidate.
    #[must_use]
    pub fn retained(&self) -> &[u8] {
        &self.pending
    }

    /// Returns the total byte count of the current candidate.
    #[must_use]
    pub const fn pending_len(&self) -> usize {
        self.pending_len
    }

    /// Whether the retained numerical CSI candidate must remain quarantined.
    ///
    /// This includes ambiguous invalid prefixes and overlong candidates. A
    /// final-looking byte can still belong to such a prefix; callers must not
    /// reclassify it as completed input. Completed foreign CSI and paste bodies
    /// have already been emitted through their separate framing paths.
    #[must_use]
    pub fn has_pending_keyboard_frame(&self) -> bool {
        self.pending.starts_with(b"\x1b[")
            && self
                .pending
                .get(2)
                .is_some_and(|byte| byte.is_ascii_digit() || *byte == b';' || *byte == b':')
    }

    /// Discards an incomplete candidate when the caller decides it has expired.
    ///
    /// This method performs no timing and chooses no timeout duration.
    pub fn expire(&mut self) -> Option<Rejection> {
        if self.pending_len == 0 {
            return None;
        }
        let rejection = Rejection {
            error: self.pending_error.unwrap_or(Error::Incomplete),
            bytes: self.pending_len,
        };
        self.clear_candidate();
        Some(rejection)
    }

    /// Consumes a stream fragment and emits ordered event, other, and rejection results.
    ///
    /// Ordinary byte runs borrow directly from `bytes`; the decoder does not
    /// allocate an owned result for each byte. The callback must consume each
    /// borrowed `Other` result before returning.
    pub fn feed(&mut self, bytes: &[u8], mut emit: impl FnMut(AcquiredItem<'_>)) {
        let mut cursor = 0;
        while cursor < bytes.len() {
            if self.control_string.is_some() {
                self.feed_control_string(bytes, &mut cursor, &mut emit);
                continue;
            }
            if self.in_foreign_csi {
                self.feed_foreign_csi(bytes, &mut cursor, &mut emit);
                continue;
            }
            if self.in_bracketed_paste {
                self.feed_bracketed_paste(bytes, &mut cursor, &mut emit);
                continue;
            }

            if self.pending_len == 0 {
                let next_escape = bytes[cursor..]
                    .iter()
                    .position(|byte| *byte == 0x1b)
                    .map_or(bytes.len(), |offset| cursor + offset);
                if next_escape > cursor {
                    emit(AcquiredItem::Other(&bytes[cursor..next_escape]));
                    cursor = next_escape;
                    continue;
                }
                self.append(0x1b);
                cursor += 1;
                continue;
            }

            let byte = bytes[cursor];
            if self.pending_len == 1 {
                if byte == b'[' {
                    self.append(byte);
                    cursor += 1;
                } else if let Some(control_string) = control_string(byte) {
                    self.append(byte);
                    cursor += 1;
                    self.emit_other(&mut emit);
                    self.control_string = Some(control_string);
                    self.control_st_prefix = false;
                } else {
                    emit(AcquiredItem::Other(&self.pending));
                    self.clear_candidate();
                }
                continue;
            }

            if byte < 0x20 || byte == 0x7f {
                let error = if self.pending_error == Some(Error::FrameLimit) {
                    Error::FrameLimit
                } else {
                    Error::Interrupted
                };
                emit(AcquiredItem::Rejected(Rejection {
                    error,
                    bytes: self.pending_len,
                }));
                self.clear_candidate();
                continue;
            }

            self.append(byte);
            cursor += 1;
            if self.pending_len == 3 && b"<>=?!".contains(&byte) {
                self.emit_other(&mut emit);
                self.in_foreign_csi = true;
                continue;
            }
            self.maybe_finish(byte, &mut emit);
        }
    }

    fn append(&mut self, byte: u8) {
        self.pending_len = self.pending_len.saturating_add(1);
        if self.pending.len() < FRAME_LIMIT {
            self.pending.push(byte);
        }
        if self.pending_len > FRAME_LIMIT {
            self.pending_error = Some(Error::FrameLimit);
        }
    }

    fn maybe_finish(&mut self, byte: u8, emit: &mut impl FnMut(AcquiredItem<'_>)) {
        if KEYBOARD_FINALS.contains(&byte) {
            self.finish_keyboard(byte, emit);
            return;
        }

        if !is_csi_final(byte) {
            if !byte.is_ascii_digit() && !b":;<=>?".contains(&byte) {
                self.pending_error.get_or_insert(Error::InvalidFrame);
            }
            return;
        }

        if self.pending_error == Some(Error::FrameLimit) {
            self.emit_rejected(Error::FrameLimit, emit);
            return;
        }

        match classify_foreign_csi(&self.pending) {
            ForeignCsiEnvelope::Complete => self.emit_other(emit),
            ForeignCsiEnvelope::Invalid => {
                self.emit_rejected(self.pending_error.unwrap_or(Error::InvalidFrame), emit);
            }
            ForeignCsiEnvelope::AmbiguousKeyboardPrefix => {
                self.pending_error.get_or_insert(Error::InvalidFrame);
            }
        }
    }

    fn finish_keyboard(&mut self, final_byte: u8, emit: &mut impl FnMut(AcquiredItem<'_>)) {
        if self.pending_error == Some(Error::FrameLimit) {
            self.emit_rejected(Error::FrameLimit, emit);
            return;
        }

        let bracketed_paste = self.pending == b"\x1b[200~";
        if !is_modify_other_keys(&self.pending) {
            match parse_event_frame(&self.pending) {
                Ok(event) => {
                    emit(AcquiredItem::Event(event));
                    self.clear_candidate();
                    return;
                }
                Err(Error::UnknownIdentity) if final_byte == b'~' => {}
                Err(error) => {
                    self.emit_rejected(error, emit);
                    return;
                }
            }
        }
        self.emit_other(emit);
        if bracketed_paste {
            self.in_bracketed_paste = true;
            self.paste_end_match = 0;
        }
    }

    fn emit_other(&mut self, emit: &mut impl FnMut(AcquiredItem<'_>)) {
        emit(AcquiredItem::Other(&self.pending));
        self.clear_candidate();
    }

    fn emit_rejected(&mut self, error: Error, emit: &mut impl FnMut(AcquiredItem<'_>)) {
        emit(AcquiredItem::Rejected(Rejection {
            error,
            bytes: self.pending_len,
        }));
        self.clear_candidate();
    }

    fn clear_candidate(&mut self) {
        self.pending.clear();
        self.pending_len = 0;
        self.pending_error = None;
    }

    fn feed_bracketed_paste(
        &mut self,
        bytes: &[u8],
        cursor: &mut usize,
        emit: &mut impl FnMut(AcquiredItem<'_>),
    ) {
        let start = *cursor;
        while *cursor < bytes.len() {
            let byte = bytes[*cursor];
            *cursor += 1;
            if byte == PASTE_END[self.paste_end_match] {
                self.paste_end_match += 1;
                if self.paste_end_match == PASTE_END.len() {
                    self.in_bracketed_paste = false;
                    self.paste_end_match = 0;
                    break;
                }
            } else {
                self.paste_end_match = usize::from(byte == PASTE_END[0]);
            }
        }
        if *cursor > start {
            emit(AcquiredItem::Other(&bytes[start..*cursor]));
        }
    }

    fn feed_control_string(
        &mut self,
        bytes: &[u8],
        cursor: &mut usize,
        emit: &mut impl FnMut(AcquiredItem<'_>),
    ) {
        let control_string = self
            .control_string
            .expect("control-string state must have a family");
        let start = *cursor;
        while *cursor < bytes.len() {
            let byte = bytes[*cursor];
            *cursor += 1;
            let is_terminator = (self.control_st_prefix && byte == b'\\')
                || (control_string == ControlString::Osc && byte == 0x07);
            self.control_st_prefix = byte == 0x1b;
            if is_terminator {
                self.control_string = None;
                self.control_st_prefix = false;
                break;
            }
        }
        if *cursor > start {
            emit(AcquiredItem::Other(&bytes[start..*cursor]));
        }
    }

    fn feed_foreign_csi(
        &mut self,
        bytes: &[u8],
        cursor: &mut usize,
        emit: &mut impl FnMut(AcquiredItem<'_>),
    ) {
        let start = *cursor;
        while *cursor < bytes.len() {
            let byte = bytes[*cursor];
            if byte == 0x1b {
                self.in_foreign_csi = false;
                break;
            }
            *cursor += 1;
            if is_csi_final(byte) || byte < 0x20 || byte == 0x7f {
                self.in_foreign_csi = false;
                break;
            }
        }
        if *cursor > start {
            emit(AcquiredItem::Other(&bytes[start..*cursor]));
        }
    }
}

impl Default for AcquiredDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn is_csi_final(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

fn classify_foreign_csi(frame: &[u8]) -> ForeignCsiEnvelope {
    let Some(payload) = frame.strip_prefix(b"\x1b[") else {
        return ForeignCsiEnvelope::Invalid;
    };
    let Some((&final_byte, body)) = payload.split_last() else {
        return ForeignCsiEnvelope::Invalid;
    };
    if !is_csi_final(final_byte) {
        return ForeignCsiEnvelope::Invalid;
    }

    let mut in_intermediates = false;
    let mut last_parameter = None;
    for byte in body {
        match byte {
            0x30..=0x3f if !in_intermediates => last_parameter = Some(*byte),
            0x20..=0x2f => in_intermediates = true,
            _ => return ForeignCsiEnvelope::Invalid,
        }
    }

    // Except for finals with established preservation precedence, a final
    // immediately after a Kitty field separator and without an intermediate
    // is indistinguishable from a bad byte inside a later CSI-u event. Keep
    // only that residual shape quarantined.
    if !PRESERVED_FOREIGN_FINALS.contains(&final_byte)
        && !in_intermediates
        && matches!(last_parameter, Some(b';' | b':'))
    {
        ForeignCsiEnvelope::AmbiguousKeyboardPrefix
    } else {
        ForeignCsiEnvelope::Complete
    }
}

fn control_string(introducer: u8) -> Option<ControlString> {
    match introducer {
        b']' => Some(ControlString::Osc),
        b'P' | b'X' | b'^' | b'_' => Some(ControlString::StTerminated),
        _ => None,
    }
}

fn is_modify_other_keys(frame: &[u8]) -> bool {
    frame.ends_with(b"~")
        && frame.starts_with(b"\x1b[27;")
        && frame.iter().filter(|byte| **byte == b';').count() == 2
}
