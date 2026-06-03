use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use tokio::sync::{Mutex, MutexGuard};

pub struct LiveDaemonLock {
    local: Mutex<()>,
}

pub struct LiveDaemonGuard {
    _local: MutexGuard<'static, ()>,
    file: File,
}

impl LiveDaemonLock {
    pub const fn new() -> Self {
        Self {
            local: Mutex::const_new(()),
        }
    }

    pub async fn lock(&'static self) -> LiveDaemonGuard {
        let local = self.local.lock().await;
        let path = lock_path();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap_or_else(|error| {
                panic!("open SDK live-daemon lock {}: {error}", path.display())
            });
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        assert_eq!(
            result,
            0,
            "lock SDK live-daemon gate {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        LiveDaemonGuard {
            _local: local,
            file,
        }
    }
}

impl Drop for LiveDaemonGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn lock_path() -> PathBuf {
    std::env::temp_dir().join("rmux-live-daemon-tests.lock")
}
