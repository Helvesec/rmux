//! Opt-in diagnostics at the native record boundary and encoded Ctrl boundary.
use std::sync::OnceLock;

use rmux_proto::AttachedWindowsConsoleKey;

use super::ConsoleKeyEvent;

fn enabled() -> bool {
    static TRACE_WINDOWS_KEYS: OnceLock<bool> = OnceLock::new();
    *TRACE_WINDOWS_KEYS.get_or_init(|| std::env::var_os("RMUX_TRACE_WINDOWS_KEYS").is_some())
}

pub(super) fn trace_console_key_event(event: ConsoleKeyEvent) {
    if !enabled() {
        return;
    }
    tracing::debug!(
        target: "rmux::windows_keys",
        key_down = event.key_down,
        repeat_count = event.repeat_count,
        virtual_key_code = event.virtual_key_code,
        virtual_scan_code = event.virtual_scan_code,
        unicode_char = event.unicode_char,
        control_key_state = event.control_key_state,
        "read Windows KEY_EVENT_RECORD"
    );
}

pub(super) fn trace_windows_console_key(key: AttachedWindowsConsoleKey, bytes: &[u8]) {
    if !enabled() {
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
