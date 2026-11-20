use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

use rustix::fs::{fstat, FileType};
use rustix::io::{fcntl_getfd, Errno, FdFlags};

use super::{Grant, GrantWatchdog};

// Far above an arm-grant-disarm round trip, even on a loaded host.
const LIMIT: Duration = Duration::from_secs(1);
// Bounds a broken escape: a restarting grant then gives up and reports success.
const UNESCAPED_GIVE_UP: Duration = Duration::from_secs(10);

/// A pipe stands in for the PTY master: no PTY is allocated on the host.
fn stand_in_master() -> OwnedFd {
    let (reader, _writer) = std::io::pipe().expect("pipe");
    OwnedFd::from(reader)
}

fn watchdog() -> &'static GrantWatchdog {
    GrantWatchdog::spawn(LIMIT).expect("watchdog thread")
}

fn file_type(fd: BorrowedFd<'_>) -> FileType {
    FileType::from_raw_mode(fstat(fd).expect("fstat").st_mode)
}

/// Re-issues a syscall on the descriptor number like xnu's ERESTART does for
/// `TIOCPTYGRANT`, until that number no longer names the stand-in master.
/// Returns that syscall's errno, or `Ok` if the escape never came.
fn restart_until_swapped(master: RawFd) -> Result<(), Errno> {
    let started = Instant::now();
    while started.elapsed() < UNESCAPED_GIVE_UP {
        // SAFETY: the test owns `master` for the whole grant.
        let fd = unsafe { BorrowedFd::borrow_raw(master) };
        // xnu's dup2 shows EBADF to concurrent lookups while it swaps.
        if FileType::from_raw_mode(fstat(fd)?.st_mode) != FileType::Fifo {
            return Err(Errno::NOTTY);
        }
    }
    Ok(())
}

#[test]
fn a_restarting_grant_is_escaped_and_its_master_swapped_for_dev_null() {
    let master = stand_in_master();
    let raw = master.as_raw_fd();
    let started = Instant::now();

    let grant = watchdog().watch(master.as_fd(), || restart_until_swapped(raw));

    assert_eq!(grant, Ok(Grant::Stalled));
    assert!(
        started.elapsed() < UNESCAPED_GIVE_UP,
        "the grant was never escaped"
    );
    assert_eq!(file_type(master.as_fd()), FileType::CharacterDevice);
    assert!(fcntl_getfd(master.as_fd())
        .expect("F_GETFD")
        .contains(FdFlags::CLOEXEC));
}

#[test]
fn a_prompt_grant_keeps_its_master() {
    let master = stand_in_master();

    let grant = watchdog().watch(master.as_fd(), || Ok(()));

    assert_eq!(grant, Ok(Grant::Granted));
    std::thread::sleep(LIMIT * 2);
    assert_eq!(file_type(master.as_fd()), FileType::Fifo);
}

#[test]
fn a_prompt_grant_error_is_returned_unchanged() {
    let master = stand_in_master();

    let grant = watchdog().watch(master.as_fd(), || Err(Errno::ACCESS));

    assert_eq!(grant, Err(Errno::ACCESS));
    assert_eq!(file_type(master.as_fd()), FileType::Fifo);
}

#[test]
fn a_grant_returning_after_its_escape_still_discards_the_pair() {
    let master = stand_in_master();

    let raw = master.as_raw_fd();

    // A healthy grant that was descheduled past the limit: it returns success
    // once the swap has happened.
    let grant = watchdog().watch(master.as_fd(), || {
        let _ = restart_until_swapped(raw);
        Ok(())
    });

    assert_eq!(grant, Ok(Grant::Stalled));
    assert_eq!(file_type(master.as_fd()), FileType::CharacterDevice);
}

#[test]
fn one_watchdog_escapes_only_the_stalled_grant_among_concurrent_ones() {
    let watchdog = watchdog();
    let stalled = stand_in_master();
    let healthy = stand_in_master();
    let raw = stalled.as_raw_fd();

    let (armed, stalled_is_armed) = std::sync::mpsc::channel();

    let (stalled_grant, healthy_grant) = std::thread::scope(|scope| {
        let stalled_grant = scope.spawn(|| {
            watchdog.watch(stalled.as_fd(), || {
                armed.send(()).expect("signal the armed stall");
                restart_until_swapped(raw)
            })
        });
        stalled_is_armed.recv().expect("the stalled grant starts");
        let healthy_grant = watchdog.watch(healthy.as_fd(), || Ok(()));
        (stalled_grant.join().expect("stalled grant"), healthy_grant)
    });

    assert_eq!(stalled_grant, Ok(Grant::Stalled));
    assert_eq!(healthy_grant, Ok(Grant::Granted));
    assert_eq!(file_type(stalled.as_fd()), FileType::CharacterDevice);
    assert_eq!(file_type(healthy.as_fd()), FileType::Fifo);
    assert_eq!(
        watchdog.watch(healthy.as_fd(), || Ok(())),
        Ok(Grant::Granted),
        "the watchdog keeps serving after an escape"
    );
}

#[test]
fn an_unwinding_grant_leaves_its_descriptor_number_unwatched() {
    let watchdog = watchdog();
    let master = stand_in_master();

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        watchdog.watch(master.as_fd(), || panic!("grant panicked"))
    }));
    assert!(unwound.is_err());
    // A watch left armed would swap this number once its deadline passed,
    // even after the caller closed it and another open reused it.
    std::thread::sleep(LIMIT * 2);

    assert_eq!(file_type(master.as_fd()), FileType::Fifo);
}
