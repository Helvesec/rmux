#![cfg(unix)]
//! End-to-end attach-flow tests for `--passthrough` sessions.
//!
//! These exercise behaviour reported by humans actually using
//! `rmux new-session --passthrough` interactively, where the
//! server-side forwarder unit tests can't see the symptom.

mod common;

use std::error::Error;
use std::time::Duration;

use common::{
    drain_attach_output, read_until_contains, terminate_child, AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SHELL_PROMPT_MARKER: &str = "tester@RMUXHOST";

type TestResult = Result<(), Box<dyn Error>>;

/// Invariant: a plain stdout-printing command run inside the inner
/// shell of a passthrough session must have its output forwarded
/// verbatim to the attached client.
///
/// Background: a user reported that `rmux --help` typed at their
/// real interactive prompt produced no visible output. Tracing
/// proved the help DID flow through — but their custom zsh prompt
/// emits `\x1b[H\x1b[2J` between commands, so the host terminal
/// dutifully wiped what passthrough had just forwarded. This test
/// pins down the rmux-side half: in a sterile shell environment
/// the help text reaches the client. Regressions in the forwarder
/// (e.g. if we ever started swallowing stdout bytes) would show up
/// here, separated from prompt-config side effects.
#[test]
fn rmux_help_inside_passthrough_session_forwards_stdout_to_client() -> TestResult {
    let harness = CliHarness::new("passthrough-rmux-help")?;
    let mut daemon = harness.start_hidden_daemon()?;

    // Note: don't use assert_success here — the detached-create
    // status banner currently goes to stderr in non-TTY contexts
    // (the /dev/tty write falls back to eprintln when there's no
    // controlling terminal). That's an orthogonal wart; check exit
    // code only.
    let create = harness.run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?;
    assert!(
        create.status.success(),
        "new-session --passthrough -d exited non-zero: status={:?} stderr={:?}",
        create.status,
        String::from_utf8_lossy(&create.stderr)
    );

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    // Wait for the inner shell's first prompt so we know it's ready
    // for keystrokes (otherwise input lands in the cooked-mode
    // buffer before the shell starts reading and is silently lost).
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Use the exact test-built rmux binary so PATH lookup inside the
    // sandboxed inner shell can't get in the way of what we're
    // trying to test (the attach-forwarder behaviour on inner-PTY
    // output, not which `rmux` the user's shell happens to find).
    let rmux_path = env!("CARGO_BIN_EXE_rmux");
    let command = format!("{rmux_path} --help\r");
    attach.send_bytes(command.as_bytes())?;

    let output = read_until_contains(attach.master_mut(), "usage: rmux", IO_TIMEOUT)
        .map_err(|err| format!("`rmux --help` produced no visible output inside attach: {err}"))?;

    // The help line is the contract: anything starting with
    // "usage: rmux" is the tmux-compat usage banner.
    assert!(
        output.contains("usage: rmux"),
        "expected `rmux --help` usage banner in attach output, got: {output:?}"
    );

    // Detach cleanly and verify the attach client exits 0.
    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Reported bug (interactive): in a passthrough attach the screen
/// resets between commands — e.g. typing `rmux --help` at the prompt
/// briefly shows the help then everything is wiped.
///
/// Root cause: the daemon sends `AttachControl::Switch` to attached
/// clients for many non-switch reasons (resize, focus change, status
/// refresh). `switch_passthrough_target` treats *every* such message
/// as a real window switch and re-emits the rmux title sequence + a
/// full `\x1b[m\x1b[H\x1b[2J` reset, wiping whatever the inner
/// program just rendered.
///
/// Contract this test pins: the passthrough forwarder MUST NOT emit
/// `\x1b[2J` (clear screen) into a single-window single-pane session
/// where no real window switch ever happens. Any clear we observe in
/// the byte stream came from the server (the inner shell's own
/// output was already verified clear-free in the trace that produced
/// this test).
#[test]
fn passthrough_attach_never_emits_clear_screen_for_same_pane_refresh() -> TestResult {
    let harness = CliHarness::new("passthrough-no-clear-on-refresh")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let create = harness.run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?;
    assert!(create.status.success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Print a sentinel through the inner shell, then drain everything
    // we've seen so we can isolate the next phase's bytes.
    assert!(harness
        .run(&[
            "send-keys",
            "-t",
            "alpha",
            "printf SENTINEL_BEFORE",
            "Enter",
        ])?
        .status
        .success());
    let _ = read_until_contains(attach.master_mut(), "SENTINEL_BEFORE", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Trigger a refresh by resizing the client. tmux/rmux respond to
    // a client resize by re-syncing the attach state, which in our
    // case feeds `AttachControl::Switch` to the forwarder.
    attach.resize(TerminalSize::new(132, 50))?;
    // Give the daemon a beat to process and emit the refresh.
    std::thread::sleep(Duration::from_millis(300));
    let post_refresh_bytes = common::drain_attach_output_bytes(attach.master_mut())?;

    assert!(
        !contains_bytes(&post_refresh_bytes, b"\x1b[2J"),
        "passthrough refresh emitted CSI 2J (clear screen) — that wipes the user's \
         terminal between commands. bytes seen: {:?}",
        String::from_utf8_lossy(&post_refresh_bytes),
    );
    assert!(
        !contains_bytes(&post_refresh_bytes, b"\x1b]0;rmux: alpha\x07"),
        "passthrough refresh re-emitted the rmux title sequence — that's a marker \
         the forwarder treated a refresh as a real window switch. bytes seen: {:?}",
        String::from_utf8_lossy(&post_refresh_bytes),
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Reported bug (interactive): create a passthrough session, exit
/// it, then create another passthrough session — the second one
/// spontaneously detaches after a few seconds with no user input.
///
/// Root cause: when the last session exits,
/// `queue_shutdown_if_server_empty` sets `shutdown_requested = true`.
/// `request_shutdown_if_pending` doesn't fire immediately because
/// the retained-exited-outputs cache holds it back for ~5s. The
/// user creates a new session inside that window. The flag stays
/// set. When the retained-outputs TTL eventually expires, the
/// shutdown fires anyway — even though the server now has an
/// active session and attach — and the new attach gets detached.
///
/// Fix: cancel any queued shutdown when a new session is created.
#[test]
fn passthrough_second_session_after_exit_does_not_spontaneously_detach() -> TestResult {
    let harness = CliHarness::new("passthrough-second-session-survives")?;
    let mut daemon = harness.start_hidden_daemon()?;

    // Session 1: create, attach via PTY, kill it, wait for client to
    // notice. This mirrors the user's "rmux new-session --passthrough"
    // -> "exit" sequence and primes the shutdown-pending flag.
    let create_first = harness.run(&["new-session", "--passthrough", "-d", "-s", "first"])?;
    assert!(create_first.status.success());
    let mut attach_first = AttachedSession::spawn(&harness, "first", TerminalSize::new(120, 40))?;
    attach_first.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach_first.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    // Kill the session to fully drop it (Detach alone wouldn't empty
    // the server). This mirrors the inner `exit` typed by the user.
    let _ = harness.run(&["kill-session", "-t", "first"]);
    let _ = attach_first.wait_for_exit(IO_TIMEOUT)?;

    // The shutdown-pending flag is now set. Give the daemon a beat
    // — but stay under the retained-outputs TTL so the shutdown
    // hasn't fired yet on its own.
    std::thread::sleep(Duration::from_millis(800));

    // Session 2: same flow. Under the unfixed code path, this attach
    // gets detached within a few seconds when the stale flag fires.
    let create_second = harness.run(&["new-session", "--passthrough", "-d", "-s", "second"])?;
    assert!(
        create_second.status.success(),
        "creating second passthrough session failed: stderr={:?}",
        String::from_utf8_lossy(&create_second.stderr)
    );
    let mut attach_second = AttachedSession::spawn(&harness, "second", TerminalSize::new(120, 40))?;
    attach_second.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach_second.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Wait past the typical retained-outputs TTL (~5s) and verify
    // the second attach is still alive — not detached out from
    // under us by the stale shutdown flag.
    std::thread::sleep(Duration::from_secs(6));
    assert!(
        attach_second.child_mut().try_wait()?.is_none(),
        "second passthrough attach spontaneously exited — stale shutdown flag fired \
         despite the new session existing. This is the bug."
    );

    // Clean up: detach by sending Ctrl-B d, then kill the session.
    attach_second.send_bytes(b"\x02d")?;
    let status = attach_second.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    let _ = harness.run(&["kill-session", "-t", "second"]);

    terminate_child(daemon.child_mut())?;
    Ok(())
}
