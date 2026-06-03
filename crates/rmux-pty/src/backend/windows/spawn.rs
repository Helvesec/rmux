use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, ERROR_ACCESS_DENIED,
    ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW,
};

use crate::{ChildCommand, ProcessId, Result, Signal};

use super::application::resolve_application_path;
use super::command_line::{command_line, environment_block, wide_null};
use super::{should_enable_dsr_bootstrap, WindowsPty};

#[derive(Debug)]
pub(crate) struct WindowsChild {
    process: OwnedHandle,
    #[allow(dead_code)]
    thread: OwnedHandle,
    job: Option<JobObjectGuard>,
    pty: Arc<WindowsPty>,
    pid: ProcessId,
}

impl WindowsChild {
    pub(crate) fn pid(&self) -> ProcessId {
        self.pid
    }
}

pub(crate) fn spawn_child(command: ChildCommand, pty: Arc<WindowsPty>) -> Result<WindowsChild> {
    if should_enable_dsr_bootstrap(&command.program) {
        tracing::debug!(
            target: "rmux::conpty",
            "enabling one-shot DSR bootstrap for PowerShell child"
        );
        pty.enable_dsr_bootstrap()?;
    }

    let job = JobObjectGuard::new()?;
    let process = create_suspended_process_with_conpty_fallback(&command, &pty, 0)?;

    match job.assign(&process.process) {
        Ok(()) => resume_as_child(process, Some(job), pty),
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            tracing::debug!(
                target: "rmux::conpty",
                "job assignment denied; retrying child with breakaway flag"
            );
            let _ = terminate_process(&process.process, 1);
            spawn_child_breakaway(command, pty)
        }
        Err(error) => {
            let _ = terminate_process(&process.process, 1);
            Err(error.into())
        }
    }
}

fn spawn_child_breakaway(command: ChildCommand, pty: Arc<WindowsPty>) -> Result<WindowsChild> {
    let job = JobObjectGuard::new()?;
    match create_suspended_process_with_conpty_fallback(&command, &pty, CREATE_BREAKAWAY_FROM_JOB) {
        Ok(process) => match job.assign(&process.process) {
            Ok(()) => resume_as_child(process, Some(job), pty),
            Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
                tracing::error!(
                    target: "rmux::conpty",
                    "breakaway job assignment denied; refusing to run unguarded ConPTY child"
                );
                let _ = terminate_process(&process.process, 1);
                Err(job_required_error("breakaway job assignment denied", error).into())
            }
            Err(error) => {
                let _ = terminate_process(&process.process, 1);
                Err(error.into())
            }
        },
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            tracing::error!(
                target: "rmux::conpty",
                "breakaway process creation denied; refusing to run unguarded ConPTY child"
            );
            Err(job_required_error("breakaway process creation denied", error).into())
        }
        Err(error) => Err(error.into()),
    }
}

fn create_suspended_process_with_conpty_fallback(
    command: &ChildCommand,
    pty: &WindowsPty,
    extra_creation_flags: u32,
) -> io::Result<SuspendedProcess> {
    match create_suspended_process(command, pty, extra_creation_flags) {
        Err(error) if is_invalid_parameter(&error) && pty.uses_passthrough() => {
            tracing::warn!(
                target: "rmux::conpty",
                "CreateProcessW rejected passthrough ConPTY; retrying without passthrough"
            );
            pty.recreate_without_passthrough()
                .map_err(io::Error::other)?;
            create_suspended_process(command, pty, extra_creation_flags)
        }
        other => other,
    }
}

fn create_suspended_process(
    command: &ChildCommand,
    pty: &WindowsPty,
    extra_creation_flags: u32,
) -> io::Result<SuspendedProcess> {
    let mut attributes = AttributeList::with_pseudoconsole(pty.hpc())?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.as_mut_ptr();

    let application_path = resolve_application_path(command)?;
    let application = wide_null(application_path.as_os_str());
    let mut command_line = command_line(command);
    let mut environment = environment_block(command);
    let current_dir = command
        .current_dir
        .as_ref()
        .map(|path| wide_null(path.as_os_str()));
    let mut process_info = PROCESS_INFORMATION::default();

    // SAFETY: All UTF-16 buffers are NUL-terminated and remain alive for the
    // duration of the call, `startup` and `process_info` point to initialized
    // stack values, and handle inheritance is disabled.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT
                | CREATE_SUSPENDED
                | extra_creation_flags,
            environment
                .as_mut()
                .map_or(null(), |block| block.as_mut_ptr().cast()),
            current_dir.as_ref().map_or(null(), |path| path.as_ptr()),
            &startup.StartupInfo as *const STARTUPINFOW,
            &mut process_info,
        )
    };
    if created == 0 {
        return Err(last_os_error());
    }

    // SAFETY: `CreateProcessW` succeeded, so these returned handles are owned
    // by this function and are transferred exactly once into `OwnedHandle`.
    let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess as _) };
    let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread as _) };
    Ok(SuspendedProcess {
        process,
        thread,
        pid: process_info.dwProcessId,
    })
}

fn resume_as_child(
    process: SuspendedProcess,
    job: Option<JobObjectGuard>,
    pty: Arc<WindowsPty>,
) -> Result<WindowsChild> {
    // SAFETY: `process.thread` is the primary thread handle returned by
    // `CreateProcessW` in suspended mode and is still owned here.
    let resume = unsafe { ResumeThread(process.thread.as_raw_handle() as HANDLE) };
    if resume == u32::MAX {
        let _ = terminate_process(&process.process, 1);
        return Err(last_os_error().into());
    }

    let pid = ProcessId::new(process.pid)?;
    tracing::debug!(
        target: "rmux::conpty",
        pid = pid.as_u32(),
        job_guarded = job.is_some(),
        "resumed ConPTY child"
    );
    Ok(WindowsChild {
        process: process.process,
        thread: process.thread,
        job,
        pty,
        pid,
    })
}

struct SuspendedProcess {
    process: OwnedHandle,
    thread: OwnedHandle,
    pid: u32,
}

pub(crate) fn wait_child(child: &mut WindowsChild) -> Result<ExitStatus> {
    // SAFETY: `child.process` is a live process handle owned by `WindowsChild`;
    // waiting on it does not invalidate the handle.
    let wait = unsafe { WaitForSingleObject(child.process.as_raw_handle() as HANDLE, u32::MAX) };
    if wait == WAIT_FAILED {
        return Err(last_os_error().into());
    }
    exit_status(&child.process)
}

pub(crate) fn try_wait_child(child: &mut WindowsChild) -> Result<Option<ExitStatus>> {
    // SAFETY: `child.process` is a live process handle owned by `WindowsChild`;
    // a zero-timeout wait only observes the process state.
    let wait = unsafe { WaitForSingleObject(child.process.as_raw_handle() as HANDLE, 0) };
    match wait {
        WAIT_OBJECT_0 => Ok(Some(exit_status(&child.process)?)),
        WAIT_TIMEOUT => Ok(None),
        WAIT_FAILED => Err(last_os_error().into()),
        _ => Err(io::Error::other("unexpected process wait result").into()),
    }
}

pub(crate) fn try_clone_child_for_wait(child: &WindowsChild) -> Result<WindowsChild> {
    Ok(WindowsChild {
        process: duplicate_handle(&child.process)?,
        thread: duplicate_handle(&child.thread)?,
        job: None,
        pty: Arc::clone(&child.pty),
        pid: child.pid,
    })
}

pub(crate) fn close_child_pseudoconsole(child: &WindowsChild) {
    child.pty.close_pseudoconsole();
}

pub(crate) fn interrupt_child(child: &WindowsChild) -> Result<()> {
    child.pty.write_all(b"\x03")?;
    Ok(())
}

pub(crate) fn kill_child(child: &WindowsChild, signal: Signal) -> Result<()> {
    match signal {
        Signal::INT => interrupt_child(child),
        Signal::CONT => Ok(()),
        Signal::TERM | Signal::KILL | Signal::HUP => {
            child.pty.close_pseudoconsole();
            if let Some(job) = &child.job {
                job.terminate(1)?;
                if process_is_still_running(&child.process)? {
                    let _ = terminate_process(&child.process, 1);
                }
            } else {
                return Err(io::Error::other(
                    "Windows child has no Job Object cleanup guard; refusing unsafe fallback kill",
                )
                .into());
            }
            Ok(())
        }
    }
}

fn process_is_still_running(process: &OwnedHandle) -> io::Result<bool> {
    // SAFETY: `process` is a live process handle owned by the caller; waiting
    // with a short timeout only observes process state.
    let wait = unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, 500) };
    match wait {
        WAIT_OBJECT_0 => Ok(false),
        WAIT_TIMEOUT => Ok(true),
        WAIT_FAILED => Err(last_os_error()),
        _ => Err(io::Error::other("unexpected process wait result")),
    }
}

fn terminate_process(process: &OwnedHandle, exit_code: u32) -> io::Result<()> {
    // SAFETY: `process` is a live process handle owned by the caller; the API
    // does not take ownership of the handle.
    let ok = unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, exit_code) };
    if ok == 0 {
        return Err(last_os_error());
    }
    Ok(())
}

fn is_invalid_parameter(error: &io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32)
}

fn job_required_error(stage: &'static str, source: io::Error) -> io::Error {
    io::Error::new(
        source.kind(),
        format!("{stage}; refusing to run ConPTY child without Job Object cleanup: {source}"),
    )
}

fn duplicate_handle(handle: &OwnedHandle) -> io::Result<OwnedHandle> {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle for the current
    // process and has no preconditions.
    let current_process = unsafe { GetCurrentProcess() };
    let mut duplicated: HANDLE = null_mut();
    // SAFETY: `handle` is valid, `duplicated` is a valid out-pointer, and the
    // source and target process handles intentionally reference this process.
    let ok = unsafe {
        DuplicateHandle(
            current_process,
            handle.as_raw_handle() as HANDLE,
            current_process,
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    // SAFETY: `DuplicateHandle` succeeded and returned a new owned handle that
    // is transferred exactly once into `OwnedHandle`.
    Ok(unsafe { OwnedHandle::from_raw_handle(duplicated as _) })
}

fn exit_status(process: &OwnedHandle) -> Result<ExitStatus> {
    let mut exit_code = 0_u32;
    // SAFETY: `process` is a live process handle and `exit_code` is a valid
    // out-pointer for the duration of the call.
    let ok = unsafe { GetExitCodeProcess(process.as_raw_handle() as HANDLE, &mut exit_code) };
    if ok == 0 {
        return Err(last_os_error().into());
    }
    Ok(ExitStatus::from_raw(exit_code))
}

#[derive(Debug)]
struct JobObjectGuard {
    handle: OwnedHandle,
}

impl JobObjectGuard {
    fn new() -> io::Result<Self> {
        // SAFETY: Null security attributes and name request the default unnamed
        // job object; the returned handle is checked before ownership transfer.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(last_os_error());
        }
        // SAFETY: `CreateJobObjectW` returned a non-null owned handle and this
        // function transfers it exactly once into `OwnedHandle`.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle as _) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `handle` is a live job handle, `limits` points to an
        // initialized structure of the declared size, and the API borrows it
        // only for the duration of the call.
        let ok = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(Self { handle })
    }

    fn assign(&self, process: &OwnedHandle) -> io::Result<()> {
        // SAFETY: Both handles are live and owned by their wrappers; the API
        // associates the process with the job without taking ownership.
        let ok = unsafe {
            AssignProcessToJobObject(
                self.handle.as_raw_handle() as HANDLE,
                process.as_raw_handle() as HANDLE,
            )
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(())
    }

    fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: `self.handle` is a live job handle owned by this guard; the
        // API does not take ownership of it.
        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, exit_code) };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(())
    }
}

struct AttributeList {
    storage: Vec<usize>,
}

impl AttributeList {
    fn with_pseudoconsole(hpc: isize) -> io::Result<Self> {
        let mut size = 0_usize;
        // SAFETY: The first call follows the documented sizing pattern: null
        // list pointer, attribute count one, and a valid size out-pointer.
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        }
        if size == 0 {
            return Err(last_os_error());
        }

        let slots = size.div_ceil(size_of::<usize>());
        let mut storage = vec![0_usize; slots];
        let list = storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `storage` is sized from the API-provided byte count and stays
        // alive inside `AttributeList`; `list` points into that storage.
        let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut size) };
        if initialized == 0 {
            return Err(last_os_error());
        }

        // SAFETY: `list` is initialized, `hpc` is a live ConPTY handle, and the
        // attribute value pointer is valid for the duration of the call.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpc as *const _,
                size_of::<isize>(),
                null_mut(),
                null(),
            )
        };
        if updated == 0 {
            // SAFETY: `list` was initialized successfully above and is cleaned
            // up before returning the error.
            unsafe { DeleteProcThreadAttributeList(list) };
            return Err(last_os_error());
        }

        Ok(Self { storage })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        // SAFETY: `AttributeList` only exists after successful initialization,
        // and `Drop` runs exactly once for the backing storage.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

fn last_os_error() -> io::Error {
    // SAFETY: `GetLastError` reads the calling thread's last-error slot and has
    // no preconditions.
    let code = unsafe { GetLastError() };
    io::Error::from_raw_os_error(code as i32)
}
