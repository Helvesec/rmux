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

/// Behavioural contract: running `rmux switch-client -t B` from
/// inside a passthrough attach on session A must repoint the
/// forwarder to B's pane (the host terminal starts showing B), and
/// must NOT kill session A — it stays around as a detached session
/// the user can return to. This is what makes `rmux new-session
/// --passthrough` inside another passthrough behave like a tmux
/// switch (the nested CLI hits exactly this path via
/// `detect_context() == Nested`).
///
/// Trigger we observe: the forwarder's `passthrough_title_sequence`
/// for B (`\x1b]0;rmux: B\x07`) must appear in the attach byte
/// stream after the switch — proof that `switch_passthrough_target`
/// fired with the new target.
#[test]
fn switch_client_into_other_passthrough_session_actually_swaps_target() -> TestResult {
    let harness = CliHarness::new("passthrough-switch-actually-swaps")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let create_alpha = harness.run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?;
    assert!(create_alpha.status.success());
    let create_beta = harness.run(&["new-session", "--passthrough", "-d", "-s", "beta"])?;
    assert!(create_beta.status.success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let initial = read_until_contains(attach.master_mut(), "rmux: alpha", IO_TIMEOUT)?;
    assert!(
        initial.contains("rmux: alpha"),
        "initial attach must emit alpha's tagged title; got {initial:?}",
    );
    drain_attach_output(attach.master_mut())?;

    // Single attached client → switch-client without --target-client
    // routes to that attach (resolve_managed_client fallback).
    let switch = harness.run(&["switch-client", "-t", "beta"])?;
    assert!(
        switch.status.success(),
        "switch-client -t beta failed: stderr={:?}",
        String::from_utf8_lossy(&switch.stderr)
    );

    // Forwarder must repaint with beta's title — that's the proof
    // that switch_passthrough_target ran with the new target.
    let after = read_until_contains(attach.master_mut(), "rmux: beta", IO_TIMEOUT)?;
    assert!(
        after.contains("rmux: beta"),
        "switch-client must cause the passthrough forwarder to emit beta's tagged \
         title; got {after:?}",
    );

    // Outer session must survive — switching away is not a kill.
    let ls = harness.run(&["list-sessions", "-F", "#{session_name}"])?;
    let listed = String::from_utf8_lossy(&ls.stdout);
    assert!(
        listed.contains("alpha"),
        "switching away from alpha must not destroy it; list-sessions: {listed:?}",
    );
    assert!(
        listed.contains("beta"),
        "beta must still exist after switch; list-sessions: {listed:?}",
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    let _ = harness.run(&["kill-session", "-t", "beta"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
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

// ---------------------------------------------------------------
// Multi-window passthrough tests
//
// Contract under test: `--passthrough` supports MULTIPLE windows
// per session even though it forbids multiple panes per window.
// The user-facing promise is: `Ctrl-B 0` / `Ctrl-B 1` (and the
// equivalent `select-window` CLI) flick between windows in the
// host terminal, each one's output flowing verbatim through the
// same client connection, the other window's pane staying alive
// in the background.
// ---------------------------------------------------------------

/// Sanity: `new-window -t S -d <command>` adds a second window to
/// a passthrough session and `list-windows` reports two windows.
/// This is the prerequisite for every test below.
#[test]
fn passthrough_session_supports_two_windows_via_new_window() -> TestResult {
    let harness = CliHarness::new("passthrough-multi-new-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    let create = harness.run(&["new-session", "--passthrough", "-d", "-s", "multi"])?;
    assert!(
        create.status.success(),
        "create failed: {:?}",
        String::from_utf8_lossy(&create.stderr)
    );
    let add = harness.run(&[
        "new-window",
        "-t",
        "multi",
        "-d",
        "sh",
        "-c",
        "printf WINDOW1-READY\\n; exec /bin/sh",
    ])?;
    assert!(
        add.status.success(),
        "new-window in passthrough failed: {:?}",
        String::from_utf8_lossy(&add.stderr)
    );

    let listed = harness.run(&[
        "list-windows",
        "-t",
        "multi",
        "-F",
        "#{window_index}:#{window_active}",
    ])?;
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        stdout.contains("0:1") && stdout.contains("1:0"),
        "expected window 0 active and window 1 inactive after `new-window -d`; got: {stdout:?}",
    );

    let _ = harness.run(&["kill-session", "-t", "multi"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// `rmux select-window -t S:1` while attached must repoint the
/// passthrough forwarder onto window 1's pane (proof: window 1's
/// tagged title `rmux: S` appears AND the marker we seeded in
/// window 1 shows up via replay).
#[test]
fn passthrough_select_window_via_cli_repoints_attached_forwarder() -> TestResult {
    let harness = CliHarness::new("passthrough-select-window-cli")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "multi"])?
        .status
        .success());
    // Seed window 1 with a unique marker its shell prints on startup.
    // Sleeping a beat after the marker keeps the shell alive so the
    // pane stays open across our test.
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "multi",
            "-d",
            "sh",
            "-c",
            "printf WINDOW-ONE-MARKER\\n; exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "multi", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    // Wait for window 0's shell prompt to anchor "we're on window 0".
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    let switch = harness.run(&["select-window", "-t", "multi:1"])?;
    assert!(
        switch.status.success(),
        "select-window failed: {:?}",
        String::from_utf8_lossy(&switch.stderr)
    );

    // Window 1's marker must appear in the attach stream — that's
    // the proof its replay log was emitted by switch_passthrough_target.
    let after = read_until_contains(attach.master_mut(), "WINDOW-ONE-MARKER", IO_TIMEOUT)?;
    assert!(
        after.contains("WINDOW-ONE-MARKER"),
        "after select-window the forwarder must replay window 1's marker; got {after:?}",
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    let _ = harness.run(&["kill-session", "-t", "multi"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// User-keyboard flow: with two windows live, the user pressing
/// `Ctrl-B 1` in the attach client (\x02 then '1') must move the
/// active window to 1, and `Ctrl-B 0` must move it back.
///
/// Verification: after each switch, the *active* window per
/// `list-windows` must match what we just selected.
#[test]
fn passthrough_prefix_key_window_select_changes_active_window() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-window-select")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "multi"])?
        .status
        .success());
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "multi",
            "-d",
            "sh",
            "-c",
            "printf WINDOW-ONE-MARKER\\n; exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "multi", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Ctrl-B 1
    attach.send_bytes(b"\x021")?;
    // Wait for the window 1 marker via replay.
    let after_one = read_until_contains(attach.master_mut(), "WINDOW-ONE-MARKER", IO_TIMEOUT)?;
    assert!(
        after_one.contains("WINDOW-ONE-MARKER"),
        "Ctrl-B 1 must land on window 1 and replay its content; got {after_one:?}",
    );

    let active_after_one = active_window_index(&harness, "multi")?;
    assert_eq!(
        active_after_one, 1,
        "list-windows must report window 1 active after Ctrl-B 1; got {active_after_one}",
    );

    // Ctrl-B 0
    attach.send_bytes(b"\x020")?;
    // After switching back the active marker should be 0; the
    // replay should re-paint window 0's shell prompt area.
    std::thread::sleep(Duration::from_millis(300));
    let active_after_zero = active_window_index(&harness, "multi")?;
    assert_eq!(
        active_after_zero, 0,
        "list-windows must report window 0 active after Ctrl-B 0; got {active_after_zero}",
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;
    let _ = harness.run(&["kill-session", "-t", "multi"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Flick between two windows several times. Both windows must
/// stay alive — their panes should NOT die from being
/// foregrounded/backgrounded — and each switch must land on the
/// requested window.
#[test]
fn passthrough_repeated_window_switches_keep_both_panes_alive() -> TestResult {
    let harness = CliHarness::new("passthrough-flick-windows")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "multi"])?
        .status
        .success());
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "multi",
            "-d",
            "sh",
            "-c",
            "printf WINDOW-ONE-MARKER\\n; exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "multi", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Sequence: 1, 0, 1, 0, 1, 0 (six switches)
    for round in 0..3 {
        attach.send_bytes(b"\x021")?;
        std::thread::sleep(Duration::from_millis(200));
        let active = active_window_index(&harness, "multi")?;
        assert_eq!(
            active, 1,
            "round {round}: expected active window 1 after Ctrl-B 1, got {active}"
        );
        attach.send_bytes(b"\x020")?;
        std::thread::sleep(Duration::from_millis(200));
        let active = active_window_index(&harness, "multi")?;
        assert_eq!(
            active, 0,
            "round {round}: expected active window 0 after Ctrl-B 0, got {active}"
        );
    }

    // Both windows must still be present.
    let listed = harness.run(&["list-windows", "-t", "multi", "-F", "#{window_index}"])?;
    let stdout = String::from_utf8_lossy(&listed.stdout);
    let indexes: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        indexes,
        vec!["0", "1"],
        "both windows must still exist after repeated switching; got: {stdout:?}",
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    let _ = harness.run(&["kill-session", "-t", "multi"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Replay log on round-trip: output emitted in window 0 *before*
/// switching away must be visible again when we switch back to it
/// from window 1. This is the contract that makes passthrough
/// window-switching feel non-destructive.
///
/// History: this test originally failed because the pane reader
/// captures its `Option<SharedPassthroughReplayLog>` at spawn time
/// and the CLI's old flow did `new-session` then `set-option`
/// (two requests) — so window 0's reader was already spawned with
/// `replay_log = None`. Fixed by passing `passthrough: bool` in
/// the `NewSessionExt` request so the option is set server-side
/// *before* the initial pane's reader spawns.
#[test]
fn passthrough_window_zero_replay_contains_pre_switch_output_on_round_trip() -> TestResult {
    let harness = CliHarness::new("passthrough-replay-roundtrip")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "multi"])?
        .status
        .success());
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "multi",
            "-d",
            "sh",
            "-c",
            "printf WINDOW-ONE-MARKER\\n; exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "multi", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Type into window 0's shell — output goes through the attach.
    attach.send_bytes(b"printf SCROLLBACK_SENTINEL_VALUE\n")?;
    let _ = read_until_contains(attach.master_mut(), "SCROLLBACK_SENTINEL_VALUE", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Round-trip via window 1.
    attach.send_bytes(b"\x021")?;
    let _ = read_until_contains(attach.master_mut(), "WINDOW-ONE-MARKER", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;
    attach.send_bytes(b"\x020")?;

    // When we land back on window 0, the replay must include the
    // sentinel we printed earlier — otherwise host-terminal users
    // lose their previous window's visible state on every flick.
    let after = read_until_contains(attach.master_mut(), "SCROLLBACK_SENTINEL_VALUE", IO_TIMEOUT)
        .map_err(|err| format!("window 0 replay missing pre-switch output: {err}"))?;
    assert!(
        after.contains("SCROLLBACK_SENTINEL_VALUE"),
        "window 0 replay on return must contain prior shell output; got {after:?}",
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    let _ = harness.run(&["kill-session", "-t", "multi"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// `Ctrl-B c` from the attach client must create a second window
/// and switch to it. This is the keyboard-driven counterpart to
/// `rmux new-window` and is the most common interactive flow.
#[test]
fn passthrough_prefix_c_creates_new_window_from_attach() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-c-new-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "multi"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "multi", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Ctrl-B c
    attach.send_bytes(b"\x02c")?;
    // Give the daemon a beat to spawn the new window.
    std::thread::sleep(Duration::from_millis(500));

    let listed = harness.run(&["list-windows", "-t", "multi", "-F", "#{window_index}"])?;
    let stdout = String::from_utf8_lossy(&listed.stdout);
    let indexes: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        indexes,
        vec!["0", "1"],
        "Ctrl-B c must create a second window; list-windows: {stdout:?}",
    );

    attach.send_bytes(b"\x02d")?;
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    let _ = harness.run(&["kill-session", "-t", "multi"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

fn active_window_index(harness: &CliHarness, session: &str) -> Result<usize, Box<dyn Error>> {
    let listed = harness.run(&[
        "list-windows",
        "-t",
        session,
        "-F",
        "#{window_index}:#{window_active}",
    ])?;
    let stdout = String::from_utf8_lossy(&listed.stdout);
    for line in stdout.lines() {
        let mut parts = line.split(':');
        let idx = parts.next().ok_or("missing index")?.parse::<usize>()?;
        let active = parts.next().ok_or("missing active")? == "1";
        if active {
            return Ok(idx);
        }
    }
    Err(format!("no active window found in: {stdout:?}").into())
}
