use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null;
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_BROKEN_PIPE, ERROR_HANDLE_EOF, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, CreatePipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

static NEXT_CONPTY_PIPE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct PipePair {
    pub(crate) read: OwnedHandle,
    pub(crate) write: OwnedHandle,
}

pub(crate) fn create_pipe(buffer_size: u32) -> io::Result<PipePair> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: `read` and `write` are valid out-pointers for `CreatePipe`, the
    // security attributes pointer is null by design, and Windows initializes
    // both handles on success.
    let created = unsafe { CreatePipe(&mut read, &mut write, null(), buffer_size) };
    if created == 0 {
        return Err(last_os_error());
    }

    // SAFETY: `CreatePipe` succeeded, so both raw handles are owned by this
    // function and are transferred exactly once into `OwnedHandle`.
    let read = unsafe { OwnedHandle::from_raw_handle(read as _) };
    let write = unsafe { OwnedHandle::from_raw_handle(write as _) };
    Ok(PipePair { read, write })
}

pub(crate) fn create_conpty_input_pipe(buffer_size: u32) -> io::Result<PipePair> {
    let pipe_id = NEXT_CONPTY_PIPE_ID.fetch_add(1, Ordering::Relaxed);
    let process_id = unsafe {
        // SAFETY: `GetCurrentProcessId` has no preconditions.
        GetCurrentProcessId()
    };
    let mut name = format!(r"\\.\pipe\rmux-conpty-input-{process_id}-{pipe_id}")
        .encode_utf16()
        .collect::<Vec<_>>();
    name.push(0);

    // ConPTY requires a synchronous pipe handle. The inbound server end is
    // therefore synchronous, while RMUX's client writer is opened for
    // overlapped I/O so a pending write can be cancelled on timeout.
    let read = unsafe {
        // SAFETY: `name` is NUL-terminated and all optional security
        // attributes are intentionally omitted. The returned server handle is
        // uniquely owned on success.
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            buffer_size,
            0,
            null(),
        )
    };
    if read == INVALID_HANDLE_VALUE {
        return Err(last_os_error());
    }
    // SAFETY: `CreateNamedPipeW` returned a uniquely owned live handle.
    let read = unsafe { OwnedHandle::from_raw_handle(read as _) };

    let write = unsafe {
        // SAFETY: `name` remains NUL-terminated for the call. The server
        // instance above already exists, and this handle is opened only for
        // local overlapped writes.
        CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            std::ptr::null_mut(),
        )
    };
    if write == INVALID_HANDLE_VALUE {
        return Err(last_os_error());
    }
    // SAFETY: `CreateFileW` returned a uniquely owned live handle.
    let write = unsafe { OwnedHandle::from_raw_handle(write as _) };

    Ok(PipePair { read, write })
}

pub(crate) fn read(handle: &OwnedHandle, buffer: &mut [u8]) -> io::Result<usize> {
    let len = u32::try_from(buffer.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "read buffer exceeds Windows DWORD length",
        )
    })?;
    let mut bytes_read = 0_u32;
    // SAFETY: `handle` is a live owned Windows handle, `buffer` is writable for
    // `len` bytes, and the synchronous call writes the byte count to a valid
    // stack pointer.
    let ok = unsafe {
        ReadFile(
            handle.as_raw_handle() as HANDLE,
            buffer.as_mut_ptr().cast(),
            len,
            &mut bytes_read,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = unsafe {
            // SAFETY: `GetLastError` reads the calling thread's last-error slot.
            GetLastError()
        };
        if matches!(error, ERROR_BROKEN_PIPE | ERROR_HANDLE_EOF) {
            return Ok(0);
        }
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(bytes_read as usize)
}

fn last_os_error() -> io::Error {
    // SAFETY: `GetLastError` reads the calling thread's last-error slot and has
    // no preconditions.
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_closed_pipe_reports_eof() {
        let pipe = create_pipe(0).expect("pipe");
        drop(pipe.write);

        let mut buffer = [0_u8; 8];
        let bytes_read = read(&pipe.read, &mut buffer).expect("closed pipe read");

        assert_eq!(bytes_read, 0);
    }
}
