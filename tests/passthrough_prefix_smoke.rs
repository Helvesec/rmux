#![cfg(unix)]
//! End-to-end smoke tests for prefix bindings: real daemon, real
//! `rmux attach-session` binary, real PTY, optionally wrapped in a
//! launcher shell.
//!
//! Lower layers (in-process socket protocol, lib-level dispatch) are
//! covered in `crates/rmux-server/tests/attach_session/prefix_smoke.rs`
//! and `crates/rmux-server/src/handler_attach_tests/passthrough_input.rs`.
//! This file exists for the cross-binary path the user actually
//! exercises — keystroke → PTY driver → `rmux` client → IPC →
//! daemon → IPC → client → PTY → assertion harness.
//!
//! The user-reported "Ctrl-B w does nothing" arrives here.  These
//! tests parameterise the *launcher* shell (`bash`, `zsh`, `dash`,
//! `sh`) — what a real user uses to invoke `rmux attach-session`.
//! Anything missing from the system is skipped so the suite stays
//! useful on minimal sandboxes.
//!
//! Assertion strategy: query the server via `display-message -p
//! '#{pane_in_mode}'`.  Reading bytes off the PTY for an alt-screen
//! marker is mode-specific (passthrough wraps overlays in host
//! alt-screen brackets; normal mode renders inside its own surface).
//! The mode-status query is mode-agnostic and tells us whether the
//! binding actually dispatched — the question the user is really
//! asking.

mod common;

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{
    drain_attach_output, read_until_contains, stdout, terminate_child, AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SHELL_PROMPT_MARKER: &str = "tester@RMUXHOST";

type TestResult = Result<(), Box<dyn Error>>;

#[derive(Debug, Clone, Copy)]
enum SessionMode {
    Normal,
    Passthrough,
}

impl SessionMode {
    fn new_session_args(self) -> &'static [&'static str] {
        match self {
            Self::Normal => &["new-session", "-d", "-s", "alpha"],
            Self::Passthrough => &["new-session", "--passthrough", "-d", "-s", "alpha"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Passthrough => "passthrough",
        }
    }
}

/// Resolve a shell on `$PATH` by name.  Returns `None` if absent so
/// individual cases can skip rather than fail in minimal envs.
fn find_shell(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// Poll `display-message -p '#{pane_in_mode}'` on `alpha:0.0` via a
/// short-lived non-attach client until it reports `expected`.
///
/// This goes through a *different* IPC connection from the attach
/// stream — so it bypasses anything weird about the client's output
/// thread and asks the daemon directly "did the binding fire?".
fn wait_for_pane_in_mode(
    harness: &CliHarness,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut last_seen = String::new();
    while Instant::now() < deadline {
        let output = harness.run(&[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}",
        ])?;
        if output.status.success() {
            last_seen = stdout(&output).trim().to_owned();
            if last_seen == expected {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    Err(format!("pane_in_mode never became {expected:?} (last seen {last_seen:?})").into())
}

fn run_ctrl_b_w_smoke_with_shell(
    label_suffix: &str,
    mode: SessionMode,
    shell: Option<&std::path::Path>,
) -> TestResult {
    let label = format!("ctrl-b-w-{}-{}", mode.label(), label_suffix);
    let harness = CliHarness::new(&label)?;
    let mut daemon = harness.start_hidden_daemon()?;

    let create = harness.run(mode.new_session_args())?;
    assert!(
        create.status.success(),
        "{} ({}): new-session failed: status={:?} stderr={:?}",
        label,
        mode.label(),
        create.status,
        String::from_utf8_lossy(&create.stderr)
    );

    let mut attach = match shell {
        None => AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?,
        Some(shell_path) => AttachedSession::spawn_via_shell(
            &harness,
            "alpha",
            TerminalSize::new(120, 40),
            shell_path,
        )?,
    };
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    // Wait for the inner shell's prompt to ensure the session has
    // fully settled.  Best-effort: passthrough environments use a
    // different inner prompt path.  The prefix dispatch doesn't
    // depend on the inner shell being ready, so we proceed even if
    // the marker doesn't appear within the timeout.
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT);
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;

    wait_for_pane_in_mode(&harness, "1", IO_TIMEOUT).map_err(|err| -> Box<dyn Error> {
        format!(
            "Ctrl-B w on a {} session{} did not enter tree-mode on the server side. \
             This is the user-reported bug expressed at the cross-binary boundary. \
             Underlying: {err}",
            mode.label(),
            shell
                .map(|s| format!(" (launcher shell: {})", s.display()))
                .unwrap_or_default(),
        )
        .into()
    })?;

    // Cleanup — leave choose-tree, detach, and reap.
    let _ = attach.send_bytes(b"q");
    std::thread::sleep(Duration::from_millis(50));
    let _ = attach.send_bytes(b"\x02d");
    let _ = attach.wait_for_exit(IO_TIMEOUT);
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Baseline (no launcher shell): the test-built `rmux` binary is the
/// PTY foreground process directly.  Passes on every host that has
/// the daemon working — narrows future failures to the
/// shell-wrapping path.
#[test]
fn ctrl_b_w_dispatches_on_normal_session_no_launcher_shell() -> TestResult {
    run_ctrl_b_w_smoke_with_shell("no-shell", SessionMode::Normal, None)
}

#[test]
fn ctrl_b_w_dispatches_on_passthrough_session_no_launcher_shell() -> TestResult {
    run_ctrl_b_w_smoke_with_shell("no-shell", SessionMode::Passthrough, None)
}

/// Parameterised over POSIX shells: each one launches `rmux
/// attach-session` via `<shell> -c 'exec rmux attach-session …'`,
/// which is what an interactive user invocation collapses to.  Any
/// shell missing on the system is skipped (printed) rather than
/// failing the test.
macro_rules! ctrl_b_w_shell_test {
    ($fn_name:ident, $mode:expr, $shell:literal) => {
        #[test]
        fn $fn_name() -> TestResult {
            let Some(shell_path) = find_shell($shell) else {
                eprintln!("[skip] {} not on $PATH", $shell);
                return Ok(());
            };
            run_ctrl_b_w_smoke_with_shell($shell, $mode, Some(&shell_path))
        }
    };
}

ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_normal_session_via_bash,
    SessionMode::Normal,
    "bash"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_passthrough_session_via_bash,
    SessionMode::Passthrough,
    "bash"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_normal_session_via_zsh,
    SessionMode::Normal,
    "zsh"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_passthrough_session_via_zsh,
    SessionMode::Passthrough,
    "zsh"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_normal_session_via_dash,
    SessionMode::Normal,
    "dash"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_passthrough_session_via_dash,
    SessionMode::Passthrough,
    "dash"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_normal_session_via_sh,
    SessionMode::Normal,
    "sh"
);
ctrl_b_w_shell_test!(
    ctrl_b_w_dispatches_on_passthrough_session_via_sh,
    SessionMode::Passthrough,
    "sh"
);
