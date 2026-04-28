use std::collections::{HashMap, HashSet};
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use tracing::warn;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

use crate::ProcessId;

pub(super) fn terminate_process_tree(
    root_pid: ProcessId,
    root_process: &OwnedHandle,
    exit_code: u32,
) -> io::Result<()> {
    let root_result = terminate_handle(root_process, exit_code);
    if let Err(error) = &root_result {
        warn!(
            target: "rmux::conpty",
            root_pid = root_pid.as_u32(),
            "failed to terminate fallback root process before descendant cleanup: {error}"
        );
    }

    match process_snapshot() {
        Ok(entries) => {
            for pid in descendant_pids(root_pid.as_u32(), &entries) {
                if let Err(error) = terminate_pid(pid, exit_code) {
                    warn!(
                        target: "rmux::conpty",
                        root_pid = root_pid.as_u32(),
                        pid,
                        "failed to terminate fallback child process: {error}"
                    );
                }
            }
        }
        Err(error) => {
            warn!(
                target: "rmux::conpty",
                root_pid = root_pid.as_u32(),
                "failed to snapshot process tree for fallback cleanup: {error}"
            );
        }
    }
    root_result
}

fn process_snapshot() -> io::Result<Vec<ProcessEntry>> {
    let snapshot = unsafe {
        // SAFETY: The flags request a system-owned process snapshot and do not
        // borrow memory from this process. The returned handle is checked below.
        CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(last_os_error());
    }
    let snapshot = unsafe {
        // SAFETY: `snapshot` is a valid owned snapshot handle and ownership is
        // transferred exactly once into `OwnedHandle`.
        OwnedHandle::from_raw_handle(snapshot as _)
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    let first = unsafe {
        // SAFETY: `snapshot` is live and `entry` points to initialized storage
        // with its documented size field set.
        Process32FirstW(snapshot.as_raw_handle() as HANDLE, &mut entry)
    };
    if first == 0 {
        return no_more_files_as_empty();
    }

    let mut entries = Vec::new();
    loop {
        entries.push(ProcessEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
        });
        let next = unsafe {
            // SAFETY: Same live snapshot and initialized `entry` buffer as the
            // successful first call above.
            Process32NextW(snapshot.as_raw_handle() as HANDLE, &mut entry)
        };
        if next == 0 {
            let error = last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(entries);
            }
            return Err(error);
        }
    }
}

fn no_more_files_as_empty() -> io::Result<Vec<ProcessEntry>> {
    let error = last_os_error();
    if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
        Ok(Vec::new())
    } else {
        Err(error)
    }
}

fn descendant_pids(root_pid: u32, entries: &[ProcessEntry]) -> Vec<u32> {
    let mut children_by_parent = HashMap::<u32, Vec<u32>>::new();
    for entry in entries {
        if entry.pid != root_pid {
            children_by_parent
                .entry(entry.parent_pid)
                .or_default()
                .push(entry.pid);
        }
    }

    let mut ordered = Vec::new();
    let mut seen = HashSet::new();
    let mut stack = children_by_parent
        .get(&root_pid)
        .cloned()
        .unwrap_or_default();

    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        ordered.push(pid);
        if let Some(children) = children_by_parent.get(&pid) {
            stack.extend(children);
        }
    }

    ordered.reverse();
    ordered
}

fn terminate_pid(pid: u32, exit_code: u32) -> io::Result<()> {
    let handle = unsafe {
        // SAFETY: `OpenProcess` receives a process id from a Toolhelp snapshot.
        // The handle is checked before ownership transfer.
        OpenProcess(PROCESS_TERMINATE, 0, pid)
    };
    if handle.is_null() {
        let error = last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(error);
    }
    let handle = unsafe {
        // SAFETY: `OpenProcess` returned a non-null owned handle.
        OwnedHandle::from_raw_handle(handle as _)
    };
    terminate_handle(&handle, exit_code)
}

fn terminate_handle(process: &OwnedHandle, exit_code: u32) -> io::Result<()> {
    let ok = unsafe {
        // SAFETY: `process` is a live process handle owned by the caller; the
        // API borrows it only for the duration of this call.
        TerminateProcess(process.as_raw_handle() as HANDLE, exit_code)
    };
    if ok == 0 {
        return Err(last_os_error());
    }
    Ok(())
}

fn last_os_error() -> io::Error {
    let code = unsafe {
        // SAFETY: `GetLastError` reads the calling thread's last-error slot and
        // has no preconditions.
        GetLastError()
    };
    io::Error::from_raw_os_error(code as i32)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessEntry {
    pid: u32,
    parent_pid: u32,
}

#[cfg(test)]
mod tests {
    use super::{descendant_pids, ProcessEntry};

    #[test]
    fn descendant_order_terminates_deepest_children_first() {
        let entries = [
            ProcessEntry {
                pid: 10,
                parent_pid: 1,
            },
            ProcessEntry {
                pid: 11,
                parent_pid: 10,
            },
            ProcessEntry {
                pid: 12,
                parent_pid: 11,
            },
            ProcessEntry {
                pid: 20,
                parent_pid: 1,
            },
        ];

        assert_eq!(descendant_pids(10, &entries), vec![12, 11]);
    }

    #[test]
    fn descendant_walk_handles_cycles_without_looping() {
        let entries = [
            ProcessEntry {
                pid: 2,
                parent_pid: 1,
            },
            ProcessEntry {
                pid: 3,
                parent_pid: 2,
            },
            ProcessEntry {
                pid: 2,
                parent_pid: 3,
            },
        ];

        assert_eq!(descendant_pids(1, &entries), vec![3, 2]);
    }
}
