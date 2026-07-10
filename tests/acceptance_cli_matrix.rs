use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn cli_acceptance_matrix_exercises_real_daemon_state() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cli-matrix")?;
    let session = "acceptance";
    let marker = format!("rmux_acceptance_marker_{}", std::process::id());

    harness.success(["new-session", "-d", "-s", session])?;
    harness.success(["has-session", "-t", session])?;
    harness.success(["split-window", "-h", "-t", &format!("{session}:0.0")])?;

    let panes = harness.stdout(["list-panes", "-t", session, "-F", "#{pane_index}"])?;
    assert!(
        panes.lines().any(|line| line == "0") && panes.lines().any(|line| line == "1"),
        "split-window did not create the expected two panes; list-panes output: {panes:?}"
    );

    harness.success(["send-keys", "-t", &format!("{session}:0.1"), &marker])?;
    harness.wait_for_capture_contains(&format!("{session}:0.1"), &marker)?;

    let config_dir = harness.tmpdir().join("config dir");
    fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("rmux acceptance café.conf");
    fs::write(
        &config_path,
        "set-option -g status off\nset-environment -g RMUX_ACCEPTANCE_MATRIX ok\n",
    )?;

    harness.success_in(
        &config_dir,
        [
            OsStr::new("source-file"),
            OsStr::new("rmux acceptance café.conf"),
        ],
    )?;

    let status = harness.stdout(["show-options", "-gqv", "status"])?;
    assert_eq!(
        status.trim(),
        "off",
        "source-file did not apply status option"
    );

    let env = harness.stdout(["show-environment", "-g", "RMUX_ACCEPTANCE_MATRIX"])?;
    assert_eq!(
        env.trim(),
        "RMUX_ACCEPTANCE_MATRIX=ok",
        "source-file did not apply global environment option"
    );

    let sessions = harness.stdout(["list-sessions", "-F", "#{session_name}"])?;
    assert!(
        sessions.lines().any(|line| line == session),
        "list-sessions did not report created session; output: {sessions:?}"
    );

    Ok(())
}

#[test]
fn kill_server_is_terminal_for_the_cli_command_queue() -> Result<(), Box<dyn Error>> {
    for existing_server in [false, true] {
        let harness = AcceptanceHarness::new(if existing_server {
            "kill-server-terminal-existing"
        } else {
            "kill-server-terminal-absent"
        })?;
        if existing_server {
            harness.success(["new-session", "-d", "-s", "seed"])?;
        }

        harness.success([
            "kill-server",
            ";",
            "new-session",
            "-d",
            "-s",
            "must-not-exist",
        ])?;
        let probe = harness.run(["has-session", "-t", "must-not-exist"])?;
        assert!(
            !probe.status.success(),
            "commands after kill-server must not recreate the daemon"
        );
    }
    Ok(())
}

struct AcceptanceHarness {
    label: String,
    tmpdir: PathBuf,
}

impl AcceptanceHarness {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = unique_id(label);
        let tmpdir = std::env::temp_dir().join(&unique);
        let _ = fs::remove_dir_all(&tmpdir);
        fs::create_dir_all(&tmpdir)?;
        let harness = Self {
            label: unique,
            tmpdir,
        };
        let _ = harness.run(["kill-server"]);
        Ok(harness)
    }

    fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    fn success<I, S>(&self, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)
    }

    fn success_in<I, S>(&self, cwd: &Path, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run_in(cwd, args)?;
        assert_success(&output)
    }

    fn stdout<I, S>(&self, args: I) -> Result<String, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)?;
        Ok(String::from_utf8(output.stdout)?)
    }

    fn run<I, S>(&self, args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_in(Path::new("."), args)
    }

    fn run_in<I, S>(&self, cwd: &Path, args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(rmux_binary());
        command
            .current_dir(cwd)
            .arg("-L")
            .arg(&self.label)
            .args(args);
        Ok(command.output()?)
    }

    fn wait_for_capture_contains(&self, target: &str, needle: &str) -> Result<(), Box<dyn Error>> {
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

    // `capture-pane -p` exposes physical terminal rows.  On Windows a long
    // shell prompt in a split pane can soft-wrap inside text typed with
    // `send-keys`, even though the pane contains the requested bytes in order.
    // Keep the oracle strict about character order, but ignore row breaks that
    // are an artifact of terminal capture rather than process output.
    let unwrapped: String = capture
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n'))
        .collect();
    unwrapped.contains(needle)
}

impl Drop for AcceptanceHarness {
    fn drop(&mut self) {
        let _ = self.run(["kill-server"]);
        let _ = fs::remove_dir_all(&self.tmpdir);
    }
}

fn rmux_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_rmux"))
}

fn assert_success(output: &Output) -> Result<(), Box<dyn Error>> {
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

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    format!("rmux-{label}-{}-{nanos}", std::process::id())
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
        "prompt>rmux_acceptance_marker\n_1234\n",
        "rmux_acceptance_marker_1234"
    ));
    assert!(!capture_contains_terminal_text(
        "prompt>rmux_acceptance_marker\n_wrong\n",
        "rmux_acceptance_marker_1234"
    ));
}
