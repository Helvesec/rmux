use std::io;
use std::time::{Duration, Instant};

const OBSERVATION_INTERVAL: Duration = Duration::from_millis(1);

pub(super) fn wait_for_no_live_members(process_group: i32, timeout: Duration) -> io::Result<bool> {
    let started = Instant::now();
    let mut consecutive_empty_observations = 0_u8;
    loop {
        match platform::group_has_live_members(process_group)? {
            Some(false) => {
                consecutive_empty_observations += 1;
                if consecutive_empty_observations == 2 {
                    return Ok(true);
                }
            }
            None => return Ok(false),
            Some(true) => consecutive_empty_observations = 0,
        }
        if started.elapsed() >= timeout && consecutive_empty_observations == 0 {
            return Ok(false);
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            std::thread::yield_now();
        } else {
            std::thread::sleep(OBSERVATION_INTERVAL.min(remaining));
        }
    }
}

/// Linux enumerates `/proc` for live members of the process group.
#[cfg(target_os = "linux")]
mod platform {
    use std::io;

    pub(super) fn group_has_live_members(process_group: i32) -> io::Result<Option<bool>> {
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .is_none()
            {
                continue;
            }
            let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error) if process_vanished(&error) => continue,
                Err(error) => return Err(error),
            };
            let (member_group, state) = linux_process_group_and_state(&stat).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "malformed /proc process stat")
            })?;
            if member_group == process_group && !matches!(state, 'Z' | 'X' | 'x') {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }

    /// A process reaped before `/proc/<pid>/stat` opens is absent; one reaped
    /// between open and read answers `ESRCH`. Neither is a live member, and
    /// on a busy host unrelated processes vanish mid-scan all the time.
    pub(super) fn process_vanished(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
    }

    pub(super) fn linux_process_group_and_state(stat: &str) -> Option<(i32, char)> {
        let (_, fields) = stat.rsplit_once(") ")?;
        let mut fields = fields.split_whitespace();
        let state = fields.next()?.chars().next()?;
        let _parent_pid = fields.next()?;
        let process_group = fields.next()?.parse().ok()?;
        Some((process_group, state))
    }
}

/// macOS lists group members through libproc.
#[cfg(target_os = "macos")]
mod platform {
    use std::io;

    pub(super) fn group_has_live_members(process_group: i32) -> io::Result<Option<bool>> {
        let process_group_u32 = u32::try_from(process_group)
            .map_err(|_| io::Error::other("process group id is negative"))?;
        for pid in macos_process_group_members(process_group)? {
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
            let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
                .map_err(|_| io::Error::other("process information size exceeds i32"))?;
            let read = unsafe {
                // SAFETY: `info` is writable for `size` bytes and the query does
                // not retain the pointer after returning.
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                )
            };
            if read < size {
                let exists = unsafe {
                    // SAFETY: A zero signal only probes the numeric PID returned
                    // by libproc and cannot change the target process.
                    libc::kill(pid, 0)
                };
                if exists == 0 {
                    return Ok(Some(true));
                }
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                if error.raw_os_error() == Some(libc::EPERM) {
                    return Ok(Some(true));
                }
                return Err(error);
            }
            let info = unsafe {
                // SAFETY: `proc_pidinfo` reported a full initialized structure.
                info.assume_init()
            };
            if info.pbi_pgid == process_group_u32 && info.pbi_status != libc::SZOMB {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }

    fn macos_process_group_members(process_group: i32) -> io::Result<Vec<i32>> {
        let required = unsafe {
            // SAFETY: A null buffer with zero length requests the required count.
            libc::proc_listpgrppids(process_group, std::ptr::null_mut(), 0)
        };
        if required < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut capacity = usize::try_from(required)
            .unwrap_or_default()
            .saturating_add(16)
            .max(16);
        loop {
            let mut pids = vec![0_i32; capacity];
            let byte_len = i32::try_from(capacity.saturating_mul(std::mem::size_of::<i32>()))
                .map_err(|_| io::Error::other("process group list exceeds i32"))?;
            let listed = unsafe {
                // SAFETY: `pids` is writable for `byte_len` bytes and libproc
                // returns at most the number of entries that fit in the buffer.
                libc::proc_listpgrppids(process_group, pids.as_mut_ptr().cast(), byte_len)
            };
            if listed < 0 {
                return Err(io::Error::last_os_error());
            }
            let listed = usize::try_from(listed).unwrap_or_default();
            if listed < capacity {
                pids.truncate(listed);
                pids.retain(|pid| *pid > 0);
                return Ok(pids);
            }
            capacity = capacity
                .checked_mul(2)
                .ok_or_else(|| io::Error::other("process group list capacity overflow"))?;
        }
    }
}

/// Other Unix targets cannot enumerate group members.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use std::io;

    pub(super) fn group_has_live_members(_process_group: i32) -> io::Result<Option<bool>> {
        Ok(None)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::platform::{linux_process_group_and_state, process_vanished};

    #[test]
    fn process_reaped_between_stat_open_and_read_is_not_an_observation_error() {
        use std::io::Read as _;

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleeper");
        let mut stat =
            std::fs::File::open(format!("/proc/{}/stat", child.id())).expect("open sleeper stat");
        child.kill().expect("kill sleeper");
        child.wait().expect("reap sleeper");

        let error = stat
            .read_to_string(&mut String::new())
            .expect_err("a reaped process has no stat to read");
        assert!(
            process_vanished(&error),
            "vanished member reported as {error:?}"
        );
    }

    #[test]
    fn linux_stat_parser_handles_parentheses_in_process_names() {
        let stat = "123 (worker) with spaces) R 42 789 789 0 -1";

        assert_eq!(linux_process_group_and_state(stat), Some((789, 'R')));
    }

    #[test]
    fn linux_stat_parser_preserves_zombie_state() {
        let stat = "123 (worker) Z 42 789 789 0 -1";

        assert_eq!(linux_process_group_and_state(stat), Some((789, 'Z')));
    }
}
