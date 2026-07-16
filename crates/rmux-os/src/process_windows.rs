use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::PathBuf;
use std::ptr::null;

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

const ERROR_PARTIAL_COPY: i32 = 299;
const ERROR_INVALID_ADDRESS: i32 = 487;
const ERROR_NOACCESS: i32 = 998;
const MAX_ENVIRONMENT_WIDE_CHARS: usize = 32 * 1024;
const ENVIRONMENT_READ_CHUNK_WIDE_CHARS: usize = 2048;
const MAX_REMOTE_UNICODE_STRING_BYTES: usize = u16::MAX as usize - 1;

/// Windows job object assigned to a child process so descendants can be
/// terminated as one process tree.
pub struct ProcessJob {
    handle: OwnedHandle,
}

impl ProcessJob {
    /// Creates a kill-on-close job object and assigns `child` to it.
    pub fn for_child(child: &impl AsRawHandle) -> io::Result<Self> {
        Self::for_raw_handle_with_breakaway(child.as_raw_handle(), false)
    }

    /// Creates a kill-on-close job that permits explicitly detached children.
    ///
    /// Ordinary descendants stay in the job. A child escapes only when it is
    /// itself created with `CREATE_BREAKAWAY_FROM_JOB`, which is required by
    /// RMUX's independent daemon bootstrap on Windows.
    pub fn for_child_allowing_breakaway(child: &impl AsRawHandle) -> io::Result<Self> {
        Self::for_raw_handle_with_breakaway(child.as_raw_handle(), true)
    }

    /// Creates a kill-on-close job object and assigns a live process handle to it.
    pub fn for_raw_handle(process_handle: RawHandle) -> io::Result<Self> {
        Self::for_raw_handle_with_breakaway(process_handle, false)
    }

    fn for_raw_handle_with_breakaway(
        process_handle: RawHandle,
        allow_breakaway: bool,
    ) -> io::Result<Self> {
        if process_handle.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process handle is null",
            ));
        }
        let handle = create_job_object(allow_breakaway)?;
        // SAFETY: Both handles are live for the duration of the call. The API
        // associates the process with the job without taking ownership.
        let ok = unsafe {
            AssignProcessToJobObject(handle.as_raw_handle() as HANDLE, process_handle as HANDLE)
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    /// Terminates every process currently assigned to this job object.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: `self.handle` is a live job handle owned by this guard; the
        // API does not take ownership of it.
        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, exit_code) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Clears the kill-on-close policy while keeping the process assigned to
    /// the job until this handle is dropped.
    ///
    /// This is used after a foreground child exits normally so descendants it
    /// intentionally left in the background retain standard shell semantics.
    pub fn disarm_kill_on_close(&self) -> io::Result<()> {
        set_job_limit_flags(&self.handle, 0)
    }
}

fn create_job_object(allow_breakaway: bool) -> io::Result<OwnedHandle> {
    // SAFETY: Null security attributes and name request the default unnamed
    // job object. The returned handle is checked before ownership transfer.
    let handle = unsafe { CreateJobObjectW(null(), null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `CreateJobObjectW` returned a non-null owned handle and this
    // function transfers it exactly once into `OwnedHandle`.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle as _) };
    set_job_limit_flags(&handle, job_limit_flags(allow_breakaway))?;
    Ok(handle)
}

const fn job_limit_flags(allow_breakaway: bool) -> u32 {
    if allow_breakaway {
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK
    } else {
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    }
}

fn set_job_limit_flags(handle: &OwnedHandle, flags: u32) -> io::Result<()> {
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = flags;
    // SAFETY: `handle` is a live job handle, `limits` points to an initialized
    // structure of the declared size, and the API borrows it only for the call.
    let ok = unsafe {
        SetInformationJobObject(
            handle.as_raw_handle() as HANDLE,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn current_path(pid: u32) -> io::Result<Option<String>> {
    let Some(process) = RemoteProcess::open_for_query_and_read(pid)? else {
        return Ok(None);
    };
    let Some(parameters) = process.process_parameters()? else {
        return Ok(None);
    };
    Ok(process
        .read_unicode_string(parameters.current_directory.dos_path)?
        .map(trim_trailing_current_directory_separator))
}

pub(super) fn parent_pid(pid: u32) -> io::Result<Option<u32>> {
    let handle = unsafe {
        // SAFETY: OpenProcess validates the pid and returns either a handle or null.
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid)
    };
    if handle.is_null() {
        return unavailable_or_error(io::Error::last_os_error());
    }
    let _guard = WindowsHandle(handle);
    let Some(info) = query_basic_information(handle)? else {
        return Ok(None);
    };
    if info.inherited_from_unique_process_id == 0 {
        return Ok(None);
    }
    let parent = u32::try_from(info.inherited_from_unique_process_id)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "parent pid out of range"))?;
    Ok(Some(parent))
}

pub(super) fn command_name(pid: u32) -> io::Result<Option<String>> {
    Ok(executable_path(pid)?.and_then(|path| super::executable_name(&path)))
}

pub(super) fn executable_path(pid: u32) -> io::Result<Option<String>> {
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
    Ok(Some(wide_to_string_lossy(&buffer)))
}

pub(super) fn descendant_command_names(pid: u32) -> io::Result<Vec<String>> {
    let entries = process_snapshot_entries()?;
    let mut pending = vec![pid];
    let mut names = Vec::new();
    let mut seen = std::collections::HashSet::new();
    seen.insert(pid);

    while let Some(parent) = pending.pop() {
        for entry in entries
            .iter()
            .filter(|entry| entry.parent_pid == parent && seen.insert(entry.pid))
        {
            names.push(entry.name.clone());
            pending.push(entry.pid);
        }
    }

    Ok(names)
}

pub(super) fn fd_path(_pid: u32, _fd: i32) -> io::Result<Option<PathBuf>> {
    Ok(None)
}

fn trim_trailing_current_directory_separator(mut path: String) -> String {
    while path.ends_with(['\\', '/']) && !is_windows_root_path(&path) {
        path.pop();
    }
    path
}

fn is_windows_root_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() == 3 && bytes[1] == b':' && is_separator(bytes[2]) {
        return bytes[0].is_ascii_alphabetic();
    }
    if !path.starts_with(r"\\") {
        return false;
    }
    let without_trailing = path.trim_end_matches(['\\', '/']);
    let Some(rest) = without_trailing.get(2..) else {
        return false;
    };
    rest.split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .count()
        == 2
}

fn is_separator(byte: u8) -> bool {
    byte == b'\\' || byte == b'/'
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
        query_basic_information(self.handle)
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
        let Some(byte_len) = validate_remote_unicode_string(value) else {
            return Ok(None);
        };
        let units = byte_len / size_of::<u16>();
        let mut buffer = vec![0_u16; units];
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
        let mut offset = 0_usize;
        while offset < MAX_ENVIRONMENT_WIDE_CHARS {
            let units = ENVIRONMENT_READ_CHUNK_WIDE_CHARS
                .min(MAX_ENVIRONMENT_WIDE_CHARS.saturating_sub(offset));
            let byte_offset = offset
                .checked_mul(size_of::<u16>())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "environment offset"))?;
            let chunk_address = address
                .checked_add(byte_offset)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "environment address"))?;
            let Some(chunk) = self.read_environment_chunk(chunk_address, units)? else {
                return Ok(None);
            };

            let read_units = chunk.len();
            block.extend_from_slice(&chunk);
            if let Some(end) = environment_block_end(&block) {
                block.truncate(end);
                return Ok(Some(block));
            }
            offset += read_units;
        }

        Ok(None)
    }

    fn read_environment_chunk(
        &self,
        address: usize,
        max_units: usize,
    ) -> io::Result<Option<Vec<u16>>> {
        let mut units = max_units;
        while units > 0 {
            let mut chunk = vec![0_u16; units];
            let byte_len = units
                .checked_mul(size_of::<u16>())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "environment length"))?;
            if self
                .read_exact(address, chunk.as_mut_ptr().cast(), byte_len)?
                .is_some()
            {
                return Ok(Some(chunk));
            }
            if units == 1 {
                return Ok(None);
            }
            units /= 2;
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

fn query_basic_information(handle: HANDLE) -> io::Result<Option<ProcessBasicInformationRecord>> {
    let mut info = MaybeUninit::<ProcessBasicInformationRecord>::zeroed();
    let len = u32::try_from(size_of::<ProcessBasicInformationRecord>())
        .map_err(|_| io::ErrorKind::InvalidData)?;
    let mut returned = 0_u32;
    let status = unsafe {
        // SAFETY: `info` points to writable memory sized by `len`.
        NtQueryInformationProcess(
            handle,
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

struct ProcessSnapshotEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
}

fn process_snapshot_entries() -> io::Result<Vec<ProcessSnapshotEntry>> {
    let handle = unsafe {
        // SAFETY: The flags request a read-only process snapshot and the pid
        // argument is ignored for TH32CS_SNAPPROCESS.
        CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _guard = WindowsHandle(handle);

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    let mut entries = Vec::new();
    let mut ok = unsafe {
        // SAFETY: `entry` is initialized and its dwSize field is set as
        // required by the ToolHelp API.
        Process32FirstW(handle, &mut entry)
    };
    while ok != 0 {
        entries.push(ProcessSnapshotEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            name: wide_nul_to_string(&entry.szExeFile),
        });
        ok = unsafe {
            // SAFETY: `entry` remains valid and initialized across iterations.
            Process32NextW(handle, &mut entry)
        };
    }

    Ok(entries)
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

fn validate_remote_unicode_string(value: RemoteUnicodeString) -> Option<usize> {
    let byte_len = usize::from(value.length);
    let max_byte_len = usize::from(value.maximum_length);
    if byte_len == 0 || value.buffer == 0 {
        return None;
    }
    if !byte_len.is_multiple_of(size_of::<u16>())
        || !max_byte_len.is_multiple_of(size_of::<u16>())
        || !value.buffer.is_multiple_of(size_of::<u16>())
    {
        return None;
    }
    if byte_len > max_byte_len || byte_len > MAX_REMOTE_UNICODE_STRING_BYTES {
        return None;
    }
    Some(byte_len)
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

fn environment_block_end(block: &[u16]) -> Option<usize> {
    block
        .windows(2)
        .position(|pair| pair[0] == 0 && pair[1] == 0)
        .map(|index| index + 2)
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

fn wide_nul_to_string(value: &[u16]) -> String {
    let len = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    wide_to_string_lossy(&value[..len])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, VirtualProtect, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
        PAGE_NOACCESS, PAGE_READWRITE,
    };
    #[cfg(windows)]
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    #[test]
    fn process_job_breakaway_is_explicit_and_preserves_kill_on_close() {
        assert_eq!(job_limit_flags(false), JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE);
        assert_eq!(
            job_limit_flags(true),
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK
        );
    }

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
    fn finds_environment_block_end_across_chunk_boundaries() {
        let mut block = vec![b'A' as u16; ENVIRONMENT_READ_CHUNK_WIDE_CHARS - 1];
        block.push(0);
        assert_eq!(environment_block_end(&block), None);

        block.push(0);
        assert_eq!(
            environment_block_end(&block),
            Some(ENVIRONMENT_READ_CHUNK_WIDE_CHARS + 1)
        );
    }

    #[cfg(windows)]
    #[test]
    fn reads_environment_block_ending_before_an_unreadable_page() {
        const PAGE_SIZE: usize = 4096;
        let allocation = unsafe {
            // SAFETY: VirtualAlloc is called with a null preferred address and
            // reserves/commits two pages for this test process.
            VirtualAlloc(
                std::ptr::null_mut(),
                PAGE_SIZE * 2,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        assert!(!allocation.is_null(), "VirtualAlloc failed");
        let _allocation = VirtualAllocation(allocation);

        let second_page = unsafe {
            // SAFETY: The allocation above covers two pages, so adding one
            // page yields the start of the second page.
            allocation.cast::<u8>().add(PAGE_SIZE).cast()
        };
        let mut old_protect = 0_u32;
        let protected = unsafe {
            // SAFETY: `second_page` identifies the second committed page and
            // `old_protect` is a valid out pointer.
            VirtualProtect(second_page, PAGE_SIZE, PAGE_NOACCESS, &mut old_protect)
        };
        assert_ne!(protected, 0, "VirtualProtect failed");

        let environment: Vec<u16> = "A=B\0\0".encode_utf16().collect();
        let start = unsafe {
            // SAFETY: The environment fits entirely in the first page; it is
            // deliberately placed so an initial 4 KiB read crosses into the
            // protected page.
            allocation
                .cast::<u8>()
                .add(PAGE_SIZE - environment.len() * size_of::<u16>())
                .cast::<u16>()
        };
        unsafe {
            // SAFETY: `start` is valid for `environment.len()` UTF-16 units
            // inside the writable first page.
            std::ptr::copy_nonoverlapping(environment.as_ptr(), start, environment.len());
        }

        let handle = unsafe {
            // SAFETY: OpenProcess validates the current pid and returns either
            // a handle or null.
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                0,
                GetCurrentProcessId(),
            )
        };
        assert!(!handle.is_null(), "OpenProcess failed");
        let process = RemoteProcess { handle };

        let block = process
            .read_environment_block(start as usize)
            .expect("read environment")
            .expect("environment block");

        assert_eq!(block, environment);
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

    #[test]
    fn validates_remote_unicode_string_before_remote_reads() {
        let valid = RemoteUnicodeString {
            length: 4,
            maximum_length: 6,
            buffer: 0x1000,
        };
        assert_eq!(validate_remote_unicode_string(valid), Some(4));

        for value in [
            RemoteUnicodeString {
                length: 0,
                maximum_length: 6,
                buffer: 0x1000,
            },
            RemoteUnicodeString {
                length: 3,
                maximum_length: 6,
                buffer: 0x1000,
            },
            RemoteUnicodeString {
                length: 4,
                maximum_length: 3,
                buffer: 0x1000,
            },
            RemoteUnicodeString {
                length: 4,
                maximum_length: 6,
                buffer: 0,
            },
            RemoteUnicodeString {
                length: 4,
                maximum_length: 6,
                buffer: 0x1001,
            },
        ] {
            assert_eq!(validate_remote_unicode_string(value), None);
        }
    }

    #[test]
    fn trims_trailing_current_directory_separator_without_stripping_roots() {
        assert_eq!(
            trim_trailing_current_directory_separator(r"C:\Users\User\".to_owned()),
            r"C:\Users\User"
        );
        assert_eq!(
            trim_trailing_current_directory_separator(r"C:\".to_owned()),
            r"C:\"
        );
        assert_eq!(
            trim_trailing_current_directory_separator(r"\\server\share\project\".to_owned()),
            r"\\server\share\project"
        );
        assert_eq!(
            trim_trailing_current_directory_separator(r"\\server\share\".to_owned()),
            r"\\server\share\"
        );
        assert_eq!(
            trim_trailing_current_directory_separator("\\".to_owned()),
            r""
        );
        assert_eq!(
            trim_trailing_current_directory_separator("\\\\".to_owned()),
            r""
        );
        assert_eq!(
            trim_trailing_current_directory_separator("\\/".to_owned()),
            r""
        );
    }

    #[cfg(windows)]
    struct VirtualAllocation(*mut c_void);

    #[cfg(windows)]
    impl Drop for VirtualAllocation {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: `self.0` came from VirtualAlloc and is released once
                // with MEM_RELEASE.
                VirtualFree(self.0, 0, MEM_RELEASE);
            }
        }
    }
}
