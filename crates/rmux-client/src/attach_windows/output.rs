use std::io::{self, Write};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, WriteConsoleW, STD_OUTPUT_HANDLE,
};

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
    pending_utf8: Vec<u8>,
}

// SAFETY: the writer owns only process std-output handle metadata and is moved
// into the single attach stream loop; it does not share mutable state between
// threads.
unsafe impl Send for Utf16ConsoleWriter {}

impl Utf16ConsoleWriter {
    fn stdout() -> Option<Self> {
        let handle = unsafe {
            // SAFETY: GetStdHandle accepts the documented STD_* constants.
            GetStdHandle(STD_OUTPUT_HANDLE)
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut mode = 0;
        let ok = unsafe {
            // SAFETY: handle is borrowed and mode points to writable storage.
            GetConsoleMode(handle, &mut mode)
        };
        if ok == 0 {
            return None;
        }
        Some(Self {
            handle,
            pending_utf8: Vec::new(),
        })
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pending_utf8.extend_from_slice(bytes);
        let valid_len = writable_utf8_prefix_len(&self.pending_utf8);
        if valid_len == 0 {
            return Ok(());
        }

        let text = String::from_utf8_lossy(&self.pending_utf8[..valid_len]);
        self.write_text(&text)?;
        self.pending_utf8.drain(..valid_len);
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending_utf8.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.pending_utf8);
        self.write_text(&text)?;
        self.pending_utf8.clear();
        Ok(())
    }

    fn write_text(&self, text: &str) -> io::Result<()> {
        let wide = text.encode_utf16().collect::<Vec<_>>();
        let mut written = 0;
        while written < wide.len() {
            let chunk = &wide[written..];
            let chunk_len = u32::try_from(chunk.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
            let mut chars_written = 0;
            let ok = unsafe {
                // SAFETY: handle is a live console output handle; chunk points
                // to initialized UTF-16 code units for chunk_len characters.
                WriteConsoleW(
                    self.handle,
                    chunk.as_ptr().cast(),
                    chunk_len,
                    &mut chars_written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            if chars_written == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "WriteConsoleW wrote zero UTF-16 code units",
                ));
            }
            written += usize::try_from(chars_written).map_err(|_| io::ErrorKind::InvalidData)?;
        }
        Ok(())
    }
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
    use super::writable_utf8_prefix_len;

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
}
