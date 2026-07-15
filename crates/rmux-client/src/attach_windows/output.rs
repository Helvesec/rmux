use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    STD_OUTPUT_HANDLE,
};

use super::console_coordination::ATTACH_CONSOLE_IO;
use super::vt_input_passthrough::{write_console_wide, ScopedVtInputPassthrough};
use super::vt_mode_scanner::{OutputChunk, OutputWriteKind, VtModeScanner};

pub(super) struct AttachStdout<W> {
    fallback: W,
    console: Option<Utf16ConsoleWriter>,
}

impl<W> AttachStdout<W> {
    /// Builds the attach writer after `RawTerminal` has taken ownership of the
    /// standard console modes. The standard output handle is borrowed; only a
    /// separately opened `CONOUT$` fallback owns and restores its mode.
    pub(super) fn for_managed_terminal(fallback: W) -> Self {
        Self {
            fallback,
            console: Utf16ConsoleWriter::stdout_borrowing_managed_mode(),
        }
    }
}

impl<W> Write for AttachStdout<W>
where
    W: Write,
{
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(console) = &mut self.console {
            console.write_bytes(bytes)?;
            Ok(bytes.len())
        } else {
            self.fallback.write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(console) = &mut self.console {
            console.flush_pending()?;
        } else {
            self.fallback.flush()?;
        }
        Ok(())
    }
}

struct Utf16ConsoleWriter {
    handle: HANDLE,
    _owned_handle: Option<File>,
    mode_ownership: OutputModeOwnership,
    pending_utf8: Vec<u8>,
    vt_mode_scanner: VtModeScanner,
    vt_input_passthrough: Option<ScopedVtInputPassthrough>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputModeOwnership {
    Borrowed,
    Restore(u32),
}

// SAFETY: the writer stores only process console-handle metadata and is moved
// into the single attach output worker. Cross-thread input-mode access is
// serialized by `ATTACH_CONSOLE_IO`; no mutable state is shared directly.
unsafe impl Send for Utf16ConsoleWriter {}

impl Utf16ConsoleWriter {
    fn stdout_borrowing_managed_mode() -> Option<Self> {
        select_output_console(
            std_output_handle(),
            Self::from_borrowed_handle,
            Self::from_conout,
        )
    }

    fn from_conout() -> Option<Self> {
        let file = OpenOptions::new().write(true).open("CONOUT$").ok()?;
        let handle = file.as_raw_handle() as HANDLE;
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let original_mode = configure_output_console(handle).ok()?;
        Some(Self::from_handle(
            handle,
            Some(file),
            OutputModeOwnership::Restore(original_mode),
        ))
    }

    fn from_borrowed_handle(handle: HANDLE) -> Option<Self> {
        probe_output_console(handle).ok()?;
        Some(Self::from_handle(
            handle,
            None,
            OutputModeOwnership::Borrowed,
        ))
    }

    fn from_handle(
        handle: HANDLE,
        owned_handle: Option<File>,
        mode_ownership: OutputModeOwnership,
    ) -> Self {
        Self {
            handle,
            _owned_handle: owned_handle,
            mode_ownership,
            pending_utf8: Vec::new(),
            vt_mode_scanner: VtModeScanner::default(),
            vt_input_passthrough: ScopedVtInputPassthrough::for_output(handle),
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pending_utf8.extend_from_slice(bytes);
        let valid_len = writable_utf8_prefix_len(&self.pending_utf8);
        if valid_len == 0 {
            return Ok(());
        }

        let valid_bytes = self.pending_utf8[..valid_len].to_vec();
        self.write_scanned_bytes(&valid_bytes)?;
        self.pending_utf8.drain(..valid_len);
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending_utf8.is_empty() {
            return Ok(());
        }
        let valid_len = writable_utf8_prefix_len(&self.pending_utf8);
        if valid_len == 0 {
            return Ok(());
        }

        let valid_bytes = self.pending_utf8[..valid_len].to_vec();
        self.write_scanned_bytes(&valid_bytes)?;
        self.pending_utf8.drain(..valid_len);
        Ok(())
    }

    fn write_scanned_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        // Older Windows builds, redirected handles, and failed capability
        // probes retain the byte-for-byte writer path used before this fix.
        if self.vt_input_passthrough.is_none() {
            return self.write_normal_bytes(bytes);
        }
        for chunk in self.vt_mode_scanner.push(bytes) {
            self.write_chunk(&chunk)?;
        }
        Ok(())
    }

    fn write_normal_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        let text = String::from_utf8_lossy(bytes);
        let wide = text.encode_utf16().collect::<Vec<_>>();
        write_console_wide(self.handle, &wide)
    }

    fn write_chunk(&self, chunk: &OutputChunk) -> io::Result<()> {
        let text = String::from_utf8_lossy(&chunk.bytes);
        let wide = text.encode_utf16().collect::<Vec<_>>();
        if chunk.kind == OutputWriteKind::ScopedVtInput {
            if let Some(passthrough) = self.vt_input_passthrough {
                return passthrough.write_wide(&wide);
            }
        }
        write_console_wide(self.handle, &wide)
    }
}

fn select_output_console<T>(
    stdout_handle: Option<HANDLE>,
    from_stdout: impl FnOnce(HANDLE) -> Option<T>,
    from_absent_stdout: impl FnOnce() -> Option<T>,
) -> Option<T> {
    match stdout_handle {
        Some(handle) => from_stdout(handle),
        None => from_absent_stdout(),
    }
}

impl Drop for Utf16ConsoleWriter {
    fn drop(&mut self) {
        if let Some(pending) = self.vt_mode_scanner.finish() {
            let _ = self.write_chunk(&pending);
        }
        if let OutputModeOwnership::Restore(mode) = self.mode_ownership {
            let _ = restore_output_console(self.handle, mode);
        }
    }
}

fn probe_output_console(handle: HANDLE) -> io::Result<()> {
    ATTACH_CONSOLE_IO.synchronized(|| {
        let mut mode = 0;
        let ok = unsafe {
            // SAFETY: handle is borrowed and mode points to writable storage.
            GetConsoleMode(handle, &mut mode)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })?
}

fn configure_output_console(handle: HANDLE) -> io::Result<u32> {
    ATTACH_CONSOLE_IO.synchronized(|| {
        let mut mode = 0;
        let ok = unsafe {
            // SAFETY: handle is borrowed and mode points to writable storage.
            GetConsoleMode(handle, &mut mode)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let enabled_mode = mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if enabled_mode != mode {
            let ok = unsafe {
                // SAFETY: handle is a console output handle and enabled_mode
                // only adds the documented VT output flag.
                SetConsoleMode(handle, enabled_mode)
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(mode)
    })?
}

fn restore_output_console(handle: HANDLE, mode: u32) -> io::Result<()> {
    ATTACH_CONSOLE_IO.synchronized(|| {
        let ok = unsafe {
            // SAFETY: handle and mode were captured by a successful
            // GetConsoleMode call during writer construction.
            SetConsoleMode(handle, mode)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })?
}

fn std_output_handle() -> Option<HANDLE> {
    let handle = unsafe {
        // SAFETY: GetStdHandle accepts the documented STD_* constants.
        GetStdHandle(STD_OUTPUT_HANDLE)
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return None;
    }
    Some(handle)
}

fn writable_utf8_prefix_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => bytes.len(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::super::terminal_cleanup::fallback_attach_stop_sequence;
    use super::{
        select_output_console, writable_utf8_prefix_len, OutputModeOwnership, OutputWriteKind,
        Utf16ConsoleWriter, VtModeScanner,
    };

    #[test]
    fn valid_redirected_stdout_never_falls_through_to_conout() {
        let conout_attempted = Cell::new(false);
        let result = select_output_console(
            Some(1_usize as windows_sys::Win32::Foundation::HANDLE),
            |_| None::<()>,
            || {
                conout_attempted.set(true);
                Some(())
            },
        );

        assert!(result.is_none());
        assert!(
            !conout_attempted.get(),
            "a valid redirected stdout must remain on the fallback writer"
        );
    }

    #[test]
    fn absent_stdout_may_open_conout() {
        let conout_attempted = Cell::new(false);
        let result = select_output_console(
            None,
            |_| None::<&str>,
            || {
                conout_attempted.set(true);
                Some("conout")
            },
        );

        assert_eq!(result, Some("conout"));
        assert!(conout_attempted.get());
    }

    #[test]
    fn utf8_prefix_waits_for_split_codepoint() {
        let glyph = "é".as_bytes();
        assert_eq!(writable_utf8_prefix_len(&glyph[..1]), 0);
        assert_eq!(writable_utf8_prefix_len(glyph), glyph.len());
    }

    #[test]
    fn utf8_prefix_allows_ascii_escape_sequences() {
        let bytes = b"\x1b[31mhello\x1b[0m";
        assert_eq!(writable_utf8_prefix_len(bytes), bytes.len());
    }

    #[test]
    fn flush_keeps_split_codepoint_pending() {
        let glyph = "é".as_bytes();
        let mut writer = Utf16ConsoleWriter {
            handle: std::ptr::null_mut(),
            _owned_handle: None,
            mode_ownership: OutputModeOwnership::Borrowed,
            pending_utf8: glyph[..1].to_vec(),
            vt_mode_scanner: VtModeScanner::default(),
            vt_input_passthrough: None,
        };

        writer.flush_pending().expect("split utf8 waits");

        assert_eq!(writer.pending_utf8, glyph[..1]);
    }

    #[test]
    fn fallback_cleanup_scopes_every_input_reporting_reset() {
        let chunks =
            VtModeScanner::default().push(&fallback_attach_stop_sequence("xterm-256color"));
        let scoped = chunks
            .iter()
            .filter(|chunk| chunk.kind == OutputWriteKind::ScopedVtInput)
            .flat_map(|chunk| chunk.bytes.iter().copied())
            .collect::<Vec<_>>();

        for expected in [
            b"\x1b[?1004l".as_slice(),
            b"\x1b[?1000l".as_slice(),
            b"\x1b[?1002l".as_slice(),
            b"\x1b[?1003l".as_slice(),
            b"\x1b[?1005l".as_slice(),
            b"\x1b[?1006l".as_slice(),
            b"\x1b[?2004l".as_slice(),
            b"\x1b[?2031l".as_slice(),
        ] {
            assert!(
                scoped
                    .windows(expected.len())
                    .any(|window| window == expected),
                "missing scoped reset: {expected:?}"
            );
        }
    }
}
