//! Handle-free Windows daemon creation, with explicitly bounded job lifetime.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcess, PROCESS_INFORMATION, STARTUPINFOW,
};

mod executable;

/// An owned observation handle for a daemon. Dropping it does not stop the daemon.
#[derive(Debug)]
pub struct WindowsDaemonChild {
    _process: OwnedHandle,
    pid: u32,
    job_bound: bool,
}

impl WindowsDaemonChild {
    /// Returns the process identifier assigned at creation.
    #[must_use]
    pub const fn id(&self) -> u32 {
        self.pid
    }

    /// Whether the daemon remains subject to an enclosing job's lifetime limits.
    /// An unavailable job-membership query is conservatively treated as bound.
    #[must_use]
    pub const fn is_job_bound(&self) -> bool {
        self.job_bound
    }
}

/// Launches a daemon without inheriting **any** launcher handles.
///
/// `program` names an executable, using `Command`'s Windows search order; arguments
/// use the Windows C runtime convention, not shell syntax. Environment and working
/// directory come from this process. Breakaway is attempted first; access denied
/// while in a job permits one retry in that job. A surviving job association is
/// reported with a single stderr warning, including partial nested breakaway.
/// Readiness is signaled separately by the daemon through `StartupReadyEvent`.
pub fn launch_hidden_daemon<I, S>(program: &OsStr, args: I) -> io::Result<WindowsDaemonChild>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    executable_name(program)?;
    let application = executable::resolve(program)?;
    let command_line = command_line(program, args)?;
    let process = match create_process(&application, &command_line, true) {
        Err(error)
            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                && current_process_is_job_bound()? =>
        {
            create_process(&application, &command_line, false)?
        }
        result => result?,
    };
    // SAFETY: CreateProcessW returned an owned, non-inheritable process handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
    // SAFETY: no thread operations are needed; close the separately owned handle.
    unsafe { CloseHandle(process.hThread) };
    // Do not convert a successful creation into a retryable failure. If querying
    // the new process fails, declare the more restrictive lifetime instead.
    let job_bound = process_is_job_bound(handle.as_raw_handle()).unwrap_or(true);
    if job_bound {
        let _ = writeln!(
            io::stderr().lock(),
            "rmux: warning: daemon is job-bound; an enclosing job may terminate it"
        );
    }
    Ok(WindowsDaemonChild {
        _process: handle,
        pid: process.dwProcessId,
        job_bound,
    })
}

pub(super) fn current_process_is_job_bound() -> io::Result<bool> {
    // SAFETY: a pseudo-handle is valid for querying the current process.
    process_is_job_bound(unsafe { GetCurrentProcess() })
}

fn process_is_job_bound(process: HANDLE) -> io::Result<bool> {
    let mut in_job = 0;
    // SAFETY: process is a live owned or current-process handle; out pointer valid.
    if unsafe { IsProcessInJob(process, std::ptr::null_mut(), &mut in_job) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(in_job != 0)
    }
}

fn create_process(
    application: &[u16],
    command_line: &[u16],
    breakaway: bool,
) -> io::Result<PROCESS_INFORMATION> {
    // CreateProcessW can mutate the command line, even on failure. Each attempt
    // gets its own buffer so the fallback receives exactly the same arguments.
    let mut command_line = command_line.to_vec();
    // SAFETY: zero-initialized Win32 structures with the required cb field.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: UTF-16 buffers are NUL-terminated and live for the call. No handles
    // are inherited (bInheritHandles=FALSE); STARTF_USESTDHANDLES is absent and the
    // child is detached. Null environment/cwd preserve the launcher's values.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            super::hidden_daemon_creation_flags(breakaway),
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(process)
    }
}

fn executable_name(program: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide: Vec<_> = program.encode_wide().collect();
    if wide.is_empty() || wide.contains(&0) || wide.contains(&u16::from(b'"')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid daemon executable name",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn command_line<I, S>(program: &OsStr, args: I) -> io::Result<Vec<u16>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    // argv[0] has different quoting rules: quotes cannot occur in an executable
    // path, and backslashes are literal. All later arguments use CRT escaping.
    let mut line = vec![u16::from(b'"')];
    line.extend(program.encode_wide());
    line.push(u16::from(b'"'));
    for arg in args {
        line.push(u16::from(b' '));
        append_argument(&mut line, arg.as_ref())?;
    }
    line.push(0);
    if line.len() > 32_767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon command line exceeds Windows limit",
        ));
    }
    Ok(line)
}

fn append_argument(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    line.push(u16::from(b'"'));
    let mut slashes = 0;
    for unit in arg.encode_wide() {
        if unit == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in daemon argument",
            ));
        }
        if unit == u16::from(b'\\') {
            slashes += 1;
            continue;
        }
        let count = if unit == u16::from(b'"') {
            slashes * 2 + 1
        } else {
            slashes
        };
        line.extend(std::iter::repeat_n(u16::from(b'\\'), count));
        slashes = 0;
        line.push(unit);
    }
    line.extend(std::iter::repeat_n(u16::from(b'\\'), slashes * 2));
    line.push(u16::from(b'"'));
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod handle_tests;
