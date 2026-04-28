use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::path::PathBuf;

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

const ERROR_PARTIAL_COPY: i32 = 299;
const ERROR_INVALID_ADDRESS: i32 = 487;
const ERROR_NOACCESS: i32 = 998;
const MAX_ENVIRONMENT_WIDE_CHARS: usize = 32 * 1024;

pub(super) fn current_path(pid: u32) -> io::Result<Option<String>> {
    let Some(process) = RemoteProcess::open_for_query_and_read(pid)? else {
        return Ok(None);
    };
    let Some(parameters) = process.process_parameters()? else {
        return Ok(None);
    };
    process.read_unicode_string(parameters.current_directory.dos_path)
}

pub(super) fn command_name(pid: u32) -> io::Result<Option<String>> {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        return unavailable_or_error(io::Error::last_os_error());
    }
    let _guard = WindowsHandle(handle);

    let mut buffer = vec![0_u16; 32_768];
    let mut len = u32::try_from(buffer.len()).map_err(|_| io::ErrorKind::InvalidData)?;
    let ok = unsafe {
        // SAFETY: `buffer` is writable for `len` UTF-16 code units.
        QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut len)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(usize::try_from(len).map_err(|_| io::ErrorKind::InvalidData)?);
    let path = wide_to_string_lossy(&buffer);
    Ok(super::executable_name(&path))
}

pub(super) fn fd_path(_pid: u32, _fd: i32) -> io::Result<Option<PathBuf>> {
    Ok(None)
}

pub(super) fn is_live(pid: u32) -> io::Result<Option<bool>> {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(SYNCHRONIZE, 0, pid)
    };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => Ok(Some(false)),
            Some(code) if code == ERROR_ACCESS_DENIED as i32 => Ok(None),
            _ => Err(error),
        };
    }
    let _guard = WindowsHandle(handle);

    let wait = unsafe {
        // SAFETY: `handle` is a live process handle and a zero timeout only observes state.
        WaitForSingleObject(handle, 0)
    };
    match wait {
        WAIT_TIMEOUT => Ok(Some(true)),
        WAIT_OBJECT_0 => Ok(Some(false)),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(io::Error::other("unexpected Windows process wait result")),
    }
}

pub(super) fn environment(pid: u32) -> io::Result<Option<HashMap<String, String>>> {
    let Some(process) = RemoteProcess::open_for_query_and_read(pid)? else {
        return Ok(None);
    };
    let Some(parameters) = process.process_parameters()? else {
        return Ok(None);
    };
    let Some(block) = process.read_environment_block(parameters.environment)? else {
        return Ok(None);
    };
    Ok(environment_from_wide_block(&block))
}

struct RemoteProcess {
    handle: HANDLE,
}

impl RemoteProcess {
    fn open_for_query_and_read(pid: u32) -> io::Result<Option<Self>> {
        let handle = unsafe {
            // SAFETY: OpenProcess validates the pid and returns either a handle or null.
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid)
        };
        if handle.is_null() {
            return unavailable_or_error(io::Error::last_os_error());
        }
        Ok(Some(Self { handle }))
    }

    fn process_parameters(&self) -> io::Result<Option<RtlUserProcessParametersPrefix>> {
        let Some(basic) = self.query_basic_information()? else {
            return Ok(None);
        };
        let Some(peb) = self.read_struct::<PebPrefix>(basic.peb_base_address)? else {
            return Ok(None);
        };
        self.read_struct::<RtlUserProcessParametersPrefix>(peb.process_parameters)
    }

    fn query_basic_information(&self) -> io::Result<Option<ProcessBasicInformationRecord>> {
        let mut info = MaybeUninit::<ProcessBasicInformationRecord>::zeroed();
        let len = u32::try_from(size_of::<ProcessBasicInformationRecord>())
            .map_err(|_| io::ErrorKind::InvalidData)?;
        let mut returned = 0_u32;
        let status = unsafe {
            // SAFETY: `info` points to writable memory sized by `len`.
            NtQueryInformationProcess(
                self.handle,
                ProcessBasicInformation,
                info.as_mut_ptr().cast(),
                len,
                &mut returned,
            )
        };
        if status < 0 {
            return Ok(None);
        }
        Ok(Some(unsafe {
            // SAFETY: NtQueryInformationProcess succeeded and initialized `info`.
            info.assume_init()
        }))
    }

    fn read_struct<T: Copy>(&self, address: usize) -> io::Result<Option<T>> {
        if address == 0 {
            return Ok(None);
        }
        let mut value = MaybeUninit::<T>::uninit();
        let Some(()) = self.read_exact(address, value.as_mut_ptr().cast(), size_of::<T>())? else {
            return Ok(None);
        };
        Ok(Some(unsafe {
            // SAFETY: `read_exact` filled the whole `T` byte range.
            value.assume_init()
        }))
    }

    fn read_unicode_string(&self, value: RemoteUnicodeString) -> io::Result<Option<String>> {
        if value.length == 0 || value.buffer == 0 || !value.length.is_multiple_of(2) {
            return Ok(None);
        }
        let units = usize::from(value.length) / 2;
        let mut buffer = vec![0_u16; units];
        let byte_len = units
            .checked_mul(size_of::<u16>())
            .ok_or(io::ErrorKind::InvalidData)?;
        let Some(()) = self.read_exact(value.buffer, buffer.as_mut_ptr().cast(), byte_len)? else {
            return Ok(None);
        };
        Ok(Some(wide_to_string_lossy(&buffer)))
    }

    fn read_environment_block(&self, address: usize) -> io::Result<Option<Vec<u16>>> {
        if address == 0 {
            return Ok(None);
        }

        let mut block = Vec::new();
        let mut previous_was_nul = false;
        for index in 0..MAX_ENVIRONMENT_WIDE_CHARS {
            let Some(unit) = self.read_struct::<u16>(address + index * size_of::<u16>())? else {
                return Ok(None);
            };
            block.push(unit);
            if unit == 0 {
                if previous_was_nul {
                    return Ok(Some(block));
                }
                previous_was_nul = true;
            } else {
                previous_was_nul = false;
            }
        }

        Ok(None)
    }

    fn read_exact(
        &self,
        address: usize,
        buffer: *mut c_void,
        byte_len: usize,
    ) -> io::Result<Option<()>> {
        if address == 0 || byte_len == 0 {
            return Ok(None);
        }
        let mut bytes_read = 0_usize;
        let ok = unsafe {
            // SAFETY: The destination buffer is valid for `byte_len`; the remote pointer is
            // read-only and failures are reported by the OS without writing past the buffer.
            ReadProcessMemory(
                self.handle,
                address as *const c_void,
                buffer,
                byte_len,
                &mut bytes_read,
            )
        };
        if ok == 0 {
            return unavailable_or_error(io::Error::last_os_error());
        }
        Ok((bytes_read == byte_len).then_some(()))
    }
}

impl Drop for RemoteProcess {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: `handle` is owned by this RemoteProcess and came from OpenProcess.
            CloseHandle(self.handle);
        }
    }
}

struct WindowsHandle(HANDLE);

impl Drop for WindowsHandle {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: `self.0` is a handle returned by a successful Win32 call.
            CloseHandle(self.0);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessBasicInformationRecord {
    exit_status: isize,
    peb_base_address: usize,
    affinity_mask: usize,
    base_priority: isize,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PebPrefix {
    reserved1: [u8; 2],
    being_debugged: u8,
    reserved2: u8,
    reserved3: [usize; 2],
    loader_data: usize,
    process_parameters: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RtlUserProcessParametersPrefix {
    maximum_length: u32,
    length: u32,
    flags: u32,
    debug_flags: u32,
    console_handle: usize,
    console_flags: u32,
    standard_input: usize,
    standard_output: usize,
    standard_error: usize,
    current_directory: CurDir,
    dll_path: RemoteUnicodeString,
    image_path_name: RemoteUnicodeString,
    command_line: RemoteUnicodeString,
    environment: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CurDir {
    dos_path: RemoteUnicodeString,
    handle: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RemoteUnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: usize,
}

fn environment_from_wide_block(block: &[u16]) -> Option<HashMap<String, String>> {
    let mut environment = HashMap::new();
    for entry in block.split(|unit| *unit == 0) {
        if entry.is_empty() {
            break;
        }
        let entry = wide_to_string_lossy(entry);
        if entry.starts_with('=') {
            continue;
        }
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if !name.is_empty() {
            environment.insert(name.to_owned(), value.to_owned());
        }
    }
    Some(environment)
}

fn unavailable_or_error<T>(error: io::Error) -> io::Result<Option<T>> {
    match error.raw_os_error() {
        Some(code)
            if code == ERROR_ACCESS_DENIED as i32
                || code == ERROR_INVALID_PARAMETER as i32
                || code == ERROR_PARTIAL_COPY
                || code == ERROR_INVALID_ADDRESS
                || code == ERROR_NOACCESS =>
        {
            Ok(None)
        }
        _ => Err(error),
    }
}

fn wide_to_string_lossy(value: &[u16]) -> String {
    String::from_utf16_lossy(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_wide_environment_and_skips_drive_pseudo_vars() {
        let block: Vec<u16> = "=C:=C:\\rmux\0RMUX_PANE=%4\0Path=C:\\Windows\0\0"
            .encode_utf16()
            .collect();

        let environment = environment_from_wide_block(&block).expect("environment");

        assert_eq!(environment.get("RMUX_PANE").map(String::as_str), Some("%4"));
        assert_eq!(
            environment.get("Path").map(String::as_str),
            Some("C:\\Windows")
        );
        assert!(!environment.contains_key(""));
    }

    #[test]
    fn reports_empty_environment_block() {
        let block: Vec<u16> = "\0\0".encode_utf16().collect();

        let environment = environment_from_wide_block(&block).expect("environment");

        assert!(environment.is_empty());
    }

    #[test]
    fn parses_environment_entries_lossily_when_windows_returns_invalid_utf16() {
        let mut block: Vec<u16> = "RMUX_BAD=".encode_utf16().collect();
        block.push(0xD800);
        block.extend("X\0\0".encode_utf16());

        let environment = environment_from_wide_block(&block).expect("environment");

        assert_eq!(
            environment.get("RMUX_BAD").map(String::as_str),
            Some("\u{FFFD}X")
        );
    }
}
