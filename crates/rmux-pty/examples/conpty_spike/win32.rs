//! Win32 primitives of the spike: pseudoconsole and attribute list owners,
//! pipe pairs, and child process creation and waiting.

use std::ffi::OsString;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};

use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, PipeMode, ServerOptions,
};
use windows_sys::Win32::Foundation::{GetLastError, HANDLE, S_OK, WAIT_FAILED, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    GetProcessHandleCount, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
};

pub(crate) struct BlockingPipePair {
    pub(crate) read: OwnedHandle,
    pub(crate) write: OwnedHandle,
}

pub(crate) fn anonymous_blocking_pipe() -> io::Result<BlockingPipePair> {
    let mut read = null_mut();
    let mut write = null_mut();
    let created = unsafe { CreatePipe(&mut read, &mut write, null(), 64 * 1024) };
    if created == 0 {
        return Err(last_os_error());
    }

    let read = unsafe { OwnedHandle::from_raw_handle(read as _) };
    let write = unsafe { OwnedHandle::from_raw_handle(write as _) };
    Ok(BlockingPipePair { read, write })
}

pub(crate) struct PipePair {
    pub(crate) server: NamedPipeServer,
    pub(crate) client: NamedPipeClient,
}

#[derive(Clone, Copy)]
pub(crate) enum PipeDirection {
    ClientToServer,
    ServerToClient,
}

pub(crate) fn anonymous_overlapped_pipe(direction: PipeDirection) -> io::Result<PipePair> {
    let name = format!(
        r"\\.\pipe\rmux-conpty-spike-{}-{}",
        std::process::id(),
        unique_suffix()
    );
    let mut options = ServerOptions::new();
    options
        .pipe_mode(PipeMode::Byte)
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .in_buffer_size(64 * 1024)
        .out_buffer_size(64 * 1024);

    match direction {
        PipeDirection::ClientToServer => {
            options.access_inbound(true).access_outbound(false);
        }
        PipeDirection::ServerToClient => {
            options.access_inbound(false).access_outbound(true);
        }
    }

    let server = options.create(&name)?;
    let mut client_options = ClientOptions::new();
    client_options.pipe_mode(PipeMode::Byte);
    match direction {
        PipeDirection::ClientToServer => {
            client_options.read(false).write(true);
        }
        PipeDirection::ServerToClient => {
            client_options.read(true).write(false);
        }
    }
    let client = client_options.open(&name)?;
    Ok(PipePair { server, client })
}

pub(crate) struct OwnedHpcon(HPCON);

impl OwnedHpcon {
    pub(crate) fn create(size: COORD, input: HANDLE, output: HANDLE) -> io::Result<Self> {
        let mut hpc = 0_isize;
        let hr = unsafe { CreatePseudoConsole(size, input, output, 0, &mut hpc) };
        if hr != S_OK {
            return Err(hresult_error(hr));
        }
        Ok(Self(hpc))
    }

    pub(crate) fn raw(&self) -> HPCON {
        self.0
    }
}

impl Drop for OwnedHpcon {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe { ClosePseudoConsole(self.0) };
        }
    }
}

pub(crate) struct AttributeList {
    storage: Vec<usize>,
}

impl AttributeList {
    pub(crate) fn with_pseudoconsole(hpc: HPCON) -> io::Result<Self> {
        let mut size = 0_usize;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(last_os_error());
        }

        let slots = size.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; slots];
        let list = storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
        if initialized == 0 {
            return Err(last_os_error());
        }

        let updated = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const _,
                size_of::<HPCON>(),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(last_os_error());
        }

        Ok(Self { storage })
    }

    pub(crate) fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

pub(crate) fn spawn_child(
    attributes: LPPROC_THREAD_ATTRIBUTE_LIST,
    lines: u32,
    payload_bytes: Option<usize>,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes;

    let mut process_info = PROCESS_INFORMATION::default();
    let (program, mut command_line) = child_command(lines, payload_bytes)?;
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            null(),
            null(),
            &startup.StartupInfo as *const STARTUPINFOW,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error());
    }

    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess as _) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread as _) };
    Ok((process, thread))
}

pub(crate) fn spawn_cmd_control(
    lines: u32,
    payload_bytes: Option<usize>,
    extended_startup: bool,
) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = if extended_startup {
        size_of::<STARTUPINFOEXW>() as u32
    } else {
        size_of::<STARTUPINFOW>() as u32
    };

    let mut process_info = PROCESS_INFORMATION::default();
    let (program, mut command_line) = child_command(lines, payload_bytes)?;
    let creation_flags = if extended_startup {
        EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT
    } else {
        CREATE_UNICODE_ENVIRONMENT
    };
    let created = unsafe {
        CreateProcessW(
            program.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            creation_flags,
            null(),
            null(),
            &startup.StartupInfo as *const STARTUPINFOW,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error());
    }

    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess as _) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread as _) };
    Ok((process, thread))
}

pub(crate) fn wait_for_process(process: &OwnedHandle) -> io::Result<u32> {
    let wait = unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, 5_000) };
    if wait == WAIT_FAILED {
        return Err(last_os_error());
    }
    if wait != WAIT_OBJECT_0 {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "ConPTY child did not exit within spike timeout",
        ));
    }

    let mut exit_code = 0_u32;
    let ok = unsafe { GetExitCodeProcess(process.as_raw_handle() as HANDLE, &mut exit_code) };
    if ok == 0 {
        return Err(last_os_error());
    }
    Ok(exit_code)
}

pub(crate) fn current_process_handle_count() -> io::Result<u32> {
    let mut count = 0_u32;
    let ok = unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) };
    if ok == 0 {
        return Err(last_os_error());
    }
    Ok(count)
}

fn child_command(lines: u32, payload_bytes: Option<usize>) -> io::Result<(Vec<u16>, Vec<u16>)> {
    if let Some(bytes) = payload_bytes {
        let exe = std::env::current_exe()?;
        let exe = exe.into_os_string();
        let command = format!("\"{}\" --emit-payload {bytes}", exe.to_string_lossy());
        return Ok((wide_null(exe), wide_null(OsString::from(command))));
    }

    let cmd_path = std::env::var_os("COMSPEC")
        .unwrap_or_else(|| OsString::from("C:\\Windows\\System32\\cmd.exe"));
    let command = format!(
        "\"{}\" /C \"echo RMUX_CONPTY_READY&for /L %i in (1,1,{lines}) do @echo rmux-spike-%i\"",
        cmd_path.to_string_lossy()
    );
    Ok((wide_null(cmd_path), wide_null(OsString::from(command))))
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn wide_null(value: OsString) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn hresult_error(hr: i32) -> io::Error {
    io::Error::from_raw_os_error(hr)
}

fn last_os_error() -> io::Error {
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}
