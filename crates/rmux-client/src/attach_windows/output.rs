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
    pub(super) fn new(fallback: W) -> Self {
        Self {
            fallback,
            console: Utf16ConsoleWriter::stdout(),
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
    original_mode: u32,
    pending_utf8: Vec<u8>,
    vt_mode_scanner: VtModeScanner,
    vt_input_passthrough: Option<ScopedVtInputPassthrough>,
}

// SAFETY: the writer stores only process console-handle metadata and is moved
// into the single attach output worker. Cross-thread input-mode access is
// serialized by `ATTACH_CONSOLE_IO`; no mutable state is shared directly.
unsafe impl Send for Utf16ConsoleWriter {}

impl Utf16ConsoleWriter {
    fn stdout() -> Option<Self> {
        std_output_handle()
            .and_then(|handle| Self::from_handle(handle, None))
            .or_else(Self::from_conout)
    }

    fn from_conout() -> Option<Self> {
        let file = OpenOptions::new().write(true).open("CONOUT$").ok()?;
        let handle = file.as_raw_handle() as HANDLE;
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        Self::from_handle(handle, Some(file))
    }

    fn from_handle(handle: HANDLE, owned_handle: Option<File>) -> Option<Self> {
        let mode = configure_output_console(handle).ok()?;
        Some(Self {
            handle,
            _owned_handle: owned_handle,
            original_mode: mode,
            pending_utf8: Vec::new(),
            vt_mode_scanner: VtModeScanner::default(),
            vt_input_passthrough: ScopedVtInputPassthrough::for_output(handle),
        })
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

impl Drop for Utf16ConsoleWriter {
    fn drop(&mut self) {
        if let Some(pending) = self.vt_mode_scanner.finish() {
            let _ = self.write_chunk(&pending);
        }
        let _ = restore_output_console(self.handle, self.original_mode);
    }
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
    use super::super::terminal_cleanup::fallback_attach_stop_sequence;
    use super::{writable_utf8_prefix_len, OutputWriteKind, Utf16ConsoleWriter, VtModeScanner};

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
            original_mode: 0,
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
            .map(|chunk| chunk.bytes.as_slice())
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
                scoped.contains(&expected),
                "missing scoped reset: {expected:?}"
            );
        }
    }
}
