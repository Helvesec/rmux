//! PID-reuse-safe Windows process liveness checks.

#![cfg(windows)]

use std::io;
use std::mem::zeroed;

use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, WaitForSingleObject,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::windows_endpoint_record::ProcessStamp;

pub(crate) fn current() -> io::Result<ProcessStamp> {
    creation_time(unsafe {
        // SAFETY: GetCurrentProcess returns a valid pseudo-handle.
        GetCurrentProcess()
    })
    .map(|created| ProcessStamp {
        pid: std::process::id(),
        created,
    })
}

pub(crate) fn is_live(stamp: ProcessStamp) -> bool {
    let process = unsafe {
        // SAFETY: OpenProcess validates the pid.
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            stamp.pid,
        )
    };
    if process.is_null() {
        return false;
    }
    let alive = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
    let created = creation_time(process).ok();
    unsafe {
        // SAFETY: process is a live handle returned by OpenProcess.
        CloseHandle(process);
    }
    alive && created == Some(stamp.created)
}

fn creation_time(process: HANDLE) -> io::Result<u64> {
    let mut creation: FILETIME = unsafe { zeroed() };
    let mut exit: FILETIME = unsafe { zeroed() };
    let mut kernel: FILETIME = unsafe { zeroed() };
    let mut user: FILETIME = unsafe { zeroed() };
    let ok = unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_rejects_pid_reuse_by_creation_time() {
        let current = current().expect("current process stamp");
        assert!(is_live(current));
        assert!(!is_live(ProcessStamp {
            created: current.created.wrapping_add(1),
            ..current
        }));
    }
}
