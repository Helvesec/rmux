//! Complete the companion console's own DA1 negotiation before exposing its PTY.
use std::collections::VecDeque;
use std::io;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, HANDLE};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

use super::bounded_write::OverlappedPipeWriter;

const REQUEST: &[u8] = b"\x1b[c";
const PREFIX: &[u8] = b"\x1b[c\x1b[?1004h\x1b[?9001h";
const VISIBLE_PREFIX: &[u8] = b"\x1b[1t\x1b[c\x1b[?1004h\x1b[?9001h";
// No optional capabilities: same conservative feature set as a DA1 timeout.
const RESPONSE: &[u8] = b"\x1b[?1;0c";
const LIMIT: usize = 4096;

pub(super) fn negotiate(
    output: &OwnedHandle,
    input: &OverlappedPipeWriter,
) -> io::Result<VecDeque<u8>> {
    // Microsoft ConPTY 1.24 waits 3000ms in VtIo::StartIfNeeded before the
    // client can run. This function is called after ResumeThread and before
    // spawn returns, with exclusive ownership of the unread output pipe.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    loop {
        match classify_prefix(&bytes) {
            Prefix::Request(index) => {
                input.write_all_with_timeout(RESPONSE, Duration::from_secs(1))?;
                tracing::debug!(target: "rmux::conpty", prefix_bytes = index + PREFIX.len(), query_offset = index, "completed native ConPTY startup DA1");
                bytes.drain(index..index + REQUEST.len());
                return Ok(bytes.into());
            }
            Prefix::Unknown => return Ok(bytes.into()),
            Prefix::Incomplete => {}
        }
        if Instant::now() >= deadline || bytes.len() == LIMIT {
            // A slow or different runtime still gets a usable pane: let its
            // ordinary reader handle any later query, without injecting a reply
            // or discarding the prefix we have already consumed.
            tracing::warn!(target: "rmux::conpty", "ConPTY startup prefix incomplete; preserving output");
            return Ok(bytes.into());
        }
        let mut available = 0;
        // SAFETY: output is an owned synchronous pipe. Only query the available
        // byte count; null optional pointers do not receive any data.
        if unsafe {
            PeekNamedPipe(
                output.as_raw_handle() as HANDLE,
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                return Ok(bytes.into());
            }
            return Err(error);
        }
        if available == 0 {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        let mut buffer = [0_u8; LIMIT];
        let len = (available as usize).min(LIMIT - bytes.len());
        // No other reader exists before spawn returns, so these peeked bytes
        // cannot disappear and this synchronous read cannot wait for more.
        let read = super::io::read(output, &mut buffer[..len])?;
        if read == 0 {
            return Ok(bytes.into());
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
}

#[derive(Debug, PartialEq)]
enum Prefix {
    Incomplete,
    Unknown,
    Request(usize),
}

fn classify_prefix(bytes: &[u8]) -> Prefix {
    for (pattern, offset) in [(PREFIX, 0), (VISIBLE_PREFIX, 4)] {
        if bytes.starts_with(pattern) {
            return Prefix::Request(offset);
        }
    }
    if PREFIX.starts_with(bytes) || VISIBLE_PREFIX.starts_with(bytes) {
        Prefix::Incomplete
    } else {
        Prefix::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_match_requires_complete_native_preamble_and_preserves_app_queries() {
        for (pattern, offset) in [(PREFIX, 0), (VISIBLE_PREFIX, 4)] {
            for split in 0..pattern.len() {
                assert_eq!(classify_prefix(&pattern[..split]), Prefix::Incomplete);
            }
            let app = b"APP\x1b[c\x1b]10;?\x07END";
            let mut bytes = [pattern, app].concat();
            assert_eq!(classify_prefix(&bytes), Prefix::Request(offset));
            bytes.drain(offset..offset + REQUEST.len());
            assert!(bytes.ends_with(app));
        }
        assert_eq!(classify_prefix(b"APP\x1b[c"), Prefix::Unknown);
        assert_eq!(classify_prefix(b"\x1b[cAPP"), Prefix::Unknown);
        assert_eq!(classify_prefix(b"\x1b]10;?\x07"), Prefix::Unknown);
    }
}
