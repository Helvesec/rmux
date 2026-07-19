use std::fs;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(600);
const ACCESS_DENIED_RETRY_TIMEOUT: Duration = Duration::from_secs(2);
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const LNK1104_RETRY_LIMIT: usize = 3;
const PROCESS_BINARY_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const PREBUILT_RMUX_BINARY_ENV: &str = "RMUX_WINDOWS_SMOKE_RMUX_BIN";
#[allow(dead_code)]
const PRIVATE_TARGET_DIR: &str = "rmux-windows-smoke-build";
#[allow(dead_code)]
const PROCESS_BINARY_DIR: &str = "rmux-windows-smoke-bins";

pub(crate) struct WindowsCargoBuildGuard {
    file: Option<fs::File>,
    path: PathBuf,
}

pub(crate) fn acquire(parent_target_dir: &Path) -> io::Result<WindowsCargoBuildGuard> {
    fs::create_dir_all(parent_target_dir)?;
    let path = parent_target_dir.join("rmux-windows-cargo-build.lock");
    let started = Instant::now();
    let mut retry_started = Instant::now();
    let mut retry_error = None;
    loop {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(windows)]
        options.share_mode(0);

        match options.open(&path) {
            Ok(file) => {
                return Ok(WindowsCargoBuildGuard {
                    file: Some(file),
                    path,
                });
            }
            Err(error) => {
                let Some(retry_timeout) = lock_retry_timeout(&error) else {
                    return Err(error);
                };
                let current_error = error.raw_os_error();
                if retry_error != current_error {
                    retry_error = current_error;
                    retry_started = Instant::now();
                }
                let absolute_timeout_reached = started.elapsed() >= LOCK_WAIT_TIMEOUT;
                let retry_timeout_reached = retry_started.elapsed() >= retry_timeout;
                if absolute_timeout_reached || retry_timeout_reached {
                    if is_share_violation(&error) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "timed out waiting for Windows cargo build lock '{}'",
                                path.display()
                            ),
                        ));
                    }
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "failed to acquire Windows cargo build lock '{}': {error}",
                            path.display(),
                        ),
                    ));
                }
                thread::sleep(LOCK_POLL_INTERVAL);
            }
        }
    }
}

#[allow(dead_code)]
pub(crate) fn private_target_dir(parent_target_dir: &Path) -> PathBuf {
    parent_target_dir.join(PRIVATE_TARGET_DIR)
}

#[allow(dead_code)]
pub(crate) fn prebuilt_rmux_binary() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(PREBUILT_RMUX_BINARY_ENV) else {
        return Ok(None);
    };
    let path = PathBuf::from(value);
    if path.is_file() {
        return Ok(Some(path));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "{PREBUILT_RMUX_BINARY_ENV} points to a missing rmux binary: {}",
            path.display()
        ),
    ))
}

#[allow(dead_code)]
pub(crate) fn copy_binary_for_current_process(
    source: &Path,
    parent_target_dir: &Path,
) -> io::Result<PathBuf> {
    let destination_dir = parent_target_dir.join("debug").join(PROCESS_BINARY_DIR);
    fs::create_dir_all(&destination_dir)?;
    cleanup_stale_process_binaries(&destination_dir);

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let destination = destination_dir.join(format!("rmux-{}-{suffix}.exe", process::id()));
    fs::copy(source, &destination)?;
    Ok(destination)
}

#[allow(dead_code)]
pub(crate) fn run_cargo_build_with_lnk1104_retry<F>(
    mut make_command: F,
) -> io::Result<process::Output>
where
    F: FnMut() -> process::Command,
{
    for attempt in 0..=LNK1104_RETRY_LIMIT {
        let output = make_command().output()?;
        if output.status.success()
            || !output_contains_lnk1104(&output)
            || attempt == LNK1104_RETRY_LIMIT
        {
            return Ok(output);
        }

        let next_attempt = attempt + 2;
        eprintln!(
            "cargo build hit LNK1104; retrying Windows smoke build attempt {next_attempt}/{}",
            LNK1104_RETRY_LIMIT + 1
        );
        thread::sleep(Duration::from_millis(250 * (1_u64 << attempt)));
    }

    unreachable!("bounded retry loop must return from inside the loop")
}

#[allow(dead_code)]
pub(crate) fn emit_command_output(output: &process::Output) -> io::Result<()> {
    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;
    Ok(())
}

fn lock_retry_timeout(error: &io::Error) -> Option<Duration> {
    match error.raw_os_error() {
        Some(32 | 33) => Some(LOCK_WAIT_TIMEOUT),
        Some(5) => Some(ACCESS_DENIED_RETRY_TIMEOUT),
        _ => None,
    }
}

fn is_share_violation(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(32 | 33))
}

fn cleanup_stale_process_binaries(destination_dir: &Path) {
    let Ok(entries) = fs::read_dir(destination_dir) else {
        return;
    };
    let current_process_prefix = format!("rmux-{}-", process::id());
    let stale_cutoff = SystemTime::now()
        .checked_sub(PROCESS_BINARY_STALE_AFTER)
        .unwrap_or(UNIX_EPOCH);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("rmux-") || !file_name.ends_with(".exe") {
            continue;
        }
        let same_process_copy = file_name.starts_with(&current_process_prefix);
        let old_enough = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified <= stale_cutoff);
        if same_process_copy || old_enough {
            let _ = fs::remove_file(path);
        }
    }
}

fn output_contains_lnk1104(output: &process::Output) -> bool {
    String::from_utf8_lossy(&output.stdout).contains("LNK1104")
        || String::from_utf8_lossy(&output.stderr).contains("LNK1104")
}

impl Drop for WindowsCargoBuildGuard {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}
