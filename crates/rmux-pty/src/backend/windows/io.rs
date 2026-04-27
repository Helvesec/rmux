use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null;

use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::CreatePipe;

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
        return Err(last_os_error());
    }
    Ok(bytes_read as usize)
}

pub(crate) fn write_all(handle: &OwnedHandle, mut buffer: &[u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        let len = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let mut bytes_written = 0_u32;
        // SAFETY: `handle` is a live owned Windows handle, `buffer` is readable
        // for `len` bytes, and the synchronous call writes the byte count to a
        // valid stack pointer.
        let ok = unsafe {
            WriteFile(
                handle.as_raw_handle() as HANDLE,
                buffer.as_ptr().cast(),
                len,
                &mut bytes_written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        if bytes_written == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
        }
        buffer = &buffer[bytes_written as usize..];
    }
    Ok(())
}

fn last_os_error() -> io::Error {
    // SAFETY: `GetLastError` reads the calling thread's last-error slot and has
    // no preconditions.
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}
