use std::collections::{BTreeSet, HashMap};
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::{SessionName, TerminalSize};
use tokio::runtime::{Builder, Runtime};

use crate::common::tty_size;

const PTY_RESIZE_TIMEOUT: Duration = Duration::from_secs(1);
const PTY_RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const NEW_TTY_TIMEOUT: Duration = Duration::from_secs(2);
const NEW_TTY_POLL_INTERVAL: Duration = Duration::from_millis(20);

static SESSION_ID: AtomicUsize = AtomicUsize::new(0);
static SERIAL_EXECUTION_LOCK: Mutex<()> = Mutex::new(());

pub(super) struct SerialExecutionGuard {
    _local: MutexGuard<'static, ()>,
    file: File,
}

impl Drop for SerialExecutionGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(super) fn runtime() -> Result<Runtime, Box<dyn Error>> {
    Ok(Builder::new_multi_thread().enable_all().build()?)
}

pub(super) fn serialize_test_execution() -> SerialExecutionGuard {
    let local = SERIAL_EXECUTION_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = std::env::temp_dir().join("rmux-live-daemon-tests.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap_or_else(|error| panic!("open live-daemon test lock {}: {error}", path.display()));
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(
        result,
        0,
        "lock live-daemon test gate {}: {}",
        path.display(),
        io::Error::last_os_error()
    );
    SerialExecutionGuard {
        _local: local,
        file,
    }
}

pub(super) fn unique_session_name(prefix: &str) -> SessionName {
    let id = SESSION_ID.fetch_add(1, Ordering::Relaxed);
    SessionName::new(format!("{prefix}-{id}")).expect("generated session names should be valid")
}

pub(super) fn single_new_tty(
    before: &BTreeSet<PathBuf>,
    after: &BTreeSet<PathBuf>,
) -> Result<PathBuf, Box<dyn Error>> {
    let mut current = after.clone();
    let deadline = Instant::now() + NEW_TTY_TIMEOUT;

    loop {
        let new_ttys: Vec<_> = current.difference(before).cloned().collect();
        if let [path] = new_ttys.as_slice() {
            return Ok(path.clone());
        }

        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "expected exactly one new tty, observed {}",
                new_ttys.len()
            ))
            .into());
        }

        thread::sleep(NEW_TTY_POLL_INTERVAL);
        current = crate::common::pane_tty_paths()?;
    }
}

pub(super) fn tty_sizes_by_index(
    tty_paths: &HashMap<u32, PathBuf>,
) -> Result<HashMap<u32, TerminalSize>, Box<dyn Error>> {
    tty_paths
        .iter()
        .map(|(index, path)| tty_size(path).map(|size| (*index, size)))
        .collect()
}

pub(super) fn wait_for_tty_size(
    path: &Path,
    expected: TerminalSize,
) -> Result<TerminalSize, Box<dyn Error>> {
    let deadline = Instant::now() + PTY_RESIZE_TIMEOUT;

    loop {
        let size = tty_size(path)?;
        if size == expected {
            return Ok(size);
        }

        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "tty '{}' never reached size {:?}",
                path.display(),
                expected
            ))
            .into());
        }

        thread::sleep(PTY_RESIZE_POLL_INTERVAL);
    }
}

pub(super) fn assert_valid_non_overlapping_geometry(panes: &[rmux_core::Pane], size: TerminalSize) {
    for pane in panes {
        let geometry = pane.geometry();
        assert!(
            geometry.cols() > 0,
            "pane {} width must stay positive",
            pane.index()
        );
        assert!(
            geometry.rows() > 0,
            "pane {} height must stay positive",
            pane.index()
        );
        assert!(
            geometry.x().saturating_add(geometry.cols()) <= size.cols,
            "pane {} must stay within the terminal width",
            pane.index()
        );
        assert!(
            geometry.y().saturating_add(geometry.rows()) <= size.rows,
            "pane {} must stay within the terminal height",
            pane.index()
        );
    }

    for (left_index, left) in panes.iter().enumerate() {
        for right in &panes[left_index + 1..] {
            assert!(
                !rectangles_overlap(left.geometry(), right.geometry()),
                "pane {} geometry {:?} overlaps pane {} geometry {:?}",
                left.index(),
                left.geometry(),
                right.index(),
                right.geometry()
            );
        }
    }
}

fn rectangles_overlap(left: rmux_core::PaneGeometry, right: rmux_core::PaneGeometry) -> bool {
    let left_end_x = left.x().saturating_add(left.cols());
    let left_end_y = left.y().saturating_add(left.rows());
    let right_end_x = right.x().saturating_add(right.cols());
    let right_end_y = right.y().saturating_add(right.rows());

    left.x() < right_end_x
        && right.x() < left_end_x
        && left.y() < right_end_y
        && right.y() < left_end_y
}
