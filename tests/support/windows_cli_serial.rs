#![cfg(windows)]

use std::fs::{remove_file, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const LOCK_STALE_AFTER: Duration = Duration::from_secs(10 * 60);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) struct WindowsCliSerialGuard {
    path: PathBuf,
}

pub(crate) fn acquire(label: &str) -> io::Result<WindowsCliSerialGuard> {
    let path = std::env::temp_dir().join("rmux-windows-cli-integration.lock");
    let deadline = Instant::now() + LOCK_WAIT_TIMEOUT;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                use std::io::Write as _;
                writeln!(&file, "pid={} label={label}", std::process::id())?;
                return Ok(WindowsCliSerialGuard { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_stale_lock(&path);
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out waiting for Windows CLI integration lock '{}'",
                            path.display()
                        ),
                    ));
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

fn remove_stale_lock(path: &Path) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    let Ok(modified) = metadata.modified() else {
        return;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return;
    };
    if age >= LOCK_STALE_AFTER {
        let _ = remove_file(path);
    }
}

impl Drop for WindowsCliSerialGuard {
    fn drop(&mut self) {
        let _ = remove_file(&self.path);
    }
}
