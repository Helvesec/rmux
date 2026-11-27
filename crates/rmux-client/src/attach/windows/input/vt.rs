use super::{
    encode_live_input_batch, encode_paste_body_key_event, AttachInput, BatchEvent, ConsoleKeyEvent,
    BRACKETED_PASTE_END, BRACKETED_PASTE_START,
};

const MAX_WIN32_FRAME: usize = 64;

/// Decoder for the console's explicitly requested Win32-in-VT grammar. Paste
/// state follows literal delimiters, never a drained ReadConsoleInputW batch.
#[derive(Default)]
pub(super) struct VtConsoleInput {
    frame: Vec<u8>,
    marker: Vec<u8>,
    paste_open: bool,
    key_surrogate: Option<u16>,
}

impl VtConsoleInput {
    pub(super) fn push(&mut self, inputs: Vec<AttachInput>) -> Vec<AttachInput> {
        let mut output = Vec::new();
        for input in inputs {
            if input.windows_console_key().is_some() {
                self.flush_frame(&mut output);
                self.emit(input, &mut output);
                continue;
            }
            for _ in 0..input.repeat_count() {
                for byte in input.payload() {
                    self.push_byte(*byte, &mut output);
                }
            }
        }
        output
    }

    fn push_byte(&mut self, byte: u8, output: &mut Vec<AttachInput>) {
        if byte == 0x1b {
            self.flush_frame(output);
            self.frame.push(byte);
            return;
        }
        if self.frame.is_empty() {
            self.emit(AttachInput::bytes(vec![byte]), output);
            return;
        }
        self.frame.push(byte);
        if self.frame.len() == 2 && byte == b'[' {
            return;
        }
        if self.frame.len() > 2
            && (byte.is_ascii_digit() || byte == b';')
            && self.frame.len() < MAX_WIN32_FRAME
        {
            return;
        }
        if byte == b'_' {
            if let Some(key) = decode_win32_key(&self.frame) {
                self.frame.clear();
                let inputs = if self.paste_open {
                    let bytes = encode_paste_body_key_event(
                        ConsoleKeyEvent {
                            repeat_count: 1,
                            ..key
                        },
                        &mut self.key_surrogate,
                    );
                    if bytes.is_empty() {
                        Vec::new()
                    } else {
                        vec![AttachInput::repeated_bytes(bytes, key.repeat_count)]
                    }
                } else {
                    encode_live_input_batch(
                        &[BatchEvent::Key(key)],
                        &mut self.key_surrogate,
                        &mut 0,
                    )
                };
                for input in inputs {
                    self.emit(input, output);
                }
                return;
            }
        }
        self.flush_frame(output);
    }

    fn flush_frame(&mut self, output: &mut Vec<AttachInput>) {
        if !self.frame.is_empty() {
            let bytes = std::mem::take(&mut self.frame);
            self.emit(AttachInput::bytes(bytes), output);
        }
    }

    fn emit(&mut self, input: AttachInput, output: &mut Vec<AttachInput>) {
        for _ in 0..input.repeat_count() {
            for byte in input.payload() {
                if *byte == 0x1b {
                    self.marker.clear();
                }
                if *byte == 0x1b || !self.marker.is_empty() {
                    self.marker.push(*byte);
                    if self.marker == BRACKETED_PASTE_START {
                        self.paste_open = true;
                        self.marker.clear();
                    } else if self.marker == BRACKETED_PASTE_END {
                        self.paste_open = false;
                        self.marker.clear();
                    } else if !BRACKETED_PASTE_START.starts_with(&self.marker)
                        && !BRACKETED_PASTE_END.starts_with(&self.marker)
                    {
                        self.marker.clear();
                    }
                }
            }
        }
        if input.windows_console_key().is_none() && input.repeat_count() == 1 {
            if let Some(last) = output.last_mut().filter(|last| {
                last.windows_console_key().is_none()
                    && last.repeat_count() == 1
                    && last.bytes.len() + input.bytes.len() <= super::ATTACH_INPUT_CHUNK_LIMIT
            }) {
                last.bytes.extend_from_slice(input.payload());
                return;
            }
        }
        output.push(input);
    }
}

fn decode_win32_key(frame: &[u8]) -> Option<ConsoleKeyEvent> {
    let body = frame.strip_prefix(b"\x1b[")?.strip_suffix(b"_")?;
    let mut fields = body.split(|byte| *byte == b';');
    let mut values = [0_u32; 6];
    for value in &mut values {
        let field = fields.next()?;
        if field.is_empty() {
            return None;
        }
        for digit in field {
            if !digit.is_ascii_digit() {
                return None;
            }
            *value = value
                .checked_mul(10)?
                .checked_add(u32::from(digit - b'0'))?;
        }
    }
    if fields.next().is_some() || values[3] > 1 {
        return None;
    }
    Some(ConsoleKeyEvent {
        virtual_key_code: values[0].try_into().ok()?,
        virtual_scan_code: values[1].try_into().ok()?,
        unicode_char: values[2].try_into().ok()?,
        key_down: values[3] != 0,
        control_key_state: values[4],
        repeat_count: values[5].try_into().ok()?,
    })
}
