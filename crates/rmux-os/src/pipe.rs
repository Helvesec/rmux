//! Safe Windows anonymous-pipe readiness probes.

use std::io;
use std::os::windows::io::AsRawHandle;
use std::process::ChildStdout;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, HANDLE,
};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;

/// Nonblocking readiness state for a child-process stdout pipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildStdoutReadiness {
    /// The writer is still connected but no bytes are currently buffered.
    Pending,
    /// Every writer has closed its handle.
    Closed,
    /// At least this many bytes can be read without blocking.
    Bytes(usize),
}

/// Reports buffered bytes or closure without consuming child stdout.
pub fn child_stdout_readiness(stdout: &ChildStdout) -> io::Result<ChildStdoutReadiness> {
    let mut available = 0_u32;
    let peeked = unsafe {
        // SAFETY: `stdout` owns a live anonymous-pipe read handle and
        // `available` is a valid out pointer. Anonymous Windows pipes support
        // PeekNamedPipe, which does not consume their contents.
        PeekNamedPipe(
            stdout.as_raw_handle() as HANDLE,
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        )
    };
    if peeked == 0 {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error(),
            Some(code)
                if code == ERROR_BROKEN_PIPE as i32
                    || code == ERROR_PIPE_NOT_CONNECTED as i32
                    || code == ERROR_NO_DATA as i32
        ) {
            return Ok(ChildStdoutReadiness::Closed);
        }
        return Err(error);
    }
    if available == 0 {
        return Ok(ChildStdoutReadiness::Pending);
    }

    Ok(ChildStdoutReadiness::Bytes(
        usize::try_from(available).unwrap_or(usize::MAX),
    ))
}
