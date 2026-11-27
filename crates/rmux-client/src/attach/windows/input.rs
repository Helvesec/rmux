use std::io;
use std::os::windows::io::RawHandle;

use rmux_proto::AttachedWindowsConsoleKey;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{INPUT_RECORD, LEFT_CTRL_PRESSED};

use crate::nested::{detect_parent, ClientContextParent};

use super::console_coordination::{ConsoleIoCoordinator, ATTACH_CONSOLE_IO};
use super::console_input_read::{
    read_console_input_batch_with, ConsoleInputApi, ConsoleInputRead, Win32ConsoleInput,
};

mod vt;

mod events;
use events::{append_batch_events, encode_live_input_batch, BatchEvent, ConsoleKeyEvent};

mod keys;
use keys::{encode_key_event, encode_paste_body_key_event, windows_console_key_for_event};

mod mouse;
use mouse::encode_mouse_event;

mod paste;
use paste::{
    ambiguous_paste_body_count, ctrl_modifier_down_after, encode_input_batch_with_hint,
    is_held_physical_ctrl_lf_batch, is_paste_body_text, is_pure_modifier_key_down, paste_lookahead,
    PasteEncodingHint,
};
#[cfg(test)]
use paste::{encode_input_batch, strip_embedded_paste_markers};

const ATTACH_INPUT_CHUNK_LIMIT: usize = 4096;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const CONSOLE_INPUT_RECORD_BATCH: usize = 32;
const MAX_TMUX_SGR_MOUSE_BUTTON: u16 = u8::MAX as u16;
const MAX_TMUX_SGR_MOUSE_FRAME_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AttachInput {
    bytes: Vec<u8>,
    windows_console_key: Option<AttachedWindowsConsoleKey>,
    /// Number of times this logical payload must be emitted by the attach stream.
    /// Kept compact here so a single Win32 record never allocates per repetition.
    repeat_count: u16,
}

impl AttachInput {
    pub(super) fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            windows_console_key: None,
            repeat_count: 1,
        }
    }

    pub(super) fn with_windows_console_key(bytes: Vec<u8>, key: AttachedWindowsConsoleKey) -> Self {
        Self {
            bytes,
            windows_console_key: Some(key),
            repeat_count: key.repeat_count().max(1),
        }
    }

    fn repeated_bytes(bytes: Vec<u8>, repeat_count: u16) -> Self {
        Self {
            bytes,
            windows_console_key: None,
            repeat_count: repeat_count.max(1),
        }
    }

    pub(super) fn payload(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn windows_console_key(&self) -> Option<AttachedWindowsConsoleKey> {
        self.windows_console_key
    }

    pub(super) const fn repeat_count(&self) -> u16 {
        self.repeat_count
    }
}

pub(super) fn synthetic_ctrl_c_input() -> AttachInput {
    AttachInput::with_windows_console_key(
        vec![0x03],
        AttachedWindowsConsoleKey::new(b'C' as u16, 0x2e, 0x03, LEFT_CTRL_PRESSED, 1),
    )
}

pub(super) struct ConsoleInputReader {
    handle: HANDLE,
    pending_high_surrogate: Option<u16>,
    last_mouse_button_state: u32,
    /// True while a bracketed-paste run (opened by a detected paste burst) is
    /// still being emitted across `ReadConsoleInputW` batches.
    paste_open: bool,
    /// Trailing bytes of the previous batch's stripped paste body that could
    /// still form part of a bracketed-paste marker straddling the boundary.
    /// Emitted with the next batch's body (and re-stripped as a whole) so a
    /// hostile `\x1b[201~` spanning two batches cannot slip through per-batch
    /// stripping. Only ever populated while `paste_open` is true.
    paste_carryover: Vec<u8>,
    /// Physical Control modifier state observed across console reads. ConPTY
    /// brackets a synthesized paste LF with Control down/up in the same group;
    /// a held physical chord remains down across repeated Enter records.
    ctrl_modifier_down: bool,
    /// Whether a real outer tmux may deliver SGR mouse frames as Win32 key
    /// records. This exception is intentionally unavailable outside tmux.
    tmux_parent_sgr_mouse: bool,
    vt_input: bool,
    vt_decoder: vt::VtConsoleInput,
}

impl ConsoleInputReader {
    pub(super) fn from_handle(handle: RawHandle) -> Option<Self> {
        let handle = handle as HANDLE;
        let vt_input = super::console_input_read::native_vt_input_enabled(handle).ok()?;
        Some(Self {
            handle,
            vt_decoder: vt::VtConsoleInput::default(),
            pending_high_surrogate: None,
            last_mouse_button_state: 0,
            paste_open: false,
            paste_carryover: Vec::new(),
            ctrl_modifier_down: false,
            tmux_parent_sgr_mouse: parent_allows_sgr_mouse_key_batches(detect_parent()),
            vt_input,
        })
    }

    pub(super) fn read_key_inputs(&mut self) -> io::Result<Vec<AttachInput>> {
        self.vt_input = super::console_input_read::native_vt_input_enabled(self.handle)?;
        self.read_key_inputs_with(&ATTACH_CONSOLE_IO, &Win32ConsoleInput)
    }

    pub(super) fn reset_after_exclusive_action(&mut self) {
        self.vt_decoder = vt::VtConsoleInput::default();
        self.pending_high_surrogate = None;
        self.last_mouse_button_state = 0;
        self.paste_open = false;
        self.paste_carryover.clear();
        self.ctrl_modifier_down = false;
    }

    fn read_key_inputs_with<Api>(
        &mut self,
        coordinator: &ConsoleIoCoordinator,
        api: &Api,
    ) -> io::Result<Vec<AttachInput>>
    where
        Api: ConsoleInputApi,
    {
        let mut events = Vec::with_capacity(CONSOLE_INPUT_RECORD_BATCH * 2);
        let mut records = [INPUT_RECORD::default(); CONSOLE_INPUT_RECORD_BATCH];
        let ConsoleInputRead::Records {
            records_read,
            mut drained,
        } = read_console_input_batch_with(coordinator, api, self.handle, &mut records)?
        else {
            // Teardown may flush an input which was readable immediately
            // before the attach input-read lease was acquired. Preserve all
            // decoder and paste state until a real record batch is available.
            return Ok(Vec::new());
        };

        append_batch_events(&records[..records_read], &mut events);
        if self.vt_input {
            return Ok(self.vt_decoder.push(encode_live_input_batch(
                &events,
                &mut self.pending_high_surrogate,
                &mut self.last_mouse_button_state,
            )));
        }
        self.ctrl_modifier_down = ctrl_modifier_down_after(&events, self.ctrl_modifier_down);

        let mut force_ambiguous_lookahead_paste = false;
        if let Some(body_before_lookahead) = paste_lookahead(&events, drained, self.paste_open) {
            let lookahead_events_start = events.len();
            // A non-drained prefix guarantees a queued record, so this second
            // read is nonblocking and can safely remain under the same outer
            // input-read lease.
            match read_console_input_batch_with(coordinator, api, self.handle, &mut records)? {
                ConsoleInputRead::NoEvents => drained = true,
                ConsoleInputRead::Records {
                    records_read,
                    drained: lookahead_drained,
                } => {
                    append_batch_events(&records[..records_read], &mut events);
                    self.ctrl_modifier_down = ctrl_modifier_down_after(
                        &events[lookahead_events_start..],
                        self.ctrl_modifier_down,
                    );
                    drained = lookahead_drained;
                }
            }
            let body_after_lookahead = ambiguous_paste_body_count(&events);
            let gained_body =
                body_after_lookahead.is_some_and(|after| after > body_before_lookahead);
            force_ambiguous_lookahead_paste =
                body_after_lookahead.is_some_and(|_| !drained || gained_body);
        }

        Ok(self.encode_console_events(
            &events,
            drained,
            PasteEncodingHint {
                force_ambiguous_lookahead_paste,
                suppress_fresh_paste: self.ctrl_modifier_down
                    && is_held_physical_ctrl_lf_batch(&events),
            },
        ))
    }

    fn encode_console_events(
        &mut self,
        events: &[BatchEvent],
        drained: bool,
        hint: PasteEncodingHint,
    ) -> Vec<AttachInput> {
        if self.tmux_parent_sgr_mouse {
            if let Some(inputs) = encode_drained_tmux_sgr_mouse_batch(
                events,
                drained,
                self.paste_open,
                &mut self.pending_high_surrogate,
            ) {
                return inputs;
            }
        }

        // Legacy record mode has no explicit paste delimiters. Retain its
        // existing burst classifier only outside the VT transport context.
        encode_input_batch_with_hint(
            events,
            drained,
            hint,
            &mut self.paste_open,
            &mut self.paste_carryover,
            &mut self.pending_high_surrogate,
            &mut self.last_mouse_button_state,
        )
    }
}

const fn parent_allows_sgr_mouse_key_batches(parent: ClientContextParent) -> bool {
    matches!(parent, ClientContextParent::Tmux)
}

/// Restores the one input shape an outer tmux can make authoritative on older
/// Windows hosts: a completely drained batch containing only complete SGR
/// mouse frames. Real bracketed paste includes its `ESC[200~`/`ESC[201~`
/// delimiters and therefore cannot match this grammar. Ordinary text, mixed
/// input, fragments, malformed fields, and multi-batch input all remain on the
/// fail-closed paste path.
///
/// Win32 record mode cannot distinguish an unbracketed clipboard containing
/// exactly one valid SGR frame from the same frame relayed by tmux. Limiting the
/// exception to an inherited tmux context, a drained batch, and this bounded
/// grammar keeps that unavoidable ambiguity narrower than the reported path.
fn encode_drained_tmux_sgr_mouse_batch(
    events: &[BatchEvent],
    drained: bool,
    paste_open: bool,
    pending_high_surrogate: &mut Option<u16>,
) -> Option<Vec<AttachInput>> {
    if !drained || paste_open || pending_high_surrogate.is_some() || events.is_empty() {
        return None;
    }

    let mut candidate = Vec::new();
    let mut candidate_surrogate = *pending_high_surrogate;
    for event in events {
        let BatchEvent::Key(key) = event else {
            return None;
        };
        if key.key_down && !is_paste_body_text(key) && !is_pure_modifier_key_down(key) {
            return None;
        }
        candidate.extend_from_slice(&encode_key_event(*key, &mut candidate_surrogate));
    }
    if !is_complete_bounded_sgr_mouse_run(&candidate) {
        return None;
    }

    *pending_high_surrogate = candidate_surrogate;
    Some(vec![AttachInput::bytes(candidate)])
}

fn is_complete_bounded_sgr_mouse_run(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(frame_len) = bounded_sgr_mouse_frame_len(&bytes[cursor..]) else {
            return false;
        };
        cursor += frame_len;
    }
    true
}

fn bounded_sgr_mouse_frame_len(bytes: &[u8]) -> Option<usize> {
    if !bytes.starts_with(b"\x1b[<") {
        return None;
    }

    let (button, button_end) = parse_bounded_sgr_decimal(bytes, 3, b';')?;
    if button > MAX_TMUX_SGR_MOUSE_BUTTON {
        return None;
    }
    let (x, x_end) = parse_bounded_sgr_decimal(bytes, button_end + 1, b';')?;
    let (y, y_end) = parse_bounded_sgr_decimal_with_final(bytes, x_end + 1)?;
    if x == 0 || y == 0 {
        return None;
    }
    let frame_len = y_end + 1;
    (frame_len <= MAX_TMUX_SGR_MOUSE_FRAME_BYTES).then_some(frame_len)
}

fn parse_bounded_sgr_decimal(bytes: &[u8], start: usize, terminator: u8) -> Option<(u16, usize)> {
    parse_bounded_sgr_decimal_until(bytes, start, |byte| byte == terminator)
}

fn parse_bounded_sgr_decimal_with_final(bytes: &[u8], start: usize) -> Option<(u16, usize)> {
    parse_bounded_sgr_decimal_until(bytes, start, |byte| matches!(byte, b'M' | b'm'))
}

fn parse_bounded_sgr_decimal_until(
    bytes: &[u8],
    start: usize,
    is_terminator: impl Fn(u8) -> bool,
) -> Option<(u16, usize)> {
    let mut cursor = start;
    let mut value = 0_u16;
    let mut has_digit = false;
    while let Some(byte) = bytes.get(cursor).copied() {
        if is_terminator(byte) {
            return has_digit.then_some((value, cursor));
        }
        if !byte.is_ascii_digit() {
            return None;
        }
        has_digit = true;
        value = value.checked_mul(10)?.checked_add(u16::from(byte - b'0'))?;
        cursor += 1;
        if cursor.saturating_sub(start) >= MAX_TMUX_SGR_MOUSE_FRAME_BYTES {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod input_escape_tests;

mod input_trace;
use input_trace::{trace_console_key_event, trace_windows_console_key};

pub(super) fn attach_input_chunks(bytes: &[u8]) -> AttachInputChunks<'_> {
    AttachInputChunks { bytes, offset: 0 }
}

pub(super) struct AttachInputChunks<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for AttachInputChunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.bytes.len() {
            return None;
        }

        let start = self.offset;
        let ideal_end = start
            .saturating_add(ATTACH_INPUT_CHUNK_LIMIT)
            .min(self.bytes.len());
        let end = if ideal_end == self.bytes.len() {
            ideal_end
        } else {
            bounded_chunk_end(self.bytes, start, ideal_end)
        };
        self.offset = end;
        Some(&self.bytes[start..end])
    }
}

fn bounded_chunk_end(bytes: &[u8], start: usize, ideal_end: usize) -> usize {
    let end = avoid_utf8_split(bytes, start, ideal_end);
    let end = avoid_bracketed_paste_marker_split(bytes, start, end);
    if end > start {
        end
    } else {
        ideal_end
    }
}

fn avoid_utf8_split(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start
        && end < bytes.len()
        && bytes
            .get(end)
            .is_some_and(|byte| is_utf8_continuation(*byte))
    {
        end -= 1;
    }
    end
}

fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn avoid_bracketed_paste_marker_split(bytes: &[u8], start: usize, end: usize) -> usize {
    for marker in [BRACKETED_PASTE_START, BRACKETED_PASTE_END] {
        if let Some(adjusted) = marker_adjusted_end(bytes, start, end, marker) {
            return adjusted;
        }
    }
    end
}

fn marker_adjusted_end(bytes: &[u8], start: usize, end: usize, marker: &[u8]) -> Option<usize> {
    let search_start = end
        .saturating_sub(marker.len().saturating_sub(1))
        .max(start);
    for marker_start in search_start..end {
        let prefix = &bytes[marker_start..end];
        if !prefix.is_empty()
            && marker.starts_with(prefix)
            && marker_start + marker.len() <= bytes.len()
        {
            return Some(marker_start + marker.len());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::super::console_coordination::ConsoleIoCoordinator;
    use super::super::console_input_read::ConsoleInputApi;
    use super::{
        attach_input_chunks, encode_drained_tmux_sgr_mouse_batch, encode_input_batch,
        encode_key_event, encode_mouse_event, is_complete_bounded_sgr_mouse_run,
        parent_allows_sgr_mouse_key_batches, strip_embedded_paste_markers,
        windows_console_key_for_event, AttachInput, BatchEvent, ConsoleInputReader,
        ConsoleKeyEvent, ATTACH_INPUT_CHUNK_LIMIT, BRACKETED_PASTE_END, BRACKETED_PASTE_START,
    };
    use crate::nested::ClientContextParent;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED, INPUT_RECORD, INPUT_RECORD_0,
        KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED,
        MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED, MOUSE_WHEELED,
        RIGHTMOST_BUTTON_PRESSED, RIGHT_ALT_PRESSED, SHIFT_PRESSED,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F5, VK_HOME, VK_LEFT,
        VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
    };

    include!("../../attach_windows/input_focus_tests.rs");

    #[test]
    fn paste_chunks_preserve_bracketed_paste_markers() {
        let mut input = vec![b'a'; ATTACH_INPUT_CHUNK_LIMIT - 2];
        input.extend_from_slice(BRACKETED_PASTE_START);
        input.extend_from_slice(b"line one\r\nline two");
        input.extend_from_slice(BRACKETED_PASTE_END);

        let chunks = collect_chunks(&input);

        assert_eq!(chunks.concat(), input);
        assert_eq!(
            chunks[0].len(),
            ATTACH_INPUT_CHUNK_LIMIT - 2 + BRACKETED_PASTE_START.len()
        );
    }

    #[test]
    fn paste_chunks_do_not_split_utf8_scalars() {
        let mut input = vec![b'a'; ATTACH_INPUT_CHUNK_LIMIT - 1];
        input.extend_from_slice("東".as_bytes());
        input.extend_from_slice(" tail".as_bytes());

        let chunks = collect_chunks(&input);

        assert_eq!(chunks.concat(), input);
        assert_eq!(chunks[0].len(), ATTACH_INPUT_CHUNK_LIMIT - 1);
        assert!(std::str::from_utf8(&chunks[1]).is_ok());
    }

    #[test]
    fn paste_chunks_preserve_control_bytes() {
        let mut input = Vec::from([0x02, b'w', 0x03]);
        input.extend(vec![b'x'; ATTACH_INPUT_CHUNK_LIMIT + 32]);

        let chunks = collect_chunks(&input);

        assert_eq!(chunks.concat(), input);
        assert_eq!(&chunks[0][..3], &[0x02, b'w', 0x03]);
    }

    fn collect_chunks(input: &[u8]) -> Vec<Vec<u8>> {
        attach_input_chunks(input)
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>()
    }

    fn batch_bytes(inputs: &[AttachInput]) -> Vec<u8> {
        inputs
            .iter()
            .flat_map(|input| input.payload().to_vec())
            .collect()
    }

    fn key_event_batch(bytes: &[u8]) -> Vec<BatchEvent> {
        bytes
            .iter()
            .map(|byte| {
                let virtual_key = if *byte == b'\x1b' {
                    VK_ESCAPE
                } else {
                    u16::from(*byte)
                };
                BatchEvent::Key(key_event(virtual_key, u16::from(*byte), 0))
            })
            .collect()
    }

    fn bracketed_text(bytes: &[u8]) -> Vec<u8> {
        let mut framed = BRACKETED_PASTE_START.to_vec();
        framed.extend_from_slice(bytes);
        framed.extend_from_slice(BRACKETED_PASTE_END);
        framed
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakeConsoleCall {
        EventCount,
        Flush,
        Read,
    }

    struct FakeConsoleState {
        records: VecDeque<INPUT_RECORD>,
        deferred_records: VecDeque<INPUT_RECORD>,
        read_caps: VecDeque<usize>,
        calls: Vec<FakeConsoleCall>,
    }

    struct FakeConsoleInput {
        state: Mutex<FakeConsoleState>,
    }

    impl FakeConsoleInput {
        fn new(records: impl IntoIterator<Item = INPUT_RECORD>) -> Self {
            Self {
                state: Mutex::new(FakeConsoleState {
                    records: records.into_iter().collect(),
                    deferred_records: VecDeque::new(),
                    read_caps: VecDeque::new(),
                    calls: Vec::new(),
                }),
            }
        }

        fn with_deferred(
            records: impl IntoIterator<Item = INPUT_RECORD>,
            deferred_records: impl IntoIterator<Item = INPUT_RECORD>,
        ) -> Self {
            Self {
                state: Mutex::new(FakeConsoleState {
                    records: records.into_iter().collect(),
                    deferred_records: deferred_records.into_iter().collect(),
                    read_caps: VecDeque::new(),
                    calls: Vec::new(),
                }),
            }
        }

        fn with_read_caps(
            records: impl IntoIterator<Item = INPUT_RECORD>,
            read_caps: impl IntoIterator<Item = usize>,
        ) -> Self {
            Self {
                state: Mutex::new(FakeConsoleState {
                    records: records.into_iter().collect(),
                    deferred_records: VecDeque::new(),
                    read_caps: read_caps.into_iter().collect(),
                    calls: Vec::new(),
                }),
            }
        }

        fn with_deferred_and_read_caps(
            records: impl IntoIterator<Item = INPUT_RECORD>,
            deferred_records: impl IntoIterator<Item = INPUT_RECORD>,
            read_caps: impl IntoIterator<Item = usize>,
        ) -> Self {
            Self {
                state: Mutex::new(FakeConsoleState {
                    records: records.into_iter().collect(),
                    deferred_records: deferred_records.into_iter().collect(),
                    read_caps: read_caps.into_iter().collect(),
                    calls: Vec::new(),
                }),
            }
        }

        fn flush(&self) {
            let mut state = self.state.lock().expect("fake console remains usable");
            state.calls.push(FakeConsoleCall::Flush);
            state.records.clear();
        }

        fn release_deferred(&self) {
            let mut state = self.state.lock().expect("fake console remains usable");
            let deferred = std::mem::take(&mut state.deferred_records);
            state.records.extend(deferred);
        }

        fn calls(&self) -> Vec<FakeConsoleCall> {
            self.state
                .lock()
                .expect("fake console remains usable")
                .calls
                .clone()
        }
    }

    impl ConsoleInputApi for FakeConsoleInput {
        fn event_count(&self, _handle: HANDLE) -> io::Result<u32> {
            let mut state = self.state.lock().expect("fake console remains usable");
            state.calls.push(FakeConsoleCall::EventCount);
            u32::try_from(state.records.len())
                .map_err(|_| io::Error::other("fake console record count overflow"))
        }

        fn read_records(&self, _handle: HANDLE, records: &mut [INPUT_RECORD]) -> io::Result<usize> {
            let mut state = self.state.lock().expect("fake console remains usable");
            state.calls.push(FakeConsoleCall::Read);
            let read_cap = state.read_caps.pop_front().unwrap_or(records.len());
            let records_read = records.len().min(read_cap).min(state.records.len());
            for record in records.iter_mut().take(records_read) {
                *record = state
                    .records
                    .pop_front()
                    .expect("the fake queue length was checked");
            }
            Ok(records_read)
        }
    }

    fn console_key_input_record(byte: u8) -> INPUT_RECORD {
        console_key_input_record_from_event(key_event(u16::from(byte), u16::from(byte), 0))
    }

    fn console_paste_input_record(byte: u8) -> INPUT_RECORD {
        let event = match byte {
            b'\r' => key_event(VK_RETURN, b'\r' as u16, 0),
            // Windows OpenSSH/ConPTY synthesizes a raw LF as Ctrl+Enter, not
            // Ctrl+J: VK_RETURN, scan 0x1c, U+000A, LEFT_CTRL_PRESSED.
            b'\n' => conpty_lf_event(LEFT_CTRL_PRESSED),
            _ => key_event(u16::from(byte), u16::from(byte), 0),
        };
        console_key_input_record_from_event(event)
    }

    fn console_key_input_record_from_event(event: ConsoleKeyEvent) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: i32::from(event.key_down),
                    wRepeatCount: event.repeat_count,
                    wVirtualKeyCode: event.virtual_key_code,
                    wVirtualScanCode: event.virtual_scan_code,
                    uChar: KEY_EVENT_RECORD_0 {
                        UnicodeChar: event.unicode_char,
                    },
                    dwControlKeyState: event.control_key_state,
                },
            },
        }
    }

    fn console_mouse_input_record(event: MOUSE_EVENT_RECORD) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: MOUSE_EVENT as u16,
            Event: INPUT_RECORD_0 { MouseEvent: event },
        }
    }

    fn fake_console_reader() -> ConsoleInputReader {
        ConsoleInputReader {
            handle: std::ptr::null_mut(),
            pending_high_surrogate: None,
            last_mouse_button_state: 0,
            paste_open: false,
            paste_carryover: Vec::new(),
            ctrl_modifier_down: false,
            tmux_parent_sgr_mouse: false,
            vt_input: false,
            vt_decoder: super::vt::VtConsoleInput::default(),
        }
    }

    /// The complete physical Ctrl+Enter chord ConPTY delivers for a human key
    /// press: Control down, the exact `VK_RETURN`/`0x1c`/`U+000A` record, its
    /// key-up, then Control up.
    fn physical_ctrl_enter_group() -> Vec<INPUT_RECORD> {
        let mut ctrl_down = key_event(VK_CONTROL, 0, LEFT_CTRL_PRESSED);
        ctrl_down.virtual_scan_code = 0x1d;
        let mut ctrl_up = ctrl_down;
        ctrl_up.key_down = false;
        ctrl_up.control_key_state = 0;
        let down = conpty_lf_event(LEFT_CTRL_PRESSED);
        let mut up = down;
        up.key_down = false;
        [ctrl_down, down, up, ctrl_up]
            .into_iter()
            .map(console_key_input_record_from_event)
            .collect()
    }

    /// Asserts that `inputs` is exactly one live Ctrl+Enter keystroke carrying
    /// its Windows console-key sidecar, with no bracketed-paste framing.
    fn assert_live_ctrl_enter(inputs: &[AttachInput], context: &str) {
        assert_eq!(batch_bytes(inputs), b"\x1b[13;5u", "{context}");
        assert_eq!(inputs.len(), 1, "{context}");
        assert!(
            !batch_bytes(inputs).starts_with(BRACKETED_PASTE_START),
            "{context}"
        );
        let key = inputs[0]
            .windows_console_key()
            .unwrap_or_else(|| panic!("{context}: the physical key keeps its console sidecar"));
        assert_eq!(key.virtual_key_code(), VK_RETURN, "{context}");
        assert_eq!(key.virtual_scan_code(), 0x1c, "{context}");
        assert_eq!(key.unicode_char(), b'\n' as u16, "{context}");
        assert_eq!(key.control_key_state(), LEFT_CTRL_PRESSED, "{context}");
        assert_eq!(key.repeat_count(), 1, "{context}");
    }

    #[test]
    fn exclusive_action_reset_discards_decoder_state_from_dropped_input() {
        let mut reader = fake_console_reader();
        reader.pending_high_surrogate = Some(0xd83d);
        reader.last_mouse_button_state = FROM_LEFT_1ST_BUTTON_PRESSED;
        reader.paste_open = true;
        reader.paste_carryover = b"\x1b[20".to_vec();
        reader.ctrl_modifier_down = true;

        reader.reset_after_exclusive_action();

        assert_eq!(reader.pending_high_surrogate, None);
        assert_eq!(reader.last_mouse_button_state, 0);
        assert!(!reader.paste_open);
        assert!(reader.paste_carryover.is_empty());
        assert!(!reader.ctrl_modifier_down);
    }

    #[test]
    fn poisoned_coordinator_skips_console_read_without_mutating_decoder_state() {
        let coordinator = ConsoleIoCoordinator::new();
        assert!(std::panic::catch_unwind(|| {
            let _ = coordinator.synchronized(|| panic!("console operation panic"));
        })
        .is_err());
        let api = FakeConsoleInput::new([console_key_input_record(b'x')]);
        let mut reader = fake_console_reader();
        reader.pending_high_surrogate = Some(0xd83d);
        reader.last_mouse_button_state = FROM_LEFT_1ST_BUTTON_PRESSED;
        reader.paste_open = true;
        reader.paste_carryover = b"\x1b[20".to_vec();
        reader.ctrl_modifier_down = true;

        for _ in 0..2 {
            let error = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect_err("poison must propagate from the batch read");
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), "Windows console coordinator poisoned");
            assert!(api.calls().is_empty(), "must not query or consume records");
            assert_eq!(reader.pending_high_surrogate, Some(0xd83d));
            assert_eq!(reader.last_mouse_button_state, FROM_LEFT_1ST_BUTTON_PRESSED);
            assert!(reader.paste_open);
            assert_eq!(reader.paste_carryover, b"\x1b[20");
            assert!(reader.ctrl_modifier_down);
        }
    }

    #[test]
    fn readiness_then_teardown_flush_skips_read_without_mutating_decoder_state() {
        let api = FakeConsoleInput::new([console_key_input_record(b'x')]);
        let handle = std::ptr::null_mut();
        assert_eq!(
            api.event_count(handle).expect("readiness probe succeeds"),
            1,
            "the outer wait observed readable input"
        );
        api.flush();

        let mut reader = fake_console_reader();
        reader.pending_high_surrogate = Some(0xd83d);
        reader.last_mouse_button_state = FROM_LEFT_1ST_BUTTON_PRESSED;
        reader.paste_open = true;
        reader.paste_carryover = b"\x1b[20".to_vec();
        let coordinator = ConsoleIoCoordinator::new();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the flushed race is an empty nonblocking read");

        assert!(inputs.is_empty());
        assert_eq!(reader.pending_high_surrogate, Some(0xd83d));
        assert_eq!(reader.last_mouse_button_state, FROM_LEFT_1ST_BUTTON_PRESSED);
        assert!(reader.paste_open);
        assert_eq!(reader.paste_carryover, b"\x1b[20");
        assert_eq!(
            api.calls(),
            [
                FakeConsoleCall::EventCount,
                FakeConsoleCall::Flush,
                FakeConsoleCall::EventCount,
            ],
            "the coordinated recheck must return before the blocking read"
        );
    }

    #[test]
    fn coordinated_console_reads_preserve_multi_batch_paste_framing() {
        let source = vec![b'a'; super::CONSOLE_INPUT_RECORD_BATCH + 1];
        let api = FakeConsoleInput::new(source.iter().copied().map(console_key_input_record));
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let first = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the first console batch is read");
        assert!(
            reader.paste_open,
            "one queued record remains after the batch"
        );
        let second = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the final console batch is read");

        let actual = first
            .iter()
            .chain(&second)
            .flat_map(|input| input.payload().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(actual, bracketed_text(&source));
        assert!(!reader.paste_open, "the drained batch closes the paste");
        assert_eq!(
            api.calls(),
            [
                FakeConsoleCall::EventCount,
                FakeConsoleCall::Read,
                FakeConsoleCall::EventCount,
                FakeConsoleCall::EventCount,
                FakeConsoleCall::Read,
                FakeConsoleCall::EventCount,
            ]
        );
    }

    #[test]
    fn undrained_first_paste_character_is_joined_to_the_next_batch() {
        let first = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = first;
        key_up.key_down = false;
        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(31),
        );
        records.push(console_key_input_record(b'b'));

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the bounded lookahead resolves the split paste");
        assert_eq!(batch_bytes(&inputs), bracketed_text(b"ab"));
        assert!(!reader.paste_open);
    }

    #[test]
    fn isolated_key_followed_only_by_keyups_remains_live() {
        let first = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = first;
        key_up.key_down = false;
        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(32),
        );

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the drained lookahead contains no second text record");

        assert_eq!(batch_bytes(&inputs), b"a");
        assert!(!reader.paste_open);
    }

    #[test]
    fn short_lf_bursts_are_bracketed_when_the_console_is_drained() {
        let ctrl_lf = key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED);
        for (events, expected) in [
            (
                [key_event(b'a' as u16, b'a' as u16, 0), ctrl_lf],
                b"a\n".as_slice(),
            ),
            (
                [ctrl_lf, key_event(b'b' as u16, b'b' as u16, 0)],
                b"\nb".as_slice(),
            ),
            ([ctrl_lf, ctrl_lf], b"\n\n".as_slice()),
        ] {
            let api = FakeConsoleInput::new(events.map(console_key_input_record_from_event));
            let coordinator = ConsoleIoCoordinator::new();
            let mut reader = fake_console_reader();
            let inputs = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect("a two-record LF burst is paste evidence");

            assert_eq!(batch_bytes(&inputs), bracketed_text(expected));
            assert!(!reader.paste_open);
        }
    }

    #[test]
    fn short_lf_burst_ignores_keyup_noise() {
        let ctrl_lf = key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED);
        let text = key_event(b'b' as u16, b'b' as u16, 0);
        let mut key_up = text;
        key_up.key_down = false;
        let mut records = vec![
            console_key_input_record_from_event(ctrl_lf),
            console_key_input_record_from_event(text),
        ];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(30),
        );

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("keyup noise does not erase the two-record burst");

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"\nb"));
        assert!(!reader.paste_open);
    }

    #[test]
    fn synthesized_lf_singleton_lookahead_distinguishes_paste_from_keyup() {
        for (suffix, expected, bracketed) in [
            (b"bc".as_slice(), b"\nbc".as_slice(), true),
            (&[][..], b"\n".as_slice(), false),
        ] {
            let lf = key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED);
            let mut lf_up = lf;
            lf_up.key_down = false;
            let mut records = vec![console_key_input_record_from_event(lf)];
            records.extend(
                std::iter::repeat_with(|| console_key_input_record_from_event(lf_up)).take(31),
            );
            if suffix.is_empty() {
                records.push(console_key_input_record_from_event(lf_up));
            } else {
                records.extend(suffix.iter().copied().map(console_key_input_record));
            }

            let api = FakeConsoleInput::new(records);
            let coordinator = ConsoleIoCoordinator::new();
            let mut reader = fake_console_reader();
            let inputs = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect("LF singleton lookahead remains bounded");

            if bracketed {
                assert_eq!(batch_bytes(&inputs), bracketed_text(expected));
            } else {
                assert_eq!(batch_bytes(&inputs), expected);
            }
            assert!(!reader.paste_open);
        }
    }

    #[test]
    fn drained_conpty_lf_never_waits_for_a_late_continuation() {
        let api = FakeConsoleInput::with_deferred(
            [console_key_input_record_from_event(conpty_lf_event(
                LEFT_CTRL_PRESSED,
            ))],
            [console_key_input_record(b'b')],
        );
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let initial = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("a drained singleton is classified from the buffer alone");
        assert_live_ctrl_enter(&initial, "drained singleton LF");

        api.release_deferred();
        let late = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("a later batch cannot retroactively consume the released key");
        assert_eq!(batch_bytes(&late), b"b");
        assert!(!reader.paste_open);
    }

    #[test]
    fn drained_conpty_lf_without_a_continuation_stays_live() {
        let api = FakeConsoleInput::new([console_key_input_record_from_event(conpty_lf_event(
            LEFT_CTRL_PRESSED,
        ))]);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("an unmatched synthesized LF is live input");

        assert_live_ctrl_enter(&inputs, "unmatched synthesized LF");
        assert!(!reader.paste_open);
    }

    #[test]
    fn complete_conpty_lf_group_stays_live_when_the_queue_is_drained() {
        let api = FakeConsoleInput::with_deferred(
            physical_ctrl_enter_group(),
            [console_key_input_record(b'b')],
        );
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let initial = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the captured ConPTY chord is classified immediately");
        assert_live_ctrl_enter(&initial, "complete drained chord");

        api.release_deferred();
        let late = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the later text is classified on its own evidence");
        assert_eq!(batch_bytes(&late), b"b");
        assert!(!reader.paste_open);
    }

    #[test]
    fn conpty_lf_groups_split_across_drained_batches_stay_live() {
        let mut ctrl_down = key_event(VK_CONTROL, 0, LEFT_CTRL_PRESSED);
        ctrl_down.virtual_scan_code = 0x1d;
        let mut first = physical_ctrl_enter_group();
        first.push(console_key_input_record_from_event(ctrl_down));
        let second = physical_ctrl_enter_group()
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>();
        let api = FakeConsoleInput::with_deferred(first, second);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let initial = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the first drained chord is live input");
        assert_live_ctrl_enter(&initial, "first drained chord");

        api.release_deferred();
        let second_inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the second drained chord is also live input");
        assert_live_ctrl_enter(&second_inputs, "second drained chord");
        assert!(!reader.paste_open);
    }

    #[test]
    fn a_closed_paste_never_converts_a_following_physical_ctrl_enter() {
        // 349/350/351 ms were the boundaries of the removed recent-paste
        // window and 0 ms is the case it always captured. A closed envelope is
        // not evidence about the next chord at any delay.
        for delay_ms in [0, 349, 350, 351, 700] {
            let coordinator = ConsoleIoCoordinator::new();
            let mut reader = fake_console_reader();
            let prefix = FakeConsoleInput::new([
                console_key_input_record(b'a'),
                console_key_input_record(b'b'),
            ]);
            let prefix_inputs = reader
                .read_key_inputs_with(&coordinator, &prefix)
                .expect("the drained prefix is one complete paste");
            assert_eq!(batch_bytes(&prefix_inputs), bracketed_text(b"ab"));
            assert!(!reader.paste_open, "the drained paste closed its envelope");

            if delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }

            let trailing = FakeConsoleInput::new(physical_ctrl_enter_group());
            let inputs = reader
                .read_key_inputs_with(&coordinator, &trailing)
                .expect("a physical Ctrl+Enter after a closed paste is live input");

            assert_live_ctrl_enter(&inputs, &format!("{delay_ms} ms after a closed paste"));
            assert!(!reader.paste_open);
        }
    }

    #[test]
    fn a_queued_lf_continuation_forms_one_envelope_without_a_live_prefix() {
        let records = std::iter::once(console_key_input_record_from_event(conpty_lf_event(
            LEFT_CTRL_PRESSED,
        )))
        .chain(b"KLMNO".iter().copied().map(console_key_input_record));
        let api = FakeConsoleInput::with_read_caps(records, [1]);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the queued continuation resolves the non-drained prefix");

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"\nKLMNO"));
        assert_eq!(inputs.len(), 1, "one keystroke carries the whole envelope");
        assert!(
            inputs
                .iter()
                .all(|input| input.windows_console_key().is_none()),
            "a paste body carries no console-key sidecar"
        );
        assert!(!reader.paste_open);
    }

    #[test]
    fn stale_control_latch_cannot_break_an_open_paste_continuation() {
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        reader.paste_open = true;
        reader.ctrl_modifier_down = true;
        let mut records = b"abcdefghijklmnop"
            .iter()
            .copied()
            .map(console_key_input_record)
            .collect::<Vec<_>>();
        records.push(console_key_input_record_from_event(conpty_lf_event(
            LEFT_CTRL_PRESSED,
        )));
        let continuation = FakeConsoleInput::new(records);

        let inputs = reader
            .read_key_inputs_with(&coordinator, &continuation)
            .expect("an open paste remains authoritative over stale modifier state");

        let mut expected = b"abcdefghijklmnop\n".to_vec();
        expected.extend_from_slice(BRACKETED_PASTE_END);
        assert_eq!(batch_bytes(&inputs), expected);
        assert!(!reader.ctrl_modifier_down);
        assert!(!reader.paste_open);
    }

    #[test]
    fn neutral_key_snapshot_reconciles_a_lost_control_keyup() {
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        reader.ctrl_modifier_down = true;
        let paste = FakeConsoleInput::new([
            console_key_input_record(b'a'),
            console_key_input_record(b'b'),
        ]);

        let inputs = reader
            .read_key_inputs_with(&coordinator, &paste)
            .expect("neutral key snapshots clear a stale Control latch");

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"ab"));
        assert!(!reader.ctrl_modifier_down);
    }

    #[test]
    fn stale_control_latch_cannot_suppress_a_looked_ahead_paste() {
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        reader.ctrl_modifier_down = true;
        let records = std::iter::once(console_key_input_record_from_event(conpty_lf_event(
            LEFT_CTRL_PRESSED,
        )))
        .chain(b"KLMNO".iter().copied().map(console_key_input_record));
        let paste = FakeConsoleInput::with_read_caps(records, [1]);

        let inputs = reader
            .read_key_inputs_with(&coordinator, &paste)
            .expect("the lookahead reconciles stale Control state before classification");

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"\nKLMNO"));
        assert!(!reader.ctrl_modifier_down);
        assert!(!reader.paste_open);
    }

    #[test]
    fn a_noise_only_lookahead_releases_the_lf_live_and_leaks_no_envelope() {
        let mut ctrl_down = key_event(VK_CONTROL, 0, LEFT_CTRL_PRESSED);
        ctrl_down.virtual_scan_code = 0x1d;
        let mut ctrl_up = ctrl_down;
        ctrl_up.key_down = false;
        ctrl_up.control_key_state = 0;
        let lf_down = conpty_lf_event(LEFT_CTRL_PRESSED);
        let mut lf_up = lf_down;
        lf_up.key_down = false;
        let paste = FakeConsoleInput::with_deferred_and_read_caps(
            [ctrl_down, lf_down, lf_up, ctrl_up]
                .into_iter()
                .map(console_key_input_record_from_event),
            b"KLMNO".iter().copied().map(console_key_input_record),
            [2, 2],
        );
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        reader.ctrl_modifier_down = true;

        let initial = reader
            .read_key_inputs_with(&coordinator, &paste)
            .expect("the one queued continuation is consumed under the same lease");
        assert_live_ctrl_enter(&initial, "chord completed by a noise-only lookahead");
        assert!(!reader.ctrl_modifier_down);
        assert!(!reader.paste_open, "no envelope may stay open behind it");

        paste.release_deferred();
        let later = reader
            .read_key_inputs_with(&coordinator, &paste)
            .expect("the later write is classified on its own evidence");
        assert_eq!(batch_bytes(&later), bracketed_text(b"KLMNO"));
        assert_eq!(later.len(), 1, "the later write is one complete envelope");
        assert!(!reader.paste_open);
    }

    #[test]
    fn drained_physical_ctrl_enter_keyup_keeps_the_key_live() {
        let mut ctrl_down = key_event(VK_CONTROL, 0, LEFT_CTRL_PRESSED);
        ctrl_down.virtual_scan_code = 0x1d;
        let down = conpty_lf_event(LEFT_CTRL_PRESSED);
        let mut up = down;
        up.key_down = false;
        let api = FakeConsoleInput::with_deferred(
            [ctrl_down, down]
                .into_iter()
                .map(console_key_input_record_from_event),
            [console_key_input_record_from_event(up)],
        );
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let initial = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("a still-held physical Ctrl+Enter is live input");
        assert_live_ctrl_enter(&initial, "still-held physical chord");

        api.release_deferred();
        let keyup = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the matching key-up carries no input of its own");
        assert!(keyup.is_empty());
        assert!(!reader.paste_open);
    }

    #[test]
    fn drained_physical_ctrl_enter_completed_in_one_batch_stays_live() {
        let api = FakeConsoleInput::new(physical_ctrl_enter_group());
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("a fully buffered chord with no paste history is live input");

        assert_live_ctrl_enter(&inputs, "fully buffered chord");
        assert!(!reader.paste_open);
    }

    #[test]
    fn held_physical_ctrl_enter_auto_repeat_stays_live() {
        let mut ctrl_down = key_event(VK_CONTROL, 0, LEFT_CTRL_PRESSED);
        ctrl_down.virtual_scan_code = 0x1d;
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let modifier = FakeConsoleInput::new([console_key_input_record_from_event(ctrl_down)]);
        let modifier_inputs = reader
            .read_key_inputs_with(&coordinator, &modifier)
            .expect("the held modifier state is observed");
        assert!(modifier_inputs.is_empty());
        assert!(reader.ctrl_modifier_down);

        let repeated = FakeConsoleInput::with_deferred(
            [console_key_input_record_from_event(conpty_lf_event(
                LEFT_CTRL_PRESSED,
            ))],
            [console_key_input_record_from_event(conpty_lf_event(
                LEFT_CTRL_PRESSED,
            ))],
        );
        let first_inputs = reader
            .read_key_inputs_with(&coordinator, &repeated)
            .expect("physical Enter auto-repeat remains live while Control is held");
        assert_eq!(
            batch_bytes(&first_inputs),
            b"\x1b[13;5u",
            "each repeat is released as it is read"
        );
        repeated.release_deferred();
        let second_inputs = reader
            .read_key_inputs_with(&coordinator, &repeated)
            .expect("the next physical Enter repeat remains live");
        let inputs = first_inputs
            .into_iter()
            .chain(second_inputs)
            .collect::<Vec<_>>();

        assert_eq!(batch_bytes(&inputs), b"\x1b[13;5u\x1b[13;5u");
        assert!(!batch_bytes(&inputs).starts_with(BRACKETED_PASTE_START));
        assert!(reader.ctrl_modifier_down);
        assert!(!reader.paste_open);
    }

    #[test]
    fn repeated_conpty_paste_lf_groups_remain_bracketed() {
        let mut records = Vec::new();
        for _ in 0..2 {
            let mut ctrl_down = key_event(VK_CONTROL, 0, LEFT_CTRL_PRESSED);
            ctrl_down.virtual_scan_code = 0x1d;
            let mut ctrl_up = ctrl_down;
            ctrl_up.key_down = false;
            ctrl_up.control_key_state = 0;
            let down = conpty_lf_event(LEFT_CTRL_PRESSED);
            let mut up = down;
            up.key_down = false;
            records.extend(
                [ctrl_down, down, up, ctrl_up]
                    .into_iter()
                    .map(console_key_input_record_from_event),
            );
        }
        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("complete ConPTY modifier groups remain paste evidence");

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"\n\n"));
        assert!(!reader.ctrl_modifier_down);
        assert!(!reader.paste_open);
    }

    #[test]
    fn single_text_lookahead_flushes_live_before_a_control_chord() {
        let first = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = first;
        key_up.key_down = false;
        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(31),
        );
        records.push(console_key_input_record_from_event(key_event(
            b'C' as u16,
            0x03,
            LEFT_CTRL_PRESSED,
        )));

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the lookahead control chord keeps both records live");

        assert_eq!(batch_bytes(&inputs), b"a\x03");
        assert!(!batch_bytes(&inputs).starts_with(BRACKETED_PASTE_START));
    }

    #[test]
    fn single_text_lookahead_preserves_a_following_mouse_event_in_order() {
        let first = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = first;
        key_up.key_down = false;
        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(31),
        );
        records.push(console_mouse_input_record(mouse_event(
            FROM_LEFT_1ST_BUTTON_PRESSED,
            0,
            0,
            3,
            4,
        )));

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("mouse lookahead keeps the ambiguous character live");

        assert_eq!(batch_bytes(&inputs), b"a\x1b[<0;4;5M");
        assert!(!reader.paste_open);
    }

    #[test]
    fn split_paste_with_coalesced_mouse_matches_single_batch_policy() {
        let first = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = first;
        key_up.key_down = false;
        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(31),
        );
        records.push(console_key_input_record(b'b'));
        records.push(console_mouse_input_record(mouse_event(
            FROM_LEFT_1ST_BUTTON_PRESSED,
            0,
            0,
            3,
            4,
        )));

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("mouse coalesced with a split paste is suppressed");

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"ab"));
        assert_eq!(
            reader.last_mouse_button_state, FROM_LEFT_1ST_BUTTON_PRESSED,
            "suppressed mouse input still advances the state tracker"
        );
        assert!(!reader.paste_open);
    }

    #[test]
    fn isolated_first_character_and_multiline_lookahead_form_one_paste() {
        let marker_source = b"X\x1b[201~Y\nZ";
        for (source, expected_body) in [
            (b"FGHIJ\nKLMNO".as_slice(), b"FGHIJ\nKLMNO".as_slice()),
            (b"ONE-1\r\nONE-2".as_slice(), b"ONE-1\r\nONE-2".as_slice()),
            (marker_source.as_slice(), b"XY\nZ".as_slice()),
        ] {
            let first = key_event(u16::from(source[0]), u16::from(source[0]), 0);
            let mut key_up = first;
            key_up.key_down = false;
            let mut records = vec![console_key_input_record_from_event(first)];
            records.extend(
                std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(31),
            );
            records.extend(source[1..].iter().copied().map(console_paste_input_record));

            let api = FakeConsoleInput::new(records);
            let coordinator = ConsoleIoCoordinator::new();
            let mut reader = fake_console_reader();
            let inputs = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect("lookahead reads the multiline paste continuation");

            assert_eq!(batch_bytes(&inputs), bracketed_text(expected_body));
            assert!(!reader.paste_open);
        }
    }

    #[test]
    fn multiline_paste_continuation_keeps_one_envelope_across_reads() {
        let mut source = vec![b'a'; super::CONSOLE_INPUT_RECORD_BATCH];
        source.extend_from_slice(b"\nb");
        let api = FakeConsoleInput::new(source.iter().copied().map(console_paste_input_record));
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();

        let first = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the first paste batch opens the envelope");
        assert!(reader.paste_open);
        let second = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the LF continuation closes the same envelope");

        let combined = first
            .iter()
            .chain(&second)
            .flat_map(|input| input.payload().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(combined, bracketed_text(&source));
        assert!(!reader.paste_open);
    }

    #[test]
    fn open_paste_ignores_undrained_key_noise_before_an_lf_continuation() {
        let text = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = text;
        key_up.key_down = false;
        let modifier = key_event(VK_SHIFT, 0, SHIFT_PRESSED);

        for noise in [key_up, modifier] {
            let mut records = std::iter::repeat_with(|| console_key_input_record_from_event(text))
                .take(super::CONSOLE_INPUT_RECORD_BATCH)
                .collect::<Vec<_>>();
            records.extend(
                std::iter::repeat_with(|| console_key_input_record_from_event(noise))
                    .take(super::CONSOLE_INPUT_RECORD_BATCH),
            );
            records.push(console_paste_input_record(b'\n'));

            let api = FakeConsoleInput::new(records);
            let coordinator = ConsoleIoCoordinator::new();
            let mut reader = fake_console_reader();

            let first = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect("the first text batch opens one paste envelope");
            assert!(reader.paste_open);

            let noise_output = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect("non-drained key noise preserves the envelope");
            assert!(noise_output.is_empty());
            assert!(reader.paste_open);

            let last = reader
                .read_key_inputs_with(&coordinator, &api)
                .expect("the LF continuation closes the original envelope");
            let combined = first
                .iter()
                .chain(&last)
                .flat_map(|input| input.payload().to_vec())
                .collect::<Vec<_>>();
            let mut body = vec![b'a'; super::CONSOLE_INPUT_RECORD_BATCH];
            body.push(b'\n');
            assert_eq!(combined, bracketed_text(&body));
            assert!(!reader.paste_open);
        }
    }

    #[test]
    fn drained_key_noise_closes_an_open_paste() {
        let mut paste_open = true;
        let mut carryover = Vec::new();
        let text = key_event(b'a' as u16, b'a' as u16, 0);
        let mut key_up = text;
        key_up.key_down = false;

        let inputs = encode_input_batch(
            &[BatchEvent::Key(key_up)],
            true,
            &mut paste_open,
            &mut carryover,
            &mut None,
            &mut 0,
        );

        assert_eq!(batch_bytes(&inputs), BRACKETED_PASTE_END);
        assert!(!paste_open);
    }

    #[test]
    fn exhausted_single_text_lookahead_keeps_one_envelope_until_drain() {
        let first = key_event(VK_ESCAPE, b'\x1b' as u16, 0);
        let mut key_up = first;
        key_up.key_down = false;
        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(key_up)).take(63),
        );
        records.extend([
            console_key_input_record(b'b'),
            console_key_input_record(b'c'),
        ]);

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let first = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the exhausted lookahead opens a fail-closed paste");
        assert_eq!(batch_bytes(&first), BRACKETED_PASTE_START);
        assert!(reader.paste_open);

        let second = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("future records close the same paste");
        assert_eq!(batch_bytes(&second), b"\x1bbc\x1b[201~");
        let combined = first
            .iter()
            .chain(&second)
            .flat_map(|input| input.payload().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(combined, bracketed_text(b"\x1bbc"));
        assert!(!reader.paste_open);
    }

    #[test]
    fn synthesized_lf_only_lookahead_cannot_submit_before_third_batch() {
        let first = key_event(b'a' as u16, b'a' as u16, 0);
        let mut first_up = first;
        first_up.key_down = false;
        let lf = key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED);
        let mut lf_up = lf;
        lf_up.key_down = false;

        let mut records = vec![console_key_input_record_from_event(first)];
        records.extend(
            std::iter::repeat_with(|| console_key_input_record_from_event(first_up)).take(31),
        );
        records.push(console_key_input_record_from_event(lf));
        records
            .extend(std::iter::repeat_with(|| console_key_input_record_from_event(lf_up)).take(31));
        records.push(console_key_input_record(b'b'));

        let api = FakeConsoleInput::new(records);
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let first = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the LF-only lookahead stays inside an open paste");
        assert_eq!(batch_bytes(&first), b"\x1b[200~a\n");
        assert!(reader.paste_open);

        let second = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("the third batch closes the same paste");
        assert_eq!(batch_bytes(&second), b"b\x1b[201~");
        let combined = first
            .iter()
            .chain(&second)
            .flat_map(|input| input.payload().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(combined, bracketed_text(b"a\nb"));
        assert!(!reader.paste_open);
    }

    #[test]
    fn only_a_tmux_parent_enables_the_sgr_mouse_exception() {
        assert!(parent_allows_sgr_mouse_key_batches(
            ClientContextParent::Tmux
        ));
        assert!(!parent_allows_sgr_mouse_key_batches(
            ClientContextParent::Rmux
        ));
        assert!(!parent_allows_sgr_mouse_key_batches(
            ClientContextParent::None
        ));
    }

    #[test]
    fn drained_tmux_sgr_mouse_batches_are_forwarded_live() {
        for sequence in [
            b"\x1b[<0;10;5M".as_slice(),
            b"\x1b[<0;10;5M\x1b[<32;11;5M\x1b[<0;11;5m".as_slice(),
            b"\x1b[<255;65535;65535M".as_slice(),
        ] {
            let inputs = encode_drained_tmux_sgr_mouse_batch(
                &key_event_batch(sequence),
                true,
                false,
                &mut None,
            )
            .expect("strict, drained tmux SGR batch");
            assert_eq!(batch_bytes(&inputs), sequence);
        }
    }

    #[test]
    fn tmux_sgr_mouse_exception_rejects_every_ambiguous_shape() {
        let cases = [
            b"\x1b[200~\x1b[<0;10;5M\x1b[201~".as_slice(),
            b"text\x1b[<0;10;5M".as_slice(),
            b"\x1b[<0;10;5Mtext".as_slice(),
            b"\x1b[<0;10;5".as_slice(),
            b"\x1b[<0;10;5X".as_slice(),
            b"\x1b[<a;10;5M".as_slice(),
            b"\x1b[<;10;5M".as_slice(),
            b"\x1b[<0;;5M".as_slice(),
            b"\x1b[<0;10;M".as_slice(),
            b"\x1b[<256;10;5M".as_slice(),
            b"\x1b[<0;0;5M".as_slice(),
            b"\x1b[<0;10;0M".as_slice(),
            b"\x1b[<0;65536;5M".as_slice(),
            b"\x1b[<0;10;65536M".as_slice(),
        ];
        for bytes in cases {
            assert!(
                !is_complete_bounded_sgr_mouse_run(bytes),
                "ambiguous input must not become live: {bytes:?}"
            );
        }

        let valid = key_event_batch(b"\x1b[<0;10;5M");
        assert!(encode_drained_tmux_sgr_mouse_batch(&valid, false, false, &mut None).is_none());
        assert!(encode_drained_tmux_sgr_mouse_batch(&valid, true, true, &mut None).is_none());
        let mut pending_surrogate = Some(0xd83d);
        assert!(
            encode_drained_tmux_sgr_mouse_batch(&valid, true, false, &mut pending_surrogate,)
                .is_none()
        );
        assert_eq!(pending_surrogate, Some(0xd83d));

        let mut mixed = valid;
        mixed.push(BatchEvent::Mouse(mouse_event(0, 0, 0, 1, 1)));
        assert!(encode_drained_tmux_sgr_mouse_batch(&mixed, true, false, &mut None).is_none());
    }

    #[test]
    fn non_tmux_reader_keeps_sgr_key_batches_fail_closed() {
        let sequence = b"\x1b[<0;10;5M";
        let api = FakeConsoleInput::new(sequence.iter().copied().map(console_key_input_record));
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("fake console batch");
        assert_eq!(batch_bytes(&inputs), bracketed_text(sequence));
    }

    #[test]
    fn tmux_reader_forwards_only_the_strict_drained_sgr_batch() {
        let sequence = b"\x1b[<0;10;5M";
        let api = FakeConsoleInput::new(sequence.iter().copied().map(console_key_input_record));
        let coordinator = ConsoleIoCoordinator::new();
        let mut reader = fake_console_reader();
        reader.tmux_parent_sgr_mouse = true;
        let inputs = reader
            .read_key_inputs_with(&coordinator, &api)
            .expect("fake console batch");
        assert_eq!(batch_bytes(&inputs), sequence);
    }

    #[test]
    fn sgr_mouse_syntax_from_key_events_is_fail_closed_as_paste() {
        let sequence = b"\x1b[<0;10;5M";
        let inputs = encode_input_batch(
            &key_event_batch(sequence),
            true,
            &mut false,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(batch_bytes(&inputs), bracketed_text(sequence));
    }

    #[test]
    fn coalesced_sgr_key_event_frames_are_also_fail_closed_as_paste() {
        let sequence = b"\x1b[<0;10;5M\x1b[<32;11;5M\x1b[<0;11;5m";
        let inputs = encode_input_batch(
            &key_event_batch(sequence),
            true,
            &mut false,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(batch_bytes(&inputs), bracketed_text(sequence));
    }

    #[test]
    fn mixed_sgr_mouse_and_text_bursts_remain_one_faithful_paste() {
        let click = b"\x1b[<0;10;5M";
        let drag = b"\x1b[<32;11;5M";
        let mut cases = Vec::new();

        let mut text_then_sgr = b"ab".to_vec();
        text_then_sgr.extend_from_slice(click);
        cases.push((
            "text + SGR",
            text_then_sgr.clone(),
            bracketed_text(&text_then_sgr),
        ));

        let mut sgr_then_one = click.to_vec();
        sgr_then_one.push(b'x');
        cases.push((
            "SGR + one character",
            sgr_then_one.clone(),
            bracketed_text(&sgr_then_one),
        ));

        let mut sgr_then_paste = click.to_vec();
        sgr_then_paste.extend_from_slice(b"xy");
        cases.push((
            "SGR + multi-character paste",
            sgr_then_paste.clone(),
            bracketed_text(&sgr_then_paste),
        ));

        let mut adjacent_sgr_then_paste = click.to_vec();
        adjacent_sgr_then_paste.extend_from_slice(drag);
        adjacent_sgr_then_paste.extend_from_slice(b"yz");
        cases.push((
            "SGR + SGR + text",
            adjacent_sgr_then_paste.clone(),
            bracketed_text(&adjacent_sgr_then_paste),
        ));

        for (label, bytes, expected) in cases {
            let mut paste_open = false;
            let inputs = encode_input_batch(
                &key_event_batch(&bytes),
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut None,
                &mut 0,
            );

            assert_eq!(batch_bytes(&inputs), expected, "{label}");
            assert!(!paste_open, "{label} must leave no paste envelope open");
        }
    }

    #[test]
    fn native_mouse_record_inside_paste_does_not_make_text_live_or_stale_button_state() {
        let mut paste_open = false;
        let mut carryover = Vec::new();
        let mut surrogate = None;
        let mut last_mouse = FROM_LEFT_1ST_BUTTON_PRESSED;

        let first = encode_input_batch(
            &key_event_batch(b"ab"),
            false,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        assert!(paste_open);

        let mixed = [
            BatchEvent::Mouse(mouse_event(0, 0, 0, 3, 4)),
            BatchEvent::Key(key_event(b'c' as u16, b'c' as u16, 0)),
        ];
        let middle = encode_input_batch(
            &mixed,
            false,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        assert!(
            paste_open,
            "a coalesced mouse release must not close the active paste"
        );
        assert_eq!(last_mouse, 0, "the suppressed release still updates state");

        let final_part = encode_input_batch(
            &key_event_batch(b"d"),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let combined = first
            .iter()
            .chain(&middle)
            .chain(&final_part)
            .flat_map(|input| input.payload().to_vec())
            .collect::<Vec<_>>();
        assert_eq!(combined, bracketed_text(b"abcd"));
        assert!(!paste_open);

        let hover = encode_input_batch(
            &[BatchEvent::Mouse(mouse_event(0, MOUSE_MOVED, 0, 4, 5))],
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        assert_eq!(batch_bytes(&hover), b"\x1b[<35;5;6M");
    }

    #[test]
    fn sgr_key_event_paste_stays_fail_closed_after_a_multi_key_first_batch() {
        let sequence = b"\x1b[<32;123;45M";
        for split in 2..sequence.len() {
            let mut paste_open = false;
            let mut carryover = Vec::new();
            let mut surrogate = None;
            let mut last_mouse = 0;

            let first = encode_input_batch(
                &key_event_batch(&sequence[..split]),
                false,
                &mut paste_open,
                &mut carryover,
                &mut surrogate,
                &mut last_mouse,
            );
            let second = encode_input_batch(
                &key_event_batch(&sequence[split..]),
                true,
                &mut paste_open,
                &mut carryover,
                &mut surrogate,
                &mut last_mouse,
            );

            assert_eq!(
                first
                    .iter()
                    .chain(&second)
                    .flat_map(|input| input.payload().to_vec())
                    .collect::<Vec<_>>(),
                bracketed_text(sequence),
                "split at byte {split}"
            );
            assert!(!paste_open, "the drained paste must close its envelope");
        }
    }

    #[test]
    fn drained_partial_sgr_prefix_and_later_text_never_become_a_live_click() {
        let prefix = b"\x1b[<0;";
        let continuation = b"10;5M";
        let mut paste_open = false;
        let mut carryover = Vec::new();
        let mut surrogate = None;
        let mut last_mouse = 0;

        let first = encode_input_batch(
            &key_event_batch(prefix),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let second = encode_input_batch(
            &key_event_batch(continuation),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );

        assert_eq!(batch_bytes(&first), bracketed_text(prefix));
        assert_eq!(batch_bytes(&second), bracketed_text(continuation));
        assert!(!paste_open);
    }

    #[test]
    fn text_before_a_split_sgr_prefix_keeps_the_whole_burst_in_paste() {
        let first_bytes = b"ab\x1b[<0;";
        let second_bytes = b"10;5M";
        let mut paste_open = false;
        let mut carryover = Vec::new();
        let mut surrogate = None;
        let mut last_mouse = 0;

        let first = encode_input_batch(
            &key_event_batch(first_bytes),
            false,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let second = encode_input_batch(
            &key_event_batch(second_bytes),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let expected = bracketed_text(b"ab\x1b[<0;10;5M");

        assert_eq!(batch_bytes(&first), b"\x1b[200~ab\x1b[<0;");
        assert_eq!(
            first
                .iter()
                .chain(&second)
                .flat_map(|input| input.payload().to_vec())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(!paste_open);
    }

    #[test]
    fn undrained_pure_sgr_key_events_remain_one_paste() {
        let first_bytes = b"\x1b[<0;10;5M";
        let second_bytes = b"\x1b[<32;11;5M\x1b[<0;11;5m";
        let mut paste_open = false;
        let mut carryover = Vec::new();
        let mut surrogate = None;
        let mut last_mouse = 0;

        let first = encode_input_batch(
            &key_event_batch(first_bytes),
            false,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        assert!(paste_open, "the undrained key burst opens paste state");

        let second = encode_input_batch(
            &key_event_batch(second_bytes),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let source = [first_bytes.as_slice(), second_bytes.as_slice()].concat();

        assert_eq!(
            first
                .iter()
                .chain(&second)
                .flat_map(|input| input.payload().to_vec())
                .collect::<Vec<_>>(),
            bracketed_text(&source)
        );
        assert!(!paste_open);
    }

    #[test]
    fn undrained_sgr_followed_by_text_falls_back_to_one_paste() {
        let first_bytes = b"\x1b[<0;10;5M";
        let second_bytes = b"ordinary text";
        let mut paste_open = false;
        let mut carryover = Vec::new();
        let mut surrogate = None;
        let mut last_mouse = 0;

        let first = encode_input_batch(
            &key_event_batch(first_bytes),
            false,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        assert!(paste_open, "the undrained key burst opens paste state");
        let second = encode_input_batch(
            &key_event_batch(second_bytes),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let source = [first_bytes.as_slice(), second_bytes.as_slice()].concat();

        assert_eq!(
            first
                .iter()
                .chain(&second)
                .flat_map(|input| input.payload().to_vec())
                .collect::<Vec<_>>(),
            bracketed_text(&source)
        );
        assert!(!paste_open);
    }

    #[test]
    fn invalid_sgr_lookalike_remains_a_detected_paste() {
        let text = b"\x1b[<not-a-mouse-frame";
        let inputs = encode_input_batch(
            &key_event_batch(text),
            true,
            &mut false,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );
        let mut expected = BRACKETED_PASTE_START.to_vec();
        expected.extend_from_slice(text);
        expected.extend_from_slice(BRACKETED_PASTE_END);

        assert_eq!(batch_bytes(&inputs), expected);
    }

    #[test]
    fn split_invalid_sgr_lookalike_falls_back_to_one_paste_envelope() {
        let first_text = b"\x1b[<12;";
        let second_text = b"x;9M ordinary text";
        let mut paste_open = false;
        let mut carryover = Vec::new();
        let mut surrogate = None;
        let mut last_mouse = 0;

        let first = encode_input_batch(
            &key_event_batch(first_text),
            false,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let second = encode_input_batch(
            &key_event_batch(second_text),
            true,
            &mut paste_open,
            &mut carryover,
            &mut surrogate,
            &mut last_mouse,
        );
        let mut expected = BRACKETED_PASTE_START.to_vec();
        expected.extend_from_slice(first_text);
        expected.extend_from_slice(second_text);
        expected.extend_from_slice(BRACKETED_PASTE_END);

        assert_eq!(
            first
                .iter()
                .chain(&second)
                .flat_map(|input| input.payload().to_vec())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(!paste_open);
    }

    #[test]
    fn multi_character_paste_burst_is_wrapped_in_bracketed_markers() {
        // The record pattern conhost delivers for pasting "ab\r\ncd" (probed
        // 2026-07-11): each character is a plain key-down carrying its real
        // virtual-key code and the newline collapses to a single VK_RETURN,
        // indistinguishable from typed input except that the whole run arrives
        // in one ReadConsoleInputW batch.
        let events = [
            BatchEvent::Key(key_event('a' as u16, 'a' as u16, 0)),
            BatchEvent::Key(key_event('b' as u16, 'b' as u16, 0)),
            BatchEvent::Key(key_event(VK_RETURN, 0x0d, 0)),
            BatchEvent::Key(key_event('c' as u16, 'c' as u16, 0)),
            BatchEvent::Key(key_event('d' as u16, 'd' as u16, 0)),
        ];
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &events,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );
        let mut expected = Vec::new();
        expected.extend_from_slice(BRACKETED_PASTE_START);
        expected.extend_from_slice(b"ab\rcd");
        expected.extend_from_slice(BRACKETED_PASTE_END);
        assert_eq!(batch_bytes(&inputs), expected);
        assert!(!paste_open, "a drained burst must close its bracket");
    }

    #[test]
    fn supported_synthesized_lf_shapes_stay_inside_a_paste_burst() {
        let cases: &[(&[BatchEvent], &[u8])] = &[
            (
                &[
                    BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                    BatchEvent::Key(key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED)),
                    BatchEvent::Key(key_event(b'b' as u16, b'b' as u16, 0)),
                ],
                b"a\nb",
            ),
            (
                &[
                    BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                    BatchEvent::Key(key_event(VK_RETURN, b'\r' as u16, 0)),
                    BatchEvent::Key(key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED)),
                    BatchEvent::Key(key_event(b'b' as u16, b'b' as u16, 0)),
                ],
                b"a\r\nb",
            ),
            (
                &[
                    BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                    BatchEvent::Key(conpty_lf_event(LEFT_CTRL_PRESSED)),
                    BatchEvent::Key(key_event(b'b' as u16, b'b' as u16, 0)),
                ],
                b"a\nb",
            ),
            (
                &[
                    BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                    BatchEvent::Key(key_event(VK_RETURN, b'\r' as u16, 0)),
                    BatchEvent::Key(conpty_lf_event(LEFT_CTRL_PRESSED)),
                    BatchEvent::Key(key_event(b'b' as u16, b'b' as u16, 0)),
                ],
                b"a\r\nb",
            ),
        ];

        for (events, body) in cases {
            let mut paste_open = false;
            let inputs = encode_input_batch(
                events,
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut None,
                &mut 0,
            );
            assert_eq!(batch_bytes(&inputs), bracketed_text(body));
            assert!(!paste_open);
        }
    }

    #[test]
    fn synthesized_lf_singletons_remain_live_outside_a_detected_paste() {
        let cases = [
            key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED),
            conpty_lf_event(LEFT_CTRL_PRESSED),
            key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED | SHIFT_PRESSED),
            conpty_lf_event(LEFT_CTRL_PRESSED | SHIFT_PRESSED),
            key_event(
                b'J' as u16,
                b'\n' as u16,
                LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED,
            ),
            conpty_lf_event(LEFT_CTRL_PRESSED | LEFT_ALT_PRESSED),
        ];

        for event in cases {
            let mut paste_open = false;
            let inputs = encode_input_batch(
                &[BatchEvent::Key(event)],
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut None,
                &mut 0,
            );
            assert!(!batch_bytes(&inputs).starts_with(BRACKETED_PASTE_START));
            assert!(!paste_open);
        }
    }

    #[test]
    fn unrecognized_ctrl_lf_virtual_key_does_not_enter_paste() {
        let events = [
            BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
            BatchEvent::Key(key_event(b'K' as u16, b'\n' as u16, LEFT_CTRL_PRESSED)),
            BatchEvent::Key(key_event(b'b' as u16, b'b' as u16, 0)),
        ];
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &events,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert!(!batch_bytes(&inputs).starts_with(BRACKETED_PASTE_START));
        assert!(!paste_open);
    }

    #[test]
    fn supported_synthesized_lf_shapes_supply_fresh_paste_evidence_in_a_burst() {
        for virtual_key in [u16::from(b'J'), VK_RETURN] {
            let lf = || BatchEvent::Key(key_event(virtual_key, b'\n' as u16, LEFT_CTRL_PRESSED));
            let cases = [
                (
                    [
                        BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                        lf(),
                    ],
                    b"a\n".as_slice(),
                ),
                (
                    [
                        lf(),
                        BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                    ],
                    b"\na".as_slice(),
                ),
                ([lf(), lf()], b"\n\n".as_slice()),
            ];

            for (events, expected) in cases {
                let mut paste_open = false;
                let inputs = encode_input_batch(
                    &events,
                    true,
                    &mut paste_open,
                    &mut Vec::new(),
                    &mut None,
                    &mut 0,
                );
                assert_eq!(batch_bytes(&inputs), bracketed_text(expected));
                assert!(!paste_open);
            }
        }
    }

    #[test]
    fn a_qualified_paste_lf_preserves_a_pending_high_surrogate() {
        // U+1F600 as UTF-16: the console delivers the halves as two records,
        // and a clipboard newline between them is unrelated to that pair.
        let high = key_event(0, 0xd83d, 0);
        let low = key_event(0, 0xde00, 0);
        let mut expected = b"\n".to_vec();
        expected.extend_from_slice("\u{1f600}".as_bytes());

        for lf in [
            key_event(b'J' as u16, b'\n' as u16, LEFT_CTRL_PRESSED),
            conpty_lf_event(LEFT_CTRL_PRESSED),
        ] {
            let mut paste_open = false;
            let mut pending = None;
            let inputs = encode_input_batch(
                &[
                    BatchEvent::Key(high),
                    BatchEvent::Key(lf),
                    BatchEvent::Key(low),
                ],
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut pending,
                &mut 0,
            );

            assert_eq!(
                batch_bytes(&inputs),
                bracketed_text(&expected),
                "the pair split by a paste LF in one batch"
            );
            assert_eq!(pending, None, "the low half consumed the pending high one");
            assert!(!paste_open);

            let mut paste_open = false;
            let mut pending = Some(0xd83d);
            let inputs = encode_input_batch(
                &[BatchEvent::Key(lf), BatchEvent::Key(low)],
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut pending,
                &mut 0,
            );

            assert_eq!(
                batch_bytes(&inputs),
                bracketed_text(&expected),
                "the high half was already pending from an earlier batch"
            );
            assert_eq!(pending, None);
            assert!(!paste_open);
        }
    }

    #[test]
    fn short_tab_and_backspace_bursts_are_bracketed() {
        let text = || BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0));
        let tab = || BatchEvent::Key(key_event(VK_TAB, b'\t' as u16, 0));
        let backspace = || BatchEvent::Key(key_event(VK_BACK, b'\x08' as u16, 0));
        let cases = [
            ([text(), tab()], b"a\t".as_slice()),
            ([tab(), text()], b"\ta".as_slice()),
            ([tab(), tab()], b"\t\t".as_slice()),
            ([text(), backspace()], b"a\x7f".as_slice()),
            ([backspace(), text()], b"\x7fa".as_slice()),
            ([backspace(), backspace()], b"\x7f\x7f".as_slice()),
        ];

        for (events, expected) in cases {
            let mut paste_open = false;
            let inputs = encode_input_batch(
                &events,
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut None,
                &mut 0,
            );

            assert_eq!(batch_bytes(&inputs), bracketed_text(expected));
            assert!(!paste_open);
        }
    }

    #[test]
    fn tab_and_backspace_singletons_remain_live() {
        for (event, expected) in [
            (key_event(VK_TAB, b'\t' as u16, 0), b"\t".as_slice()),
            (key_event(VK_BACK, b'\x08' as u16, 0), b"\x7f".as_slice()),
        ] {
            let mut paste_open = false;
            let inputs = encode_input_batch(
                &[BatchEvent::Key(event)],
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut None,
                &mut 0,
            );

            assert_eq!(batch_bytes(&inputs), expected);
            assert!(!paste_open);
        }
    }

    #[test]
    fn short_carriage_return_pastes_cannot_submit_live() {
        let cases = [
            vec![
                BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                BatchEvent::Key(key_event(VK_RETURN, b'\r' as u16, 0)),
            ],
            vec![
                BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
                BatchEvent::Key(key_event(VK_RETURN, b'\r' as u16, 0)),
                BatchEvent::Key(conpty_lf_event(LEFT_CTRL_PRESSED)),
            ],
        ];

        for (events, body) in cases.into_iter().zip([b"a\r".as_slice(), b"a\r\n"]) {
            let mut paste_open = false;
            let inputs = encode_input_batch(
                &events,
                true,
                &mut paste_open,
                &mut Vec::new(),
                &mut None,
                &mut 0,
            );

            assert_eq!(batch_bytes(&inputs), bracketed_text(body));
            assert!(!paste_open);
        }
    }

    #[test]
    fn conpty_ctrl_enter_unicode_lf_stays_inside_a_paste_burst() {
        let events = [
            BatchEvent::Key(key_event(b'a' as u16, b'a' as u16, 0)),
            BatchEvent::Key(conpty_lf_event(LEFT_CTRL_PRESSED)),
            BatchEvent::Key(key_event(b'b' as u16, b'b' as u16, 0)),
        ];
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &events,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(batch_bytes(&inputs), bracketed_text(b"a\nb"));
        assert!(!paste_open);
    }

    #[test]
    fn conpty_ctrl_enter_lf_singleton_remains_a_live_console_key() {
        let event = conpty_lf_event(LEFT_CTRL_PRESSED);
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &[BatchEvent::Key(event)],
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(inputs.len(), 1);
        assert!(!inputs[0].payload().starts_with(BRACKETED_PASTE_START));
        let key = inputs[0]
            .windows_console_key()
            .expect("interactive Ctrl+Enter retains its Windows identity");
        assert_eq!(key.virtual_key_code(), VK_RETURN);
        assert_eq!(key.virtual_scan_code(), 0x1c);
        assert_eq!(key.unicode_char(), b'\n' as u16);
        assert_eq!(key.control_key_state(), LEFT_CTRL_PRESSED);
        assert!(!paste_open);
    }

    #[test]
    fn conpty_ctrl_enter_lf_keeps_an_existing_paste_envelope_open() {
        let mut paste_open = true;
        let mut carryover = Vec::new();
        let inputs = encode_input_batch(
            &[BatchEvent::Key(conpty_lf_event(LEFT_CTRL_PRESSED))],
            false,
            &mut paste_open,
            &mut carryover,
            &mut None,
            &mut 0,
        );

        assert_eq!(batch_bytes(&inputs), b"\n");
        assert!(paste_open);
    }

    #[test]
    fn single_typed_character_is_not_bracketed() {
        // One character is indistinguishable from a keystroke, so it must reach
        // the pane verbatim with no paste markers.
        let events = [BatchEvent::Key(key_event('x' as u16, 'x' as u16, 0))];
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &events,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );
        assert_eq!(batch_bytes(&inputs), b"x");
        assert!(!paste_open);
    }

    #[test]
    fn undrained_single_character_and_mouse_remain_live() {
        // `drained` describes every queued console record, including key-up,
        // focus and mouse records. It cannot turn one otherwise ordinary key
        // into evidence of a clipboard burst or discard a coalesced click.
        let events = [
            BatchEvent::Key(key_event('x' as u16, 'x' as u16, 0)),
            BatchEvent::Mouse(mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 3, 4)),
        ];
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &events,
            false,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(batch_bytes(&inputs), b"x\x1b[<0;4;5M");
        assert!(!paste_open);
    }

    #[test]
    fn paste_burst_spanning_batches_keeps_the_bracket_open_until_drained() {
        let mut paste_open = false;
        let first = [
            BatchEvent::Key(key_event('a' as u16, 'a' as u16, 0)),
            BatchEvent::Key(key_event('b' as u16, 'b' as u16, 0)),
        ];
        let inputs = encode_input_batch(
            &first,
            false,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );
        let mut expected = Vec::new();
        expected.extend_from_slice(BRACKETED_PASTE_START);
        expected.extend_from_slice(b"ab");
        assert_eq!(batch_bytes(&inputs), expected);
        assert!(paste_open, "an undrained burst keeps the bracket open");

        // A continuation batch (even a single trailing character), now drained.
        let second = [BatchEvent::Key(key_event('c' as u16, 'c' as u16, 0))];
        let inputs = encode_input_batch(
            &second,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );
        let mut expected = Vec::new();
        expected.extend_from_slice(b"c");
        expected.extend_from_slice(BRACKETED_PASTE_END);
        assert_eq!(batch_bytes(&inputs), expected);
        assert!(!paste_open);
    }

    #[test]
    fn embedded_paste_markers_are_stripped_before_wrapping() {
        // Crafted clipboard content carrying a raw ESC[201~ (or ESC[200~) must
        // not break out of / nest the paste envelope.
        let mut text = Vec::new();
        text.extend_from_slice(b"X");
        text.extend_from_slice(BRACKETED_PASTE_END);
        text.extend_from_slice(b"Y");
        text.extend_from_slice(BRACKETED_PASTE_START);
        text.extend_from_slice(b"Z");
        strip_embedded_paste_markers(&mut text);
        assert_eq!(text, b"XYZ");
    }

    #[test]
    fn fragmented_end_marker_is_stripped_to_a_fixed_point() {
        // A hostile clipboard mixes a marker-shaped prefix, a full end marker,
        // then bytes that would rejoin with the prefix. A single-pass strip
        // removes only the middle marker and leaves the rejoined ESC[201~
        // alive — the fix-point loop must fully collapse it.
        let mut text = Vec::new();
        text.extend_from_slice(b"\x1b[20");
        text.extend_from_slice(BRACKETED_PASTE_END);
        text.extend_from_slice(b"1~body");
        strip_embedded_paste_markers(&mut text);
        assert_eq!(text, b"body");
    }

    #[test]
    fn straddling_end_marker_across_batches_is_neutralized_by_carryover() {
        // A hostile ESC[201~ split across two batches must not slip through
        // per-batch stripping: the second batch's continuation carries the
        // first batch's tail forward and re-strips the reassembled bytes.
        let mut paste_open = false;
        let mut carryover: Vec<u8> = Vec::new();
        let first = [
            BatchEvent::Key(key_event('a' as u16, 'a' as u16, 0)),
            BatchEvent::Key(key_event('b' as u16, 'b' as u16, 0)),
            BatchEvent::Key(key_event(0x1b, 0x1b, 0)),
            BatchEvent::Key(key_event('[' as u16, b'[' as u16, 0)),
            BatchEvent::Key(key_event('2' as u16, b'2' as u16, 0)),
        ];
        let inputs = encode_input_batch(
            &first,
            false,
            &mut paste_open,
            &mut carryover,
            &mut None,
            &mut 0,
        );
        let first_bytes = batch_bytes(&inputs);
        assert!(
            paste_open,
            "the burst must still be open — the mid-marker suffix should be held back"
        );
        assert!(
            first_bytes.starts_with(BRACKETED_PASTE_START),
            "opening marker should be present: {first_bytes:?}"
        );
        // The opening BRACKETED_PASTE_START itself begins with `\x1b[2`, so
        // only the emitted BODY (after the opening marker) is checked: the
        // held-back marker suffix must not have escaped into it.
        let first_body = &first_bytes[BRACKETED_PASTE_START.len()..];
        assert!(
            !first_body.windows(3).any(|w| w == b"\x1b[2"),
            "no marker prefix should escape after the opening marker: {first_bytes:?}"
        );

        // Second batch supplies the reassembled 01~ + payload while drained.
        let second = [
            BatchEvent::Key(key_event('0' as u16, b'0' as u16, 0)),
            BatchEvent::Key(key_event('1' as u16, b'1' as u16, 0)),
            BatchEvent::Key(key_event('~' as u16, b'~' as u16, 0)),
            BatchEvent::Key(key_event('e' as u16, b'e' as u16, 0)),
            BatchEvent::Key(key_event('v' as u16, b'v' as u16, 0)),
            BatchEvent::Key(key_event('i' as u16, b'i' as u16, 0)),
            BatchEvent::Key(key_event('l' as u16, b'l' as u16, 0)),
        ];
        let inputs = encode_input_batch(
            &second,
            true,
            &mut paste_open,
            &mut carryover,
            &mut None,
            &mut 0,
        );
        let combined: Vec<u8> = first_bytes
            .iter()
            .chain(batch_bytes(&inputs).iter())
            .copied()
            .collect();
        assert!(!paste_open, "drained continuation must close the bracket");
        // The reassembled bytes must not include a live end marker anywhere
        // between the opening and the true closing marker.
        let start = combined
            .windows(BRACKETED_PASTE_START.len())
            .position(|w| w == BRACKETED_PASTE_START)
            .expect("opening marker present");
        let last_end = combined
            .windows(BRACKETED_PASTE_END.len())
            .rposition(|w| w == BRACKETED_PASTE_END)
            .expect("closing marker present");
        let body = &combined[start + BRACKETED_PASTE_START.len()..last_end];
        assert!(
            !body
                .windows(BRACKETED_PASTE_END.len())
                .any(|w| w == BRACKETED_PASTE_END),
            "reassembled end marker escaped the strip: body={body:?}"
        );
        assert!(carryover.is_empty(), "carryover should be flushed on drain");
    }

    #[test]
    fn a_control_chord_in_the_batch_is_not_treated_as_paste() {
        let events = [
            BatchEvent::Key(key_event('a' as u16, 'a' as u16, 0)),
            BatchEvent::Key(key_event('C' as u16, 0x03, LEFT_CTRL_PRESSED)),
        ];
        let mut paste_open = false;
        let inputs = encode_input_batch(
            &events,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );
        let bytes = batch_bytes(&inputs);
        assert!(
            !bytes.starts_with(BRACKETED_PASTE_START),
            "a batch containing a control chord must not be bracketed: {bytes:?}"
        );
        assert!(!paste_open);
    }

    #[test]
    fn console_key_events_encode_ctrl_letters_as_control_bytes() {
        for (letter, expected) in [('a', 0x01), ('c', 0x03), ('l', 0x0c), ('z', 0x1a)] {
            let event = key_event(letter as u16, letter as u16, LEFT_CTRL_PRESSED);
            assert_eq!(
                encode(&event),
                vec![expected],
                "Ctrl+{letter} should preserve the control byte"
            );
        }
    }

    #[test]
    fn console_key_events_preserve_existing_control_chars() {
        let event = key_event('l' as u16, 0x0c, LEFT_CTRL_PRESSED);

        assert_eq!(encode(&event), vec![0x0c]);
    }

    #[test]
    fn console_key_events_preserve_ctrl_d_windows_metadata() {
        let event = key_event('D' as u16, 0x04, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        let key = windows_console_key_for_event(event, &bytes)
            .expect("Ctrl-D should preserve Windows console metadata");

        assert_eq!(bytes, vec![0x04]);
        assert_eq!(key.virtual_key_code(), 'D' as u16);
        assert_eq!(key.virtual_scan_code(), 0x20);
        assert_eq!(key.unicode_char(), 0x04);
        assert_eq!(key.control_key_state(), LEFT_CTRL_PRESSED);
        assert_eq!(key.repeat_count(), 1);
    }

    #[test]
    fn console_key_events_preserve_other_ctrl_letter_windows_metadata() {
        let event = key_event('P' as u16, 0x10, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        let key = windows_console_key_for_event(event, &bytes)
            .expect("Ctrl-P should preserve Windows console metadata");

        assert_eq!(bytes, vec![0x10]);
        assert_eq!(key.virtual_key_code(), 'P' as u16);
        assert_eq!(key.unicode_char(), 0x10);
    }

    #[test]
    fn console_key_events_do_not_invent_metadata_unicode_char() {
        let event = key_event('D' as u16, 0, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        let key = windows_console_key_for_event(event, &bytes)
            .expect("virtual Ctrl-D should still preserve Windows console metadata");

        assert_eq!(bytes, vec![0x04]);
        assert_eq!(key.virtual_key_code(), 'D' as u16);
        assert_eq!(key.unicode_char(), 0);
    }

    #[test]
    fn console_key_events_preserve_ctrl_c_windows_metadata() {
        let event = key_event('C' as u16, 0x03, LEFT_CTRL_PRESSED);
        let bytes = encode(&event);

        assert_eq!(bytes, vec![0x03]);
        assert!(windows_console_key_for_event(event, &bytes).is_some());
    }

    #[test]
    fn console_key_events_encode_ctrl_virtual_letters_without_unicode_char() {
        for (letter, expected) in [('A', 0x01), ('L', 0x0c), ('Z', 0x1a)] {
            let event = key_event(letter as u16, 0, LEFT_CTRL_PRESSED);
            assert_eq!(
                encode(&event),
                vec![expected],
                "virtual Ctrl+{letter} should preserve the control byte"
            );
        }
    }

    #[test]
    fn console_key_events_encode_ctrl_space_and_alt_ctrl_letters() {
        let ctrl_space = key_event(VK_SPACE, 0, LEFT_CTRL_PRESSED);
        assert_eq!(encode(&ctrl_space), vec![0x00]);

        let alt_ctrl_l = key_event('l' as u16, 'l' as u16, LEFT_ALT_PRESSED | LEFT_CTRL_PRESSED);
        assert_eq!(encode(&alt_ctrl_l), b"\x1b\x0c");
    }

    #[test]
    fn console_key_events_do_not_treat_alt_gr_text_as_ctrl_meta() {
        let event = key_event('e' as u16, 0x20ac, RIGHT_ALT_PRESSED | LEFT_CTRL_PRESSED);

        assert_eq!(encode(&event), "€".as_bytes());
    }

    #[test]
    fn console_key_events_encode_text_and_meta_text() {
        let plain = key_event('x' as u16, 'x' as u16, 0);
        assert_eq!(encode(&plain), b"x");

        let meta = key_event('x' as u16, 'x' as u16, LEFT_ALT_PRESSED);
        assert_eq!(encode(&meta), b"\x1bx");
    }

    #[test]
    fn console_key_events_encode_navigation_with_modifiers() {
        assert_eq!(encode(&key_event(VK_UP, 0, 0)), b"\x1b[A");
        assert_eq!(
            encode(&key_event(VK_LEFT, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[1;5D"
        );
        assert_eq!(
            encode(&key_event(VK_RIGHT, 0, SHIFT_PRESSED | LEFT_CTRL_PRESSED)),
            b"\x1b[1;6C"
        );
        assert_eq!(
            encode(&key_event(VK_HOME, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[1;5H"
        );
        assert_eq!(
            encode(&key_event(VK_END, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[1;5F"
        );
        assert_eq!(
            encode(&key_event(VK_DELETE, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[3;5~"
        );
        assert_eq!(
            encode(&key_event(VK_F5, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[15;5~"
        );
        assert_eq!(encode(&key_event(VK_DOWN, 0, 0)), b"\x1b[B");
    }

    #[test]
    fn console_key_events_encode_enter_tab_escape_and_backspace() {
        assert_eq!(encode(&key_event(VK_RETURN, 0, 0)), b"\r");
        assert_eq!(
            encode(&key_event(VK_RETURN, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[13;5u"
        );
        assert_eq!(encode(&key_event(VK_TAB, 0, SHIFT_PRESSED)), b"\x1b[Z");
        assert_eq!(
            encode(&key_event(VK_TAB, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[9;5u"
        );
        assert_eq!(encode(&key_event(VK_ESCAPE, 0, 0)), b"\x1b");
        assert_eq!(
            encode(&key_event(VK_ESCAPE, 0, LEFT_ALT_PRESSED)),
            b"\x1b\x1b"
        );
        assert_eq!(encode(&key_event(VK_BACK, 0, 0)), b"\x7f");
        assert_eq!(
            encode(&key_event(VK_BACK, 0, LEFT_CTRL_PRESSED)),
            b"\x1b[127;5u"
        );
    }

    #[test]
    fn console_key_events_map_windows_control_unicode_through_virtual_keys() {
        assert_eq!(encode(&key_event(VK_BACK, 0x08, 0)), b"\x7f");
        assert_eq!(
            encode(&key_event(VK_BACK, 0x08, LEFT_CTRL_PRESSED)),
            b"\x1b[127;5u"
        );
        assert_eq!(encode(&key_event(VK_TAB, 0x09, SHIFT_PRESSED)), b"\x1b[Z");
        assert_eq!(
            encode(&key_event(VK_TAB, 0x09, LEFT_CTRL_PRESSED)),
            b"\x1b[9;5u"
        );
        assert_eq!(
            encode(&key_event(VK_RETURN, 0x0d, LEFT_CTRL_PRESSED)),
            b"\x1b[13;5u"
        );
        assert_eq!(
            encode(&key_event(VK_ESCAPE, 0x1b, LEFT_ALT_PRESSED)),
            b"\x1b\x1b"
        );
    }

    #[test]
    fn console_key_events_repeat_encoded_bytes() {
        let mut event = key_event('x' as u16, 'x' as u16, 0);
        event.repeat_count = 3;

        assert_eq!(encode(&event), b"xxx");
    }

    #[test]
    fn console_key_event_preserves_maximum_u16_repeat_count() {
        let mut event = key_event('x' as u16, 'x' as u16, 0);
        event.repeat_count = u16::MAX;

        assert_eq!(encode(&event).len(), usize::from(u16::MAX));
    }

    #[test]
    fn repeated_control_chord_is_retained_as_one_counted_logical_input() {
        let mut event = key_event(0xba, b';' as u16, LEFT_CTRL_PRESSED);
        event.repeat_count = 3;
        let events = [BatchEvent::Key(event)];
        let mut paste_open = false;

        let inputs = encode_input_batch(
            &events,
            true,
            &mut paste_open,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(inputs.len(), 1);
        let input = &inputs[0];
        assert_eq!(input.payload(), b";");
        assert_eq!(input.repeat_count(), 3);
        let key = input
            .windows_console_key()
            .expect("Ctrl+; must retain Windows identity");
        assert_eq!(key.virtual_key_code(), 0xba);
        assert_eq!(key.unicode_char(), b';' as u16);
        assert_eq!(key.control_key_state(), LEFT_CTRL_PRESSED);
        assert_eq!(key.repeat_count(), 3);
    }

    #[test]
    fn maximum_control_repeat_is_represented_without_per_repeat_inputs() {
        let mut event = key_event(0xba, b';' as u16, LEFT_CTRL_PRESSED);
        event.repeat_count = u16::MAX;
        let events = [BatchEvent::Key(event)];

        let inputs = encode_input_batch(
            &events,
            true,
            &mut false,
            &mut Vec::new(),
            &mut None,
            &mut 0,
        );

        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].payload(), b";");
        assert_eq!(inputs[0].repeat_count(), u16::MAX);
        assert_eq!(
            inputs[0]
                .windows_console_key()
                .expect("Ctrl+; metadata remains present")
                .repeat_count(),
            u16::MAX
        );
    }

    #[test]
    fn console_key_events_ignore_key_up() {
        let mut event = key_event('x' as u16, 'x' as u16, 0);
        event.key_down = false;

        assert!(encode(&event).is_empty());
    }

    fn encode(event: &ConsoleKeyEvent) -> Vec<u8> {
        encode_key_event(*event, &mut None)
    }

    fn key_event(
        virtual_key_code: u16,
        unicode_char: u16,
        control_key_state: u32,
    ) -> ConsoleKeyEvent {
        ConsoleKeyEvent {
            key_down: true,
            repeat_count: 1,
            virtual_key_code,
            virtual_scan_code: 0x20,
            unicode_char,
            control_key_state,
        }
    }

    fn conpty_lf_event(control_key_state: u32) -> ConsoleKeyEvent {
        let mut event = key_event(VK_RETURN, b'\n' as u16, control_key_state);
        event.virtual_scan_code = 0x1c;
        event
    }

    fn mouse_event(
        button_state: u32,
        event_flags: u32,
        control_key_state: u32,
        x: i16,
        y: i16,
    ) -> MOUSE_EVENT_RECORD {
        MOUSE_EVENT_RECORD {
            dwMousePosition: windows_sys::Win32::System::Console::COORD { X: x, Y: y },
            dwButtonState: button_state,
            dwControlKeyState: control_key_state,
            dwEventFlags: event_flags,
        }
    }

    fn encode_mouse(event: MOUSE_EVENT_RECORD, last: &mut u32) -> Option<String> {
        let bytes = encode_mouse_event(event, last).concat();
        (!bytes.is_empty()).then(|| String::from_utf8(bytes).unwrap())
    }

    #[test]
    fn mouse_wheel_up_encodes_sgr_button_64() {
        // High word of dwButtonState carries the signed wheel delta; positive = up.
        let event = mouse_event(0x0078_0000, MOUSE_WHEELED, 0, 9, 14);
        let mut last = 0;
        // Coordinates are 0-based from the console and become 1-based in SGR.
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<64;10;15M")
        );
    }

    #[test]
    fn mouse_wheel_down_encodes_sgr_button_65() {
        let event = mouse_event(0xff88_0000, MOUSE_WHEELED, 0, 0, 0);
        let mut last = 0;
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<65;1;1M")
        );
    }

    #[test]
    fn mouse_left_press_then_release_encodes_press_and_sgr_release() {
        let mut last = 0;
        let press = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 4, 4);
        assert_eq!(
            encode_mouse(press, &mut last).as_deref(),
            Some("\x1b[<0;5;5M")
        );
        // SGR release uses the released button identity plus the 'm' terminator.
        let release = mouse_event(0, 0, 0, 4, 4);
        assert_eq!(
            encode_mouse(release, &mut last).as_deref(),
            Some("\x1b[<0;5;5m")
        );
    }

    #[test]
    fn mouse_middle_release_preserves_sgr_button_identity() {
        let mut last = 0;
        let press = mouse_event(FROM_LEFT_2ND_BUTTON_PRESSED, 0, 0, 8, 3);
        assert_eq!(
            encode_mouse(press, &mut last).as_deref(),
            Some("\x1b[<1;9;4M")
        );

        let release = mouse_event(0, 0, SHIFT_PRESSED, 8, 3);
        assert_eq!(
            encode_mouse(release, &mut last).as_deref(),
            Some("\x1b[<5;9;4m")
        );
    }

    #[test]
    fn mouse_left_drag_sets_drag_flag() {
        let mut last = 0;
        let _ = encode_mouse(
            mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 1, 1),
            &mut last,
        );
        // Motion while the left button stays held -> drag (button 0 | 32).
        let drag = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, MOUSE_MOVED, 0, 2, 1);
        assert_eq!(
            encode_mouse(drag, &mut last).as_deref(),
            Some("\x1b[<32;3;2M")
        );
    }

    #[test]
    fn mouse_right_press_encodes_button_2() {
        let mut last = 0;
        let press = mouse_event(RIGHTMOST_BUTTON_PRESSED, 0, 0, 0, 0);
        assert_eq!(
            encode_mouse(press, &mut last).as_deref(),
            Some("\x1b[<2;1;1M")
        );
    }

    #[test]
    fn mouse_second_button_press_uses_changed_button_not_aggregate_state() {
        let mut last = 0;
        let left = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 0, 0);
        assert_eq!(
            encode_mouse(left, &mut last).as_deref(),
            Some("\x1b[<0;1;1M")
        );

        let left_and_right = mouse_event(
            FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED,
            0,
            0,
            1,
            0,
        );
        assert_eq!(
            encode_mouse(left_and_right, &mut last).as_deref(),
            Some("\x1b[<2;2;1M")
        );
    }

    #[test]
    fn mouse_partial_release_uses_released_button_not_remaining_button() {
        let mut last = 0;
        let both = mouse_event(
            FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED,
            0,
            0,
            1,
            1,
        );
        let _ = encode_mouse(both, &mut last);

        let left_remaining = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 1, 1);
        assert_eq!(
            encode_mouse(left_remaining, &mut last).as_deref(),
            Some("\x1b[<2;2;2m")
        );
    }

    #[test]
    fn mouse_coalesced_release_and_press_emits_both_transitions() {
        let mut last = RIGHTMOST_BUTTON_PRESSED;
        let swapped = mouse_event(FROM_LEFT_1ST_BUTTON_PRESSED, 0, 0, 3, 4);

        assert_eq!(
            encode_mouse(swapped, &mut last).as_deref(),
            Some("\x1b[<2;4;5m\x1b[<0;4;5M")
        );
        assert_eq!(last, FROM_LEFT_1ST_BUTTON_PRESSED);
    }

    #[test]
    fn mouse_coalesced_multiple_releases_emits_each_release() {
        let mut last = FROM_LEFT_1ST_BUTTON_PRESSED | RIGHTMOST_BUTTON_PRESSED;
        let released = mouse_event(0, 0, 0, 2, 2);

        assert_eq!(
            encode_mouse(released, &mut last).as_deref(),
            Some("\x1b[<0;3;3m\x1b[<2;3;3m")
        );
        assert_eq!(last, 0);
    }

    #[test]
    fn mouse_modifiers_are_or_ed_into_button() {
        let mut last = 0;
        // Ctrl+Shift wheel up -> 64 | 4 (shift) | 16 (ctrl) = 84.
        let event = mouse_event(
            0x0078_0000,
            MOUSE_WHEELED,
            SHIFT_PRESSED | LEFT_CTRL_PRESSED,
            0,
            0,
        );
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<84;1;1M")
        );
    }

    #[test]
    fn mouse_horizontal_wheel_right_encodes_sgr_button_67_without_polluting_button_state() {
        let mut last = FROM_LEFT_1ST_BUTTON_PRESSED;
        let event = mouse_event(0x0078_0000, MOUSE_HWHEELED, 0, 3, 3);

        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<67;4;4M")
        );
        assert_eq!(last, FROM_LEFT_1ST_BUTTON_PRESSED);
    }

    #[test]
    fn mouse_horizontal_wheel_left_encodes_sgr_button_66_with_modifiers() {
        let mut last = FROM_LEFT_1ST_BUTTON_PRESSED;
        let event = mouse_event(
            0xff88_0000,
            MOUSE_HWHEELED,
            SHIFT_PRESSED | LEFT_CTRL_PRESSED,
            3,
            3,
        );

        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<86;4;4M")
        );
        assert_eq!(last, FROM_LEFT_1ST_BUTTON_PRESSED);
    }

    #[test]
    fn mouse_idle_move_without_button_encodes_sgr_hover_motion() {
        let mut last = 0;
        let event = mouse_event(0, MOUSE_MOVED, 0, 5, 5);
        assert_eq!(
            encode_mouse(event, &mut last).as_deref(),
            Some("\x1b[<35;6;6M")
        );
    }
}
