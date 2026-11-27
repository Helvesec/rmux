use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::null;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

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

    pub(crate) fn raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
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

pub(super) const fn job_limit_flags(allow_breakaway: bool) -> u32 {
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
