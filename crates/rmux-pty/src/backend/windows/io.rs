use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use rmux_os::identity::{IdentityResolver, UserIdentity};
use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE,
    ERROR_HANDLE_EOF, ERROR_PIPE_BUSY, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
};
use windows_sys::Win32::System::Pipes::{
    CreateNamedPipeW, CreatePipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};

const CONPTY_PIPE_CREATE_ATTEMPTS: usize = 8;
const CONPTY_PIPE_NONCE_BYTES: usize = 16;

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
    retry_pipe_creation(|| {
        let mut nonce = [0_u8; CONPTY_PIPE_NONCE_BYTES];
        getrandom::fill(&mut nonce)
            .map_err(|error| io::Error::other(format!("ConPTY pipe nonce failed: {error}")))?;
        create_conpty_input_pipe_named(buffer_size, &conpty_pipe_name(nonce))
    })
}

fn create_conpty_input_pipe_named(buffer_size: u32, name: &[u16]) -> io::Result<PipePair> {
    // ConPTY requires a synchronous pipe handle. The inbound server end is
    // therefore synchronous, while RMUX's client writer is opened for
    // overlapped I/O so a pending write can be cancelled on timeout.
    let mut security = SameUserSecurityAttributes::new()?;
    let read = unsafe {
        // SAFETY: `name` is NUL-terminated and `security` owns the descriptor
        // referenced by its attributes for this call. The returned server
        // handle is uniquely owned on success.
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            buffer_size,
            0,
            security.as_mut_ptr(),
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

fn retry_pipe_creation<T>(mut create: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut collision = None;
    for _ in 0..CONPTY_PIPE_CREATE_ATTEMPTS {
        match create() {
            Ok(value) => return Ok(value),
            Err(error) if is_pipe_name_collision(&error) => collision = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(collision.unwrap_or_else(|| io::Error::other("ConPTY pipe allocation exhausted")))
}

fn is_pipe_name_collision(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32
                || code == ERROR_ALREADY_EXISTS as i32
                || code == ERROR_PIPE_BUSY as i32
    )
}

fn conpty_pipe_name(nonce: [u8; CONPTY_PIPE_NONCE_BYTES]) -> Vec<u16> {
    let mut value = String::with_capacity(32 + nonce.len() * 2);
    value.push_str(r"\\.\pipe\rmux-conpty-input-");
    for byte in nonce {
        use std::fmt::Write as _;
        write!(value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

struct SameUserSecurityAttributes {
    descriptor: PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl SameUserSecurityAttributes {
    fn new() -> io::Result<Self> {
        let sid = match IdentityResolver::current()? {
            UserIdentity::Sid(sid) => sid,
            UserIdentity::Uid(_) => {
                return Err(io::Error::other(
                    "Windows identity resolver returned a Unix uid",
                ));
            }
        };
        let sddl = wide_null(&format!("O:{sid}G:{sid}D:P(A;;GA;;;{sid})"));
        let mut descriptor = null_mut();
        let ok = unsafe {
            // SAFETY: `sddl` is NUL-terminated and `descriptor` is an out
            // pointer released by this type's Drop implementation.
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION,
                &mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.cast(),
                bInheritHandle: 0,
            },
        })
    }

    fn as_mut_ptr(&mut self) -> *mut SECURITY_ATTRIBUTES {
        &mut self.attributes
    }
}

impl Drop for SameUserSecurityAttributes {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                // SAFETY: the descriptor was allocated by
                // ConvertStringSecurityDescriptorToSecurityDescriptorW.
                LocalFree(self.descriptor.cast());
            }
        }
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
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
    fn conpty_pipe_name_is_fixed_width_and_nul_terminated() {
        let name = conpty_pipe_name([0xab; CONPTY_PIPE_NONCE_BYTES]);
        let decoded = String::from_utf16(&name[..name.len() - 1]).expect("valid UTF-16");

        assert_eq!(
            decoded,
            r"\\.\pipe\rmux-conpty-input-abababababababababababababababab"
        );
        assert_eq!(name.last(), Some(&0));
    }

    #[test]
    fn pipe_creation_retries_only_name_collisions() {
        let mut attempts = 0;
        let value = retry_pipe_creation(|| {
            attempts += 1;
            if attempts < 3 {
                Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32))
            } else {
                Ok(42)
            }
        })
        .expect("third allocation succeeds");

        assert_eq!(value, 42);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn pipe_creation_stops_on_non_collision_error() {
        let mut attempts = 0;
        let error = retry_pipe_creation::<()>(|| {
            attempts += 1;
            Err(io::Error::from_raw_os_error(ERROR_BROKEN_PIPE as i32))
        })
        .expect_err("non-collision error must be returned");

        assert_eq!(error.raw_os_error(), Some(ERROR_BROKEN_PIPE as i32));
        assert_eq!(attempts, 1);
    }

    #[test]
    fn read_closed_pipe_reports_eof() {
        let pipe = create_pipe(0).expect("pipe");
        drop(pipe.write);

        let mut buffer = [0_u8; 8];
        let bytes_read = read(&pipe.read, &mut buffer).expect("closed pipe read");

        assert_eq!(bytes_read, 0);
    }
}
