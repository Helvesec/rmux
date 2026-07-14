use std::io;
use std::os::windows::io::RawHandle;
use std::sync::OnceLock;

use rmux_proto::AttachedWindowsConsoleKey;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED,
    FROM_LEFT_3RD_BUTTON_PRESSED, INPUT_RECORD, KEY_EVENT, KEY_EVENT_RECORD, LEFT_ALT_PRESSED,
    LEFT_CTRL_PRESSED, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED, MOUSE_WHEELED,
    RIGHTMOST_BUTTON_PRESSED, RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F10, VK_F11, VK_F12,
    VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_HOME, VK_INSERT, VK_LCONTROL,
    VK_LEFT, VK_LMENU, VK_LSHIFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT,
    VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};

use super::console_coordination::{ConsoleIoCoordinator, ATTACH_CONSOLE_IO};
use super::console_input_read::{
    read_console_input_batch_with, ConsoleInputApi, ConsoleInputRead, Win32ConsoleInput,
};

const ATTACH_INPUT_CHUNK_LIMIT: usize = 4096;
const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";
const CONSOLE_INPUT_RECORD_BATCH: usize = 32;
const HIGH_SURROGATE_START: u16 = 0xd800;
const HIGH_SURROGATE_END: u16 = 0xdbff;
const LOW_SURROGATE_START: u16 = 0xdc00;
const LOW_SURROGATE_END: u16 = 0xdfff;

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
}

impl ConsoleInputReader {
    pub(super) fn from_handle(handle: RawHandle) -> Option<Self> {
        let handle = handle as HANDLE;
        let mut mode = 0;
        let ok = ATTACH_CONSOLE_IO
            .synchronized(|| unsafe {
                // SAFETY: `mode` is writable and `handle` is only borrowed for
                // this capability probe.
                GetConsoleMode(handle, &mut mode)
            })
            .ok()?;
        (ok != 0).then_some(Self {
            handle,
            pending_high_surrogate: None,
            last_mouse_button_state: 0,
            paste_open: false,
            paste_carryover: Vec::new(),
        })
    }

    pub(super) fn read_key_inputs(&mut self) -> io::Result<Vec<AttachInput>> {
        self.read_key_inputs_with(&ATTACH_CONSOLE_IO, &Win32ConsoleInput)
    }

    fn read_key_inputs_with<Api>(
        &mut self,
        coordinator: &ConsoleIoCoordinator,
        api: &Api,
    ) -> io::Result<Vec<AttachInput>>
    where
        Api: ConsoleInputApi,
    {
        let mut records = [INPUT_RECORD::default(); CONSOLE_INPUT_RECORD_BATCH];
        let ConsoleInputRead::Records {
            records_read,
            drained,
        } = read_console_input_batch_with(coordinator, api, self.handle, &mut records)?
        else {
            // Teardown may flush an input which was readable immediately
            // before the attach input-read lease was acquired. Preserve all
            // decoder and paste state until a real record batch is available.
            return Ok(Vec::new());
        };

        let mut events = Vec::with_capacity(records_read);
        for record in &records[..records_read] {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    let event = unsafe {
                        // SAFETY: EventType says this union currently contains a KEY_EVENT_RECORD.
                        record.Event.KeyEvent
                    };
                    events.push(BatchEvent::Key(ConsoleKeyEvent::from_win32(event)));
                }
                MOUSE_EVENT => {
                    let event = unsafe {
                        // SAFETY: EventType says this union currently holds a MOUSE_EVENT_RECORD.
                        record.Event.MouseEvent
                    };
                    events.push(BatchEvent::Mouse(event));
                }
                _ => {}
            }
        }

        // A paste is injected into the console input buffer as one atomic burst,
        // so a single ReadConsoleInputW returns every pasted character at once
        // while interactive typing delivers one key per call. That burst is the
        // only paste signal available in record mode: pasted characters carry
        // their real virtual-key codes (a pasted "a" is byte-for-byte a typed
        // "a") and a pasted newline arrives as VK_RETURN just like a typed Enter,
        // so there is no per-record "injected" flag. If the buffer still holds
        // events after this batch the paste continues into the next one.
        Ok(encode_input_batch(
            &events,
            drained,
            &mut self.paste_open,
            &mut self.paste_carryover,
            &mut self.pending_high_surrogate,
            &mut self.last_mouse_button_state,
        ))
    }
}

enum BatchEvent {
    Key(ConsoleKeyEvent),
    Mouse(MOUSE_EVENT_RECORD),
}

/// A pasted printable character — or a pasted newline, which reaches the console
/// as `VK_RETURN` — is an unmodified key-down. Control and Alt chords are never
/// part of a paste and force the normal per-key path, except for AltGr on
/// European layouts (LEFT_CTRL|RIGHT_ALT) which is a legitimate paste
/// character — the encoding paths themselves treat AltGr as text, so paste
/// classification does too. Records with `unicode_char == 0` (pure modifier
/// key-downs synthesized by classic conhost around shifted characters) do not
/// count as text and are ignored here so they neither open nor close a paste.
fn is_paste_text(event: &ConsoleKeyEvent) -> bool {
    if !event.key_down || event.unicode_char == 0 {
        return false;
    }
    let alt_gr = alt_gr_pressed(event.control_key_state);
    let ctrl = ctrl_pressed(event.control_key_state) && !alt_gr;
    let meta = meta_pressed(event.control_key_state) && !alt_gr;
    !ctrl && !meta
}

/// A pure-modifier key-down (`unicode_char == 0`) — the shift/ctrl/alt records
/// classic conhost interleaves with pasted printable keys — is neither paste
/// text nor a "real" other key. Treating it as either would mis-classify a
/// paste that contains uppercase letters (VK_SHIFT down/up bracket each
/// shifted character) or a paste under AltGr (VK_LCONTROL down/up around each
/// AltGr character). This keeps burst detection stable under any layout.
fn is_pure_modifier_key_down(event: &ConsoleKeyEvent) -> bool {
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
fn strip_embedded_paste_markers(text: &mut Vec<u8>) {
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
fn encode_input_batch(
    events: &[BatchEvent],
    drained: bool,
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
            if is_paste_text(key) {
                text_downs += 1;
            } else {
                has_other_key = true;
            }
        }
    }

    // A fresh burst needs at least two pasted characters — a lone character is
    // indistinguishable from a keystroke — while a run already open continues on
    // any further pasted text until the input buffer drains.
    // Native MOUSE_EVENT records can be coalesced into the same console read as
    // a clipboard burst. They are not evidence that the key records are live:
    // when the keys form a paste, prefer the paste and suppress those mouse
    // records rather than turning clipboard bytes into interactive input.
    let paste_text_only = !has_other_key;
    let is_paste = paste_text_only
        && (text_downs >= 2 || (*paste_open && text_downs >= 1) || (!drained && text_downs >= 1));

    let mut inputs = Vec::new();
    if is_paste {
        // Emit the markers and the pasted text as ONE keystroke per batch. If
        // the open marker and the text were separate `AttachInput`s the daemon
        // would process the plain-text keystroke through its fast input path
        // (which bypasses the bracketed-paste decode), delivering the paste
        // body to the pane without the markers.
        let opening = !*paste_open;
        *paste_open = true;
        let mut payload = Vec::new();
        if opening {
            payload.extend_from_slice(BRACKETED_PASTE_START);
        }
        let mut text = std::mem::take(paste_carryover);
        for event in events {
            if let BatchEvent::Key(key) = event {
                text.extend_from_slice(&encode_key_event(*key, pending_high_surrogate));
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

fn encode_live_input_batch(
    events: &[BatchEvent],
    pending_high_surrogate: &mut Option<u16>,
    last_mouse_button_state: &mut u32,
) -> Vec<AttachInput> {
    let mut inputs = Vec::new();
    for event in events {
        match event {
            BatchEvent::Key(key) => {
                let logical_key = ConsoleKeyEvent {
                    repeat_count: 1,
                    ..*key
                };
                let bytes = encode_key_event(logical_key, pending_high_surrogate);
                if bytes.is_empty() {
                    continue;
                }
                let input = windows_console_key_for_event(*key, &bytes).map_or_else(
                    || AttachInput::repeated_bytes(bytes.clone(), key.repeat_count),
                    |console_key| {
                        trace_windows_console_key(console_key, &bytes);
                        AttachInput::with_windows_console_key(bytes.clone(), console_key)
                    },
                );
                inputs.push(input);
            }
            BatchEvent::Mouse(mouse) => {
                for bytes in encode_mouse_event(*mouse, last_mouse_button_state) {
                    inputs.push(AttachInput::bytes(bytes));
                }
            }
        }
    }
    inputs
}

// SGR mouse button codes (xterm). Modifiers are OR-ed in; drag adds 32.
const SGR_BTN_LEFT: u16 = 0;
const SGR_BTN_MIDDLE: u16 = 1;
const SGR_BTN_RIGHT: u16 = 2;
const SGR_BTN_NONE: u16 = 3;
const SGR_WHEEL_UP: u16 = 64;
const SGR_WHEEL_DOWN: u16 = 65;
const SGR_WHEEL_LEFT: u16 = 66;
const SGR_WHEEL_RIGHT: u16 = 67;
const SGR_MOD_SHIFT: u16 = 4;
const SGR_MOD_ALT: u16 = 8;
const SGR_MOD_CTRL: u16 = 16;
const SGR_DRAG_FLAG: u16 = 32;

/// Encode a Win32 console `MOUSE_EVENT_RECORD` into SGR mouse sequences
/// (`\x1b[<b;x;yM` press/wheel, `\x1b[<b;x;ym` release). The rmux server gates these
/// against the active mouse mode, so we always emit and let the server decide.
fn encode_mouse_event(event: MOUSE_EVENT_RECORD, last_button_state: &mut u32) -> Vec<Vec<u8>> {
    let x = u16::try_from(event.dwMousePosition.X.max(0)).unwrap_or(0) + 1;
    let y = u16::try_from(event.dwMousePosition.Y.max(0)).unwrap_or(0) + 1;
    let buttons = event.dwButtonState;
    let modifiers = sgr_modifier_bits(event.dwControlKeyState);

    if event.dwEventFlags & MOUSE_WHEELED != 0 {
        // High word of dwButtonState is the signed wheel delta; positive = up.
        let delta = (buttons >> 16) as i16;
        let base = if delta >= 0 {
            SGR_WHEEL_UP
        } else {
            SGR_WHEEL_DOWN
        };
        return vec![format_sgr_mouse(base | modifiers, x, y, 'M')];
    }
    if event.dwEventFlags & MOUSE_HWHEELED != 0 {
        // High word of dwButtonState is the signed horizontal wheel delta;
        // Windows reports positive = right, while SGR 66/67 are left/right.
        let delta = (buttons >> 16) as i16;
        let base = if delta >= 0 {
            SGR_WHEEL_RIGHT
        } else {
            SGR_WHEEL_LEFT
        };
        return vec![format_sgr_mouse(base | modifiers, x, y, 'M')];
    }

    // Ignore bare double-click flags; movement is handled below.
    let previous = *last_button_state;
    let pressed = buttons & !previous;
    let released = previous & !buttons;
    *last_button_state = buttons;

    let is_move = event.dwEventFlags & MOUSE_MOVED != 0;

    if pressed == 0 && released == 0 {
        if is_move {
            // Drag (motion with a button held) or bare hover motion.
            if let Some(button) = sgr_button_for_state(buttons) {
                return vec![format_sgr_mouse(
                    button | SGR_DRAG_FLAG | modifiers,
                    x,
                    y,
                    'M',
                )];
            }
            return vec![format_sgr_mouse(
                SGR_BTN_NONE | SGR_DRAG_FLAG | modifiers,
                x,
                y,
                'M',
            )];
        }
        return Vec::new();
    }

    let mut output = Vec::new();
    for button in sgr_buttons_for_state(released) {
        output.push(format_sgr_mouse(button | modifiers, x, y, 'm'));
    }
    for button in sgr_buttons_for_state(pressed) {
        output.push(format_sgr_mouse(button | modifiers, x, y, 'M'));
    }
    output
}

fn sgr_button_for_state(buttons: u32) -> Option<u16> {
    if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
        Some(SGR_BTN_LEFT)
    } else if buttons & RIGHTMOST_BUTTON_PRESSED != 0 {
        Some(SGR_BTN_RIGHT)
    } else if buttons & (FROM_LEFT_2ND_BUTTON_PRESSED | FROM_LEFT_3RD_BUTTON_PRESSED) != 0 {
        Some(SGR_BTN_MIDDLE)
    } else {
        None
    }
}

fn sgr_buttons_for_state(buttons: u32) -> Vec<u16> {
    let mut encoded = Vec::new();
    if buttons & FROM_LEFT_1ST_BUTTON_PRESSED != 0 {
        encoded.push(SGR_BTN_LEFT);
    }
    if buttons & RIGHTMOST_BUTTON_PRESSED != 0 {
        encoded.push(SGR_BTN_RIGHT);
    }
    if buttons & (FROM_LEFT_2ND_BUTTON_PRESSED | FROM_LEFT_3RD_BUTTON_PRESSED) != 0 {
        encoded.push(SGR_BTN_MIDDLE);
    }
    encoded
}

fn sgr_modifier_bits(control_key_state: u32) -> u16 {
    let mut bits = 0;
    if control_key_state & SHIFT_PRESSED != 0 {
        bits |= SGR_MOD_SHIFT;
    }
    if control_key_state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        bits |= SGR_MOD_ALT;
    }
    if control_key_state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        bits |= SGR_MOD_CTRL;
    }
    bits
}

fn format_sgr_mouse(button: u16, x: u16, y: u16, terminator: char) -> Vec<u8> {
    format!("\x1b[<{button};{x};{y}{terminator}").into_bytes()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConsoleKeyEvent {
    key_down: bool,
    repeat_count: u16,
    virtual_key_code: u16,
    virtual_scan_code: u16,
    unicode_char: u16,
    control_key_state: u32,
}

impl ConsoleKeyEvent {
    fn from_win32(event: KEY_EVENT_RECORD) -> Self {
        let unicode_char = unsafe {
            // SAFETY: Reading the UnicodeChar arm is valid for KEY_EVENT_RECORD values returned
            // by ReadConsoleInputW.
            event.uChar.UnicodeChar
        };
        Self {
            key_down: event.bKeyDown != 0,
            repeat_count: event.wRepeatCount,
            virtual_key_code: event.wVirtualKeyCode,
            virtual_scan_code: event.wVirtualScanCode,
            unicode_char,
            control_key_state: event.dwControlKeyState,
        }
    }
}

fn windows_console_key_for_event(
    event: ConsoleKeyEvent,
    encoded_bytes: &[u8],
) -> Option<AttachedWindowsConsoleKey> {
    if !event.key_down
        || encoded_bytes.is_empty()
        || !ctrl_pressed(event.control_key_state)
        || alt_gr_pressed(event.control_key_state)
        || meta_pressed(event.control_key_state)
    {
        return None;
    }

    Some(AttachedWindowsConsoleKey::new(
        event.virtual_key_code,
        event.virtual_scan_code,
        event.unicode_char,
        event.control_key_state,
        event.repeat_count.max(1),
    ))
}

fn trace_windows_console_key(key: AttachedWindowsConsoleKey, bytes: &[u8]) {
    static TRACE_WINDOWS_KEYS: OnceLock<bool> = OnceLock::new();
    if !*TRACE_WINDOWS_KEYS.get_or_init(|| std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_some()) {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        virtual_key_code = key.virtual_key_code(),
        virtual_scan_code = key.virtual_scan_code(),
        unicode_char = key.unicode_char(),
        control_key_state = key.control_key_state(),
        repeat_count = key.repeat_count(),
        ?bytes,
        "read Windows attach console key"
    );
}

fn encode_key_event(event: ConsoleKeyEvent, pending_high_surrogate: &mut Option<u16>) -> Vec<u8> {
    if !event.key_down {
        return Vec::new();
    }

    let repeat_count = usize::from(event.repeat_count.max(1));
    let mut once = if event.unicode_char != 0 && !virtual_key_requires_modifier_mapping(event) {
        encode_unicode_key_event(event, pending_high_surrogate)
    } else {
        pending_high_surrogate.take();
        encode_virtual_key_event(event)
    };

    if once.is_empty() || repeat_count == 1 {
        return once;
    }

    let single = once.clone();
    once.reserve(single.len().saturating_mul(repeat_count.saturating_sub(1)));
    for _ in 1..repeat_count {
        once.extend_from_slice(&single);
    }
    once
}

fn virtual_key_requires_modifier_mapping(event: ConsoleKeyEvent) -> bool {
    matches!(
        event.virtual_key_code,
        VK_BACK | VK_ESCAPE | VK_RETURN | VK_TAB
    )
}

fn encode_unicode_key_event(
    event: ConsoleKeyEvent,
    pending_high_surrogate: &mut Option<u16>,
) -> Vec<u8> {
    let alt = meta_pressed(event.control_key_state);
    let ctrl = ctrl_pressed(event.control_key_state) && !alt_gr_pressed(event.control_key_state);

    if ctrl {
        if let Some(control) = control_byte_for_event(event) {
            return with_meta_prefix(alt, &[control]);
        }
    }

    let Some(character) = char_from_utf16_event(event.unicode_char, pending_high_surrogate) else {
        return Vec::new();
    };
    let mut utf8 = [0; 4];
    with_meta_prefix(alt, character.encode_utf8(&mut utf8).as_bytes())
}

fn encode_virtual_key_event(event: ConsoleKeyEvent) -> Vec<u8> {
    let state = event.control_key_state;
    let alt = meta_pressed(state);
    let modifier = xterm_modifier_parameter(state);
    let key = event.virtual_key_code;

    if key == VK_ESCAPE {
        return if alt {
            b"\x1b\x1b".to_vec()
        } else {
            b"\x1b".to_vec()
        };
    }
    if key == VK_SPACE && ctrl_pressed(state) && !alt_gr_pressed(state) {
        return with_meta_prefix(alt, &[0x00]);
    }
    if ctrl_pressed(state) && !alt_gr_pressed(state) {
        if let Some(control) = control_byte_for_virtual_key(key) {
            return with_meta_prefix(alt, &[control]);
        }
    }
    if key == VK_BACK {
        return if modifier == 1 {
            b"\x7f".to_vec()
        } else {
            csi_u_sequence(0x7f, modifier)
        };
    }
    if key == VK_TAB {
        return match modifier {
            1 => b"\t".to_vec(),
            2 => b"\x1b[Z".to_vec(),
            _ => csi_u_sequence(0x09, modifier),
        };
    }
    if key == VK_RETURN {
        return if modifier == 1 {
            b"\r".to_vec()
        } else {
            csi_u_sequence(0x0d, modifier)
        };
    }

    if let Some((normal, modified_final)) = cursor_key_sequence(key) {
        return if modifier == 1 {
            normal.to_vec()
        } else {
            format!("\x1b[1;{modifier}{}", char::from(modified_final)).into_bytes()
        };
    }
    if let Some(number) = tilde_key_number(key) {
        return if modifier == 1 {
            format!("\x1b[{number}~").into_bytes()
        } else {
            format!("\x1b[{number};{modifier}~").into_bytes()
        };
    }
    if let Some((normal, modified_final)) = function_key_sequence(key) {
        return if modifier == 1 {
            normal.to_vec()
        } else {
            format!("\x1b[1;{modifier}{}", char::from(modified_final)).into_bytes()
        };
    }

    Vec::new()
}

fn char_from_utf16_event(value: u16, pending_high_surrogate: &mut Option<u16>) -> Option<char> {
    if (HIGH_SURROGATE_START..=HIGH_SURROGATE_END).contains(&value) {
        *pending_high_surrogate = Some(value);
        return None;
    }

    if let Some(high) = pending_high_surrogate.take() {
        if (LOW_SURROGATE_START..=LOW_SURROGATE_END).contains(&value) {
            let high = u32::from(high - HIGH_SURROGATE_START);
            let low = u32::from(value - LOW_SURROGATE_START);
            return char::from_u32(0x10000 + ((high << 10) | low));
        }
    }

    char::from_u32(u32::from(value))
}

fn control_byte_for_event(event: ConsoleKeyEvent) -> Option<u8> {
    if (1..=0x1a).contains(&event.unicode_char) {
        return Some(event.unicode_char as u8);
    }
    match event.virtual_key_code {
        value if value == VK_SPACE => return Some(0x00),
        _ => {}
    }

    let character = char::from_u32(u32::from(event.unicode_char))?;
    let character = character.to_ascii_lowercase();
    match character {
        'a'..='z' => Some((character as u8 - b'a') + 1),
        ' ' | '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn control_byte_for_virtual_key(key: u16) -> Option<u8> {
    match key {
        0x41..=0x5a => Some((key as u8 - b'A') + 1),
        0x32 => Some(0x00),
        0x33 => Some(0x1b),
        0x34 => Some(0x1c),
        0x35 => Some(0x1d),
        0x36 => Some(0x1e),
        0x5f => Some(0x1f),
        _ => None,
    }
}

fn with_meta_prefix(meta: bool, bytes: &[u8]) -> Vec<u8> {
    if !meta {
        return bytes.to_vec();
    }
    let mut output = Vec::with_capacity(bytes.len() + 1);
    output.push(0x1b);
    output.extend_from_slice(bytes);
    output
}

fn ctrl_pressed(state: u32) -> bool {
    state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0
}

fn meta_pressed(state: u32) -> bool {
    state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 && !alt_gr_pressed(state)
}

fn shift_pressed(state: u32) -> bool {
    state & SHIFT_PRESSED != 0
}

fn alt_gr_pressed(state: u32) -> bool {
    state & RIGHT_ALT_PRESSED != 0
        && state & LEFT_CTRL_PRESSED != 0
        && state & LEFT_ALT_PRESSED == 0
        && state & RIGHT_CTRL_PRESSED == 0
}

fn xterm_modifier_parameter(state: u32) -> u8 {
    let shift = shift_pressed(state);
    let meta = meta_pressed(state);
    let ctrl = ctrl_pressed(state) && !alt_gr_pressed(state);
    1 + u8::from(shift) + (u8::from(meta) * 2) + (u8::from(ctrl) * 4)
}

fn csi_u_sequence(key: u32, modifier: u8) -> Vec<u8> {
    format!("\x1b[{key};{modifier}u").into_bytes()
}

fn cursor_key_sequence(key: u16) -> Option<(&'static [u8], u8)> {
    match key {
        value if value == VK_UP => Some((b"\x1b[A", b'A')),
        value if value == VK_DOWN => Some((b"\x1b[B", b'B')),
        value if value == VK_RIGHT => Some((b"\x1b[C", b'C')),
        value if value == VK_LEFT => Some((b"\x1b[D", b'D')),
        value if value == VK_HOME => Some((b"\x1b[H", b'H')),
        value if value == VK_END => Some((b"\x1b[F", b'F')),
        _ => None,
    }
}

fn tilde_key_number(key: u16) -> Option<u8> {
    match key {
        value if value == VK_INSERT => Some(2),
        value if value == VK_DELETE => Some(3),
        value if value == VK_PRIOR => Some(5),
        value if value == VK_NEXT => Some(6),
        value if value == VK_F5 => Some(15),
        value if value == VK_F6 => Some(17),
        value if value == VK_F7 => Some(18),
        value if value == VK_F8 => Some(19),
        value if value == VK_F9 => Some(20),
        value if value == VK_F10 => Some(21),
        value if value == VK_F11 => Some(23),
        value if value == VK_F12 => Some(24),
        _ => None,
    }
}

fn function_key_sequence(key: u16) -> Option<(&'static [u8], u8)> {
    match key {
        value if value == VK_F1 => Some((b"\x1bOP", b'P')),
        value if value == VK_F2 => Some((b"\x1bOQ", b'Q')),
        value if value == VK_F3 => Some((b"\x1bOR", b'R')),
        value if value == VK_F4 => Some((b"\x1bOS", b'S')),
        _ => None,
    }
}

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

    use super::super::console_coordination::ConsoleIoCoordinator;
    use super::super::console_input_read::ConsoleInputApi;
    use super::{
        attach_input_chunks, encode_input_batch, encode_key_event, encode_mouse_event,
        strip_embedded_paste_markers, windows_console_key_for_event, AttachInput, BatchEvent,
        ConsoleInputReader, ConsoleKeyEvent, ATTACH_INPUT_CHUNK_LIMIT, BRACKETED_PASTE_END,
        BRACKETED_PASTE_START,
    };
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        FROM_LEFT_1ST_BUTTON_PRESSED, FROM_LEFT_2ND_BUTTON_PRESSED, INPUT_RECORD, INPUT_RECORD_0,
        KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED,
        MOUSE_EVENT_RECORD, MOUSE_HWHEELED, MOUSE_MOVED, MOUSE_WHEELED, RIGHTMOST_BUTTON_PRESSED,
        RIGHT_ALT_PRESSED, SHIFT_PRESSED,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F5, VK_HOME, VK_LEFT, VK_RETURN,
        VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
    };

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
                    calls: Vec::new(),
                }),
            }
        }

        fn flush(&self) {
            let mut state = self.state.lock().expect("fake console remains usable");
            state.calls.push(FakeConsoleCall::Flush);
            state.records.clear();
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
            let records_read = records.len().min(state.records.len());
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
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: 1,
                    wRepeatCount: 1,
                    wVirtualKeyCode: u16::from(byte),
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 {
                        UnicodeChar: u16::from(byte),
                    },
                    dwControlKeyState: 0,
                },
            },
        }
    }

    fn fake_console_reader() -> ConsoleInputReader {
        ConsoleInputReader {
            handle: std::ptr::null_mut(),
            pending_high_surrogate: None,
            last_mouse_button_state: 0,
            paste_open: false,
            paste_carryover: Vec::new(),
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
    fn sgr_key_event_paste_stays_fail_closed_across_every_console_batch_boundary() {
        let sequence = b"\x1b[<32;123;45M";
        for split in 1..sequence.len() {
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
