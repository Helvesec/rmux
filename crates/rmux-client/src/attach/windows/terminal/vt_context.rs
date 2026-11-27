use super::super::{
    console_coordination::ATTACH_CONSOLE_IO,
    vt_input_passthrough::write_console_wide,
    windows_version::{current_windows_version, supports_scoped_vt_input},
};
use std::io;
use std::sync::atomic::{AtomicPtr, Ordering};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::Console::{
        GetConsoleMode, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    },
};

static INPUT_CONTEXT: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

pub(super) fn is_active(input: HANDLE) -> bool {
    !input.is_null() && INPUT_CONTEXT.load(Ordering::Acquire) == input
}

pub(super) fn supported() -> bool {
    current_windows_version().is_some_and(supports_scoped_vt_input)
}

/// Change the input grammar and its key-reporting mode in one console epoch.
/// VT preserves the explicit paste delimiters; Win32 reporting preserves the
/// physical key identity that ordinary VT would otherwise flatten to text.
pub(super) fn set(input: HANDLE, output: HANDLE, mode: u32, enabled: bool) -> io::Result<()> {
    ATTACH_CONSOLE_IO.synchronized(|| {
        let previous = console_mode(input)?;
        // ConPTY drops FOCUS_EVENT records in VT input mode unless focus
        // reporting is enabled locally, even when its outer pipe reports focus.
        let sequence: Vec<u16> = if enabled {
            "\x1b[?1004h\x1b[?9001h"
        } else {
            "\x1b[?9001l\x1b[?1004l"
        }
        .encode_utf16()
        .collect();
        if enabled {
            set_mode(input, mode | ENABLE_VIRTUAL_TERMINAL_INPUT)?;
            if let Err(error) = write_console_wide(output, &sequence) {
                let _ = set_mode(input, previous);
                return Err(error);
            }
            INPUT_CONTEXT.store(input, Ordering::Release);
            Ok(())
        } else {
            // Restoration is idempotent, including an explicit restore followed
            // by Drop. Never print a control sequence to a non-VT output.
            let reporting = if is_active(input) && previous & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
                console_mode(output).and_then(|flags| {
                    if flags & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0 {
                        write_console_wide(output, &sequence)
                    } else {
                        Ok(())
                    }
                })
            } else {
                Ok(())
            };
            set_mode(input, mode)?;
            let _ = INPUT_CONTEXT.compare_exchange(
                input,
                std::ptr::null_mut(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            reporting
        }
    })?
}

fn console_mode(handle: HANDLE) -> io::Result<u32> {
    let mut mode = 0;
    // SAFETY: the caller borrows validated console handles and writable storage.
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mode)
}

fn set_mode(handle: HANDLE, mode: u32) -> io::Result<()> {
    // SAFETY: the borrowed console handle and documented mode flags stay valid.
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
