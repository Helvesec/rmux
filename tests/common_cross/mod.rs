#![allow(dead_code)]

use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static UNIQUE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct CrossPlatformHarness {
    label: String,
    tmpdir: PathBuf,
}

impl CrossPlatformHarness {
    pub(crate) fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = unique_id(label);
        let tmpdir = temp_root().join(&unique);
        let _ = fs::remove_dir_all(&tmpdir);
        fs::create_dir_all(&tmpdir)?;
        fs::create_dir_all(tmpdir.join("home"))?;
        fs::create_dir_all(tmpdir.join("xdg"))?;
        let harness = Self {
            label: unique,
            tmpdir,
        };
        let _ = harness.run(["kill-server"]);
        Ok(harness)
    }

    pub(crate) fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    pub(crate) fn success<I, S>(&self, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)
    }

    pub(crate) fn stdout<I, S>(&self, args: I) -> Result<String, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)?;
        Ok(String::from_utf8(output.stdout)?)
    }

    pub(crate) fn run<I, S>(&self, args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(rmux_binary());
        command.arg("-L").arg(&self.label).args(args);
        command.env("HOME", self.tmpdir.join("home"));
        command.env("XDG_CONFIG_HOME", self.tmpdir.join("xdg"));
        command.env("RMUX_TMPDIR", &self.tmpdir);
        command.env("RMUX_DISABLE_TMUX_FALLBACK", "1");
        command.env_remove("RMUX");
        command.env_remove("TMUX");
        command.env_remove("RMUX_INTERNAL_BINARY_PATH");
        Ok(command.output()?)
    }

    pub(crate) fn wait_for_capture_contains(
        &self,
        target: &str,
        needle: &str,
    ) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last = String::new();
        while Instant::now() < deadline {
            last = self.stdout(["capture-pane", "-p", "-t", target])?;
            if capture_contains_terminal_text(&last, needle) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "capture-pane for target {target} did not contain {needle:?}; last capture: {last:?}"
        )
        .into())
    }
}

fn capture_contains_terminal_text(capture: &str, needle: &str) -> bool {
    if capture.contains(needle) {
        return true;
    }

    let unwrapped: String = capture
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n'))
        .collect();
    unwrapped.contains(needle)
}

impl Drop for CrossPlatformHarness {
    fn drop(&mut self) {
        let _ = self.run(["kill-server"]);
        let _ = fs::remove_dir_all(&self.tmpdir);
    }
}

pub(crate) fn assert_success(output: &Output) -> Result<(), Box<dyn Error>> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "rmux command failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

pub(crate) fn rmux_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_rmux"))
}

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    let counter = UNIQUE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let label_hash = label.bytes().fold(0u16, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u16)
    });
    let suffix = nanos % 1_000_000_000;
    format!(
        "rx-{}-{label_hash:04x}-{counter}-{suffix}",
        std::process::id()
    )
    .chars()
    .map(|ch| {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            ch
        } else {
            '-'
        }
    })
    .collect()
}

#[test]
fn capture_contains_terminal_text_accepts_soft_wrapped_needles() {
    assert!(capture_contains_terminal_text(
        "prompt>rename_capture_marker\n_1234\n",
        "rename_capture_marker_1234"
    ));
    assert!(!capture_contains_terminal_text(
        "prompt>rename_capture_marker\n_wrong\n",
        "rename_capture_marker_1234"
    ));
}

fn temp_root() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/tmp")
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}
