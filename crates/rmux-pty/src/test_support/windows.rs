//! Native console setup for isolated child-process test fixtures.

use std::io;
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// Enables raw VT input and VT output on the calling fixture's console.
///
/// Call only inside a dedicated test child: modes are not restored and persist
/// until that child's console closes. No user or machine configuration changes.
pub fn enable_raw_console_for_test() -> io::Result<()> {
    // SAFETY: standard handles are borrowed, never closed; the output-mode
    // pointer is valid for this call. Every fallible console operation is checked.
    unsafe {
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        if GetConsoleMode(output, &mut mode) == 0
            || SetConsoleMode(output, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0
            || SetConsoleMode(
                GetStdHandle(STD_INPUT_HANDLE),
                ENABLE_VIRTUAL_TERMINAL_INPUT,
            ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
