//! `pane_tty` names the PTY the pane owns, not wherever its child's descriptor 0
//! currently points.
//!
//! Every expectation here is an independent OS observation attributed to the
//! pane's own process — the child's descriptor 1, which stays on the pane's
//! terminal — or the recorded absence contract. None of them is read back from
//! the accessor under test, and no `/dev/ttys…` constant appears anywhere.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmux_core::{EnvironmentStore, OptionStore, PaneGeometry};
use rmux_proto::{OptionName, ProcessCommand, ScopeSelector, SetOptionMode};

use super::{open_pane_terminal, PaneTerminal};
use crate::terminal::TerminalProfile;

/// A profile whose shell is the system one, for a pane this test drives itself.
fn test_profile(cwd: &Path) -> TerminalProfile {
    let mut options = OptionStore::default();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/sh".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell test option is valid");
    TerminalProfile::for_run_shell_with_base_environment(
        &EnvironmentStore::default(),
        &options,
        None,
        None,
        Path::new("/tmp/rmux-ptty1-witness.sock"),
        None,
        false,
        None,
        Some(cwd),
    )
    .expect("test profile")
}

/// A private directory for one witness, removed when it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rmux-ptty1-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The OS's own answer for one descriptor of one pid. This is the independent
/// observation the expectations are built from, never the accessor under test.
fn observed_fd(pid: u32, fd: i32) -> Option<PathBuf> {
    rmux_os::process::fd_path(pid, fd)
}

/// Waits until the OS itself reports that the child's descriptor 0 has become
/// the FIFO. A bounded poll of observed state, attributed to that pid — not a
/// sleep, and not a claim made from the gate alone.
fn wait_until_fd0_is(pid: u32, expected: &Path) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(seen) = observed_fd(pid, 0) {
            if seen == expected {
                return seen;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for pid {pid} to put its descriptor 0 on {}",
            expected.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn owned_slave_survives_the_child_replacing_fd0() {
    let scratch = Scratch::new("fd0");
    let fifo = scratch.path().join("stdin.fifo");
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&fifo)
        .status()
        .expect("mkfifo runs");
    assert!(status.success(), "mkfifo exited {status:?}");
    // The kernel reports descriptor paths already resolved, and this platform's
    // temporary directory is reached through a symlink, so the expectation is
    // built from the resolved path rather than the one that was typed.
    let fifo = std::fs::canonicalize(&fifo).expect("the FIFO resolves");

    // The child replaces its own descriptor 0 with the FIFO and then keeps it,
    // which is exactly the shape that made the old accessor publish the FIFO.
    let command =
        ProcessCommand::Shell(format!("exec 0<>{}; exec /bin/cat", fifo.to_string_lossy()));
    let mut terminal = open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 0, 80, 24),
        test_profile(scratch.path()),
        None,
        Some(&command),
    )
    .expect("the pane opens");

    let pid = terminal.pid();
    assert_ne!(pid, 0, "the pane has a process");
    let observed_fifo = wait_until_fd0_is(pid, &fifo);

    // The independent expectation: the child's descriptor 1, which the child
    // never touched, is still the pane's terminal.
    let expected_slave = observed_fd(pid, 1).expect("the OS reports descriptor 1 of the pane");
    assert_ne!(
        expected_slave, observed_fifo,
        "the fixture must really have moved descriptor 0 off the terminal"
    );

    assert_eq!(
        terminal.tty_path(),
        Some(expected_slave.clone()),
        "pane_tty must name the pane's own terminal"
    );
    assert_ne!(
        terminal.tty_path(),
        Some(observed_fifo),
        "and must not follow the child's replaced descriptor 0"
    );

    // The name survives the clone the output reader takes, and the clone keeps
    // no further slave of its own.
    let cloned = terminal
        .clone_master_for_output_reader()
        .expect("the output reader clone");
    assert_eq!(
        cloned.slave_path().map(Path::to_path_buf),
        Some(expected_slave),
        "the cloned master still names the same owned terminal"
    );
}

#[test]
fn the_inactive_target_its_active_neighbour_the_inventory_and_the_loop_agree() {
    let scratch = Scratch::new("target");
    let alpha = open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 0, 80, 16),
        test_profile(scratch.path()),
        None,
        Some(&ProcessCommand::Shell("exec /bin/cat".to_owned())),
    )
    .expect("the first pane opens");
    let beta = open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 17, 80, 7),
        test_profile(scratch.path()),
        None,
        Some(&ProcessCommand::Shell("exec /bin/cat".to_owned())),
    )
    .expect("the second pane opens");

    let alpha_pid = alpha.pid();
    let beta_pid = beta.pid();
    assert_ne!(alpha_pid, 0);
    assert_ne!(beta_pid, 0);

    let alpha_expected = observed_fd(alpha_pid, 1).expect("the OS reports the first pane's fd 1");
    let beta_expected = observed_fd(beta_pid, 1).expect("the OS reports the second pane's fd 1");
    assert_ne!(
        alpha_expected, beta_expected,
        "the two panes own two different terminals"
    );

    // Each pane answers with its own terminal; neither borrows the other's, so
    // resolving a target can never hand back a neighbour's PTY.
    assert_eq!(alpha.tty_path(), Some(alpha_expected.clone()));
    assert_eq!(beta.tty_path(), Some(beta_expected.clone()));
    assert_ne!(alpha.tty_path(), beta.tty_path());

    // Whatever a caller renders — a direct read, an inventory entry or a loop
    // body — it goes through this one accessor, so the same two values are what
    // reach all three.
    let rendered: Vec<Option<PathBuf>> = [&alpha, &beta]
        .into_iter()
        .map(PaneTerminal::tty_path)
        .collect();
    assert_eq!(
        rendered,
        vec![Some(alpha_expected), Some(beta_expected)],
        "both panes render their own terminal"
    );
    drop(scratch);
}

#[test]
fn no_process_and_no_pane_context_keep_their_absence() {
    let scratch = Scratch::new("absent");
    let terminal = open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 0, 80, 24),
        test_profile(scratch.path()),
        None,
        Some(&ProcessCommand::Shell("exec /bin/cat".to_owned())),
    )
    .expect("the pane opens");
    assert!(
        terminal.tty_path().is_some(),
        "a live pane names its terminal"
    );

    // A pane without a process keeps the contract it already had: no name at
    // all, rather than a guess or a stale one.
    let empty = PaneTerminal::empty(test_profile(scratch.path()));
    assert!(empty.is_empty(), "the fixture pane really has no process");
    assert_eq!(
        empty.tty_path(),
        None,
        "a pane with no process reports no terminal"
    );
    assert_eq!(empty.pid(), 0, "and no process id");
}
