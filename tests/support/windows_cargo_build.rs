use std::fs;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const LOCK_STALE_AFTER: Duration = Duration::from_secs(10 * 60);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct WindowsCargoBuildGuard {
    path: PathBuf,
}

pub(crate) fn acquire() -> io::Result<WindowsCargoBuildGuard> {
    let path = std::env::temp_dir().join("rmux-windows-cargo-build.lock");
    let started = Instant::now();
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(WindowsCargoBuildGuard { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_stale_lock(&path);
                if started.elapsed() >= LOCK_WAIT_TIMEOUT {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "timed out waiting for Windows cargo build lock '{}'",
                            path.display()
                        ),
                    ));
                }
                thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn remove_stale_lock(path: &PathBuf) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    let Ok(modified) = metadata.modified() else {
        return;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return;
    };
    if age >= LOCK_STALE_AFTER {
        let _ = fs::remove_file(path);
    }
}

impl Drop for WindowsCargoBuildGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
