use super::*;
use crate::test_rerun::rerun_exact_with;
use std::cell::Cell;

thread_local! {
    static FAIL_SETUP: Cell<bool> = const { Cell::new(false) };
    static FAILED_FD: Cell<RawFd> = const { Cell::new(-1) };
}

pub(super) fn fail_close_on_exec_if_requested(fd: RawFd) -> io::Result<()> {
    if FAIL_SETUP.with(|flag| flag.replace(false)) {
        FAILED_FD.with(|slot| slot.set(fd));
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(())
}

fn isolated(name: &str, scenario: impl FnOnce()) {
    const CHILD: &str = "RMUX_SIGNAL_FD_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        scenario();
        return;
    }
    let executable =
        crate::build_identity::resolve(std::env::current_exe().unwrap().as_os_str()).unwrap();
    rerun_exact_with(&executable, name, |command| {
        command.env(CHILD, name);
    });
}

fn flags(fd: RawFd, command: libc::c_int) -> libc::c_int {
    // SAFETY: fcntl only observes the supplied descriptor and returns EBADF for closed fds.
    unsafe { libc::fcntl(fd, command) }
}

fn assert_closed(fd: RawFd) {
    assert_eq!(flags(fd, libc::F_GETFD), -1, "watcher fd {fd} leaked");
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
}

fn assert_global_pipe_usable() {
    let read = SIGNAL_WAKE_READ_FD.load(Ordering::SeqCst);
    let write = SIGNAL_WAKE_WRITE_FD.load(Ordering::SeqCst);
    assert_ne!(flags(read, libc::F_GETFD), -1);
    assert_ne!(flags(write, libc::F_GETFD), -1);
    // SAFETY: this isolated child installed RMUX's SIGUSR1 handler.
    assert_eq!(unsafe { libc::raise(libc::SIGUSR1) }, 0);
    assert_eq!(
        poll_server_signal_flags(),
        Some(PolledServerSignal::RecreateSocket)
    );
    assert_eq!(poll_server_signal_flags(), None);
    let mut byte = [0_u8];
    // SAFETY: read is a live nonblocking pipe and byte is writable storage.
    assert_eq!(unsafe { libc::read(read, byte.as_mut_ptr().cast(), 1) }, 1);
}

#[test]
fn first_watcher_owns_a_separate_reader() {
    isolated(
        "signals::tests::first_watcher_owns_a_separate_reader",
        || {
            let watcher = install_server_signal_flags().unwrap();
            let fd = watcher.read_fd.as_raw_fd();
            let global = SIGNAL_WAKE_READ_FD.load(Ordering::SeqCst);
            drop(watcher);
            assert_closed(fd);
            assert_ne!(fd, global);
            assert_global_pipe_usable();
        },
    );
}

#[test]
fn reinstall_and_drop_closes_each_reader() {
    isolated(
        "signals::tests::reinstall_and_drop_closes_each_reader",
        || {
            drop(install_server_signal_flags().unwrap());
            for iteration in 0..64 {
                let watcher = install_server_signal_flags().unwrap();
                let reader = watcher.into_read_fd();
                let fd = reader.as_raw_fd();
                assert_ne!(fd, SIGNAL_WAKE_READ_FD.load(Ordering::SeqCst));
                assert_ne!(flags(fd, libc::F_GETFD) & libc::FD_CLOEXEC, 0);
                assert_ne!(flags(fd, libc::F_GETFL) & libc::O_NONBLOCK, 0);
                assert_eq!(reader.as_fd().as_raw_fd(), fd);
                drop(reader);
                assert_closed(fd);
                if iteration == 63 {
                    assert_global_pipe_usable();
                }
            }
        },
    );
}

#[test]
fn setup_error_after_dup_closes_reader() {
    isolated(
        "signals::tests::setup_error_after_dup_closes_reader",
        || {
            drop(install_server_signal_flags().unwrap());
            FAIL_SETUP.with(|flag| flag.set(true));
            let error = install_server_signal_flags().unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EIO));
            assert_closed(FAILED_FD.with(Cell::get));
            assert_global_pipe_usable();
        },
    );
}

#[test]
fn first_setup_error_after_dup_closes_reader() {
    isolated(
        "signals::tests::first_setup_error_after_dup_closes_reader",
        || {
            FAIL_SETUP.with(|flag| flag.set(true));
            let error = install_server_signal_flags().unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EIO));
            assert_closed(FAILED_FD.with(Cell::get));
            assert_global_pipe_usable();
            let watcher = install_server_signal_flags().unwrap();
            let fd = watcher.read_fd.as_raw_fd();
            drop(watcher);
            assert_closed(fd);
        },
    );
}
