use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

pub(super) fn resume_suspended_process(process_id: u32) -> io::Result<()> {
    const THREAD_DISCOVERY_ATTEMPTS: usize = 20;
    const THREAD_DISCOVERY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

    for attempt in 0..THREAD_DISCOVERY_ATTEMPTS {
        if let Some(thread_id) = find_process_thread(process_id)? {
            return resume_thread(thread_id);
        }
        if attempt + 1 < THREAD_DISCOVERY_ATTEMPTS {
            std::thread::sleep(THREAD_DISCOVERY_INTERVAL);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "suspended child primary thread was not found",
    ))
}

fn find_process_thread(process_id: u32) -> io::Result<Option<u32>> {
    let snapshot = unsafe {
        // SAFETY: A system-wide thread snapshot takes no borrowed pointers.
        CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)
    };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let snapshot = unsafe {
        // SAFETY: The snapshot is a valid owned Win32 handle and is transferred
        // exactly once into `OwnedHandle`.
        OwnedHandle::from_raw_handle(snapshot as _)
    };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut found = unsafe {
        // SAFETY: `snapshot` is live and `entry` points to initialized writable
        // storage with the required size field.
        Thread32First(snapshot.as_raw_handle() as HANDLE, &mut entry)
    } != 0;
    while found {
        if entry.th32OwnerProcessID == process_id {
            return Ok(Some(entry.th32ThreadID));
        }
        found = unsafe {
            // SAFETY: The same live snapshot and writable entry remain valid
            // for the duration of enumeration.
            Thread32Next(snapshot.as_raw_handle() as HANDLE, &mut entry)
        } != 0;
    }
    Ok(None)
}

fn resume_thread(thread_id: u32) -> io::Result<()> {
    let thread = unsafe {
        // SAFETY: `thread_id` came from a live Toolhelp snapshot. The returned
        // handle is checked before ownership transfer.
        OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id)
    };
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    let thread = unsafe {
        // SAFETY: `OpenThread` returned a non-null owned handle and it is
        // transferred exactly once into `OwnedHandle`.
        OwnedHandle::from_raw_handle(thread as _)
    };
    let previous_suspend_count = unsafe {
        // SAFETY: `thread` is live and opened with THREAD_SUSPEND_RESUME.
        ResumeThread(thread.as_raw_handle() as HANDLE)
    };
    if previous_suspend_count == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
