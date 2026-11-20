use super::spawn::{duplicate_handle, last_os_error};
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

#[derive(Debug)]
pub(super) struct JobObjectGuard {
    handle: OwnedHandle,
}

impl JobObjectGuard {
    pub(super) fn new(allow_explicit_breakaway: bool) -> io::Result<Self> {
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
        limits.BasicLimitInformation.LimitFlags = child_job_limit_flags(allow_explicit_breakaway);
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

    pub(super) fn assign(&self, process: &OwnedHandle) -> io::Result<()> {
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

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            handle: duplicate_handle(&self.handle)?,
        })
    }

    pub(super) fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: `self.handle` is a live job handle owned by this guard; the
        // API does not take ownership of it.
        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, exit_code) };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(())
    }

    pub(super) fn active_process_count(&self) -> io::Result<u32> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `self.handle` is a live job handle and `accounting` is
        // writable for the exact information-class size. No pointer is
        // retained after the call.
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle() as HANDLE,
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>())
                    .map_err(|_| io::Error::other("job accounting size exceeds u32"))?,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_os_error());
        }
        Ok(accounting.ActiveProcesses)
    }
}

const fn child_job_limit_flags(allow_explicit_breakaway: bool) -> u32 {
    if allow_explicit_breakaway {
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK
    } else {
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    }
}

#[cfg(test)]
mod tests {
    use super::child_job_limit_flags;
    use windows_sys::Win32::System::JobObjects::{
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    };

    #[test]
    fn child_job_defaults_to_strict_cleanup() {
        let flags = child_job_limit_flags(false);

        assert_ne!(flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, 0);
        assert_eq!(flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK, 0);
        assert_eq!(flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, 0);
    }

    #[test]
    fn opted_in_child_job_allows_only_explicit_breakaway() {
        let flags = child_job_limit_flags(true);

        assert_ne!(flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, 0);
        assert_ne!(flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK, 0);
        assert_eq!(flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK, 0);
    }
    #[test]
    fn resume_failure_preserves_the_error_before_failed_cleanup() {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
        use windows_sys::Win32::System::Threading::{
            CreateEventW, GetCurrentThreadId, OpenThread, THREAD_QUERY_LIMITED_INFORMATION,
        };
        // A real thread handle without suspend/resume access gives error 5.
        // An event passed to cleanup gives error 6; no child is started here.
        // SAFETY: OpenThread returns a new handle that OwnedHandle then owns.
        let thread =
            unsafe { OpenThread(THREAD_QUERY_LIMITED_INFORMATION, 0, GetCurrentThreadId()) };
        assert!(!thread.is_null());
        let thread = unsafe { OwnedHandle::from_raw_handle(thread as _) };
        // SAFETY: CreateEventW returns a new handle that OwnedHandle then owns.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        assert!(!event.is_null());
        let event = unsafe { OwnedHandle::from_raw_handle(event as _) };
        let error = super::super::spawn::resume_primary_thread(&event, &thread).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(ERROR_ACCESS_DENIED as i32));
    }
    #[test]
    fn creation_failure_preserves_the_native_directory_error() {
        use crate::{ChildCommand, PtyError};
        use std::path::PathBuf;
        let program = PathBuf::from(std::env::var_os("SystemRoot").expect("Windows directory"))
            .join("System32/cmd.exe");
        let missing = std::env::temp_dir().join(format!("rmux-absent-cwd-{}", std::process::id()));
        assert!(!missing.exists());
        let expected = std::process::Command::new(&program)
            .current_dir(&missing)
            .spawn()
            .unwrap_err()
            .raw_os_error();
        assert!(expected.is_some());
        match ChildCommand::new(program).current_dir(missing).spawn() {
            Err(PtyError::Spawn(error)) => assert_eq!(error.raw_os_error(), expected),
            Err(error) => panic!("unexpected spawn error: {error}"),
            Ok(_) => panic!("a missing current directory must reject process creation"),
        }
    }
}
