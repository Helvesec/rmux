#![cfg(unix)]
//! Programs and scenarios that historically break terminal multiplexers:
//! alt-screen toggles, in-band screen clears, OSC title overrides, burst
//! output that overflows the replay budget, binary content, and concurrent
//! activity in inactive windows. Each test runs against a real `--passthrough`
//! session via the standard `AttachedSession` harness.

mod common;

use std::error::Error;
use std::time::Duration;

use common::{
    drain_attach_output, drain_attach_output_bytes, read_until_contains, terminate_child,
    AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const SHELL_PROMPT_MARKER: &str = "tester@RMUXHOST";

type TestResult = Result<(), Box<dyn Error>>;

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Alt-screen toggle from inside the inner shell (the vim/less/htop
/// pattern). Bytes `\x1b[?1049h ... \x1b[?1049l` must flow through
/// verbatim. After the program exits the host must be back in the
/// main screen buffer.
#[test]
fn passthrough_forwards_alt_screen_toggle_verbatim() -> TestResult {
    let harness = CliHarness::new("passthrough-alt-screen-toggle")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Simulate vim/less/htop: enter alt screen, print, exit alt screen.
    // Sentinel choice: we wait for SHELL_PROMPT_MARKER because the
    // prompt redraw only happens AFTER execution and never appears
    // in the typed command. Matching on a literal from the typed
    // input itself would match the shell's echo of the typed line.
    attach
        .send_bytes(b"printf '\\033[?1049hENTERED-ALT\\033[?1049l'\n")?;
    let consumed = run_with_prompt_redraw(&mut attach)?;
    assert!(
        contains_bytes(&consumed, b"\x1b[?1049h"),
        "alt-screen enter must flow through verbatim; got {:?}",
        String::from_utf8_lossy(&consumed)
    );
    assert!(
        contains_bytes(&consumed, b"\x1b[?1049l"),
        "alt-screen exit must flow through verbatim; got {:?}",
        String::from_utf8_lossy(&consumed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// In-band screen clear from the user's shell (printf '\033[2J\033[H').
/// Passthrough's whole point is byte-perfect forwarding, so the bytes
/// must reach the client unchanged.
#[test]
fn passthrough_forwards_user_initiated_clear_screen_verbatim() -> TestResult {
    let harness = CliHarness::new("passthrough-user-clear-screen")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"printf '\\033[2J\\033[H'\n")?;
    let consumed = run_with_prompt_redraw(&mut attach)?;
    assert!(
        contains_bytes(&consumed, b"\x1b[2J"),
        "user-issued CSI 2J must flow through verbatim; got {:?}",
        String::from_utf8_lossy(&consumed)
    );
    assert!(
        contains_bytes(&consumed, b"\x1b[H"),
        "user-issued cursor-home must flow through verbatim; got {:?}",
        String::from_utf8_lossy(&consumed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// In-band OSC 0 title set: a user program emits `\x1b]0;X\x07`. Our
/// rmux-tagged title from the initial attach is fine being overridden
/// by the inner program — that's the documented contract.
#[test]
fn passthrough_forwards_user_initiated_osc_title_verbatim() -> TestResult {
    let harness = CliHarness::new("passthrough-user-osc-title")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"printf '\\033]0;USER-CHOSEN-TITLE\\007'\n")?;
    let consumed = run_with_prompt_redraw(&mut attach)?;
    assert!(
        contains_bytes(&consumed, b"\x1b]0;USER-CHOSEN-TITLE\x07"),
        "user-emitted OSC 0 title must flow through verbatim; got {:?}",
        String::from_utf8_lossy(&consumed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Burst output that exceeds the default replay budget. The reader's
/// `over_budget()` check should fire, refreshing the snapshot from the
/// pane's current screen state. When we switch away and back, the
/// snapshot path is exercised and the user sees the latest viewport.
#[test]
fn passthrough_handles_burst_output_exceeding_replay_budget() -> TestResult {
    let harness = CliHarness::new("passthrough-burst-output")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "alpha",
            "-d",
            "sh",
            "-c",
            "printf WINDOW-1-PARK\\n; exec /bin/sh",
        ])?
        .status
        .success());

    // Lower the replay budget so we don't have to spew megabytes to
    // trip over_budget(). 4 KiB is well within reach of a single
    // shell `yes` burst.
    assert!(harness
        .run(&[
            "set-option",
            "-s",
            "passthrough-replay-bytes",
            "4096",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Spew enough output to overflow the 4 KiB budget several times.
    // Each `yes | head -c N` chunk produces N bytes of "y\n…".
    attach.send_bytes(b"yes BURST | head -c 20000\n")?;
    // Wait for the burst to finish (it ends with a fresh prompt).
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Print a final sentinel so we know the post-burst state.
    attach.send_bytes(b"printf POST-BURST-SENTINEL\\\\n\n")?;
    let _ = read_until_contains(attach.master_mut(), "POST-BURST-SENTINEL", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Switch to W1, then back to W0. The replay snapshot for W0 must
    // include POST-BURST-SENTINEL (the snapshot reflects the live grid).
    attach.send_bytes(b"\x021")?;
    let _ = read_until_contains(attach.master_mut(), "WINDOW-1-PARK", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;
    attach.send_bytes(b"\x020")?;
    let after = read_until_contains(attach.master_mut(), "POST-BURST-SENTINEL", IO_TIMEOUT)
        .map_err(|err| format!("post-burst snapshot did not preserve sentinel: {err}"))?;
    assert!(
        after.contains("POST-BURST-SENTINEL"),
        "snapshot after burst must contain post-burst sentinel; got {after:?}"
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Binary content in the output stream. A program that emits non-UTF-8
/// bytes (urandom, /bin/sh binary contents, etc.) must not crash the
/// forwarder. The host terminal will render gibberish but rmux's job is
/// just to forward.
#[test]
fn passthrough_does_not_crash_on_binary_inner_output() -> TestResult {
    let harness = CliHarness::new("passthrough-binary-output")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // 200 bytes of urandom + a sentinel so we can synchronise.
    attach.send_bytes(b"head -c 200 /dev/urandom; printf '\\nBINARY-DONE\\n'\n")?;
    let bytes = read_until_contains(attach.master_mut(), "BINARY-DONE", IO_TIMEOUT)?;
    assert!(
        bytes.contains("BINARY-DONE"),
        "binary content burst must not stall the forwarder; got {bytes:?}"
    );

    // Attach must still be alive (would have exited if the forwarder
    // panicked).
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "forwarder must survive binary content burst"
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Concurrent inactive-window output: while we're attached to window 0,
/// window 1 produces output that goes into its replay log. When we
/// switch to window 1, that output appears via the replay snapshot.
#[test]
fn passthrough_inactive_window_output_replays_on_switch() -> TestResult {
    let harness = CliHarness::new("passthrough-inactive-output-replay")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    // Window 1 prints a marker every 50ms for 1 second, then dies.
    // While we're attached to window 0, this output accumulates in
    // window 1's replay log; switching shows it via snapshot.
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "alpha",
            "-d",
            "sh",
            "-c",
            "for i in 1 2 3 4 5; do printf 'BACKGROUND-LINE-%s\\n' \"$i\"; sleep 0.1; done; exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    // Let the background output accumulate while we stay on W0.
    std::thread::sleep(Duration::from_millis(700));
    drain_attach_output(attach.master_mut())?;

    // Switch to W1. Replay must surface the accumulated output.
    attach.send_bytes(b"\x021")?;
    let after = read_until_contains(attach.master_mut(), "BACKGROUND-LINE-5", IO_TIMEOUT)?;
    for n in 1..=5 {
        let marker = format!("BACKGROUND-LINE-{n}");
        assert!(
            after.contains(&marker),
            "inactive-window snapshot must include {marker}; got {after:?}"
        );
    }

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// `less` paging a file: alt-screen, key-driven navigation, clean exit.
/// Skips when `less` isn't on PATH.
#[test]
fn passthrough_can_run_less_paging_through_a_file() -> TestResult {
    if which("less").is_none() {
        eprintln!("`less` not on PATH; skipping");
        return Ok(());
    }
    let harness = CliHarness::new("passthrough-less-pager")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Pipe a small numbered file through less. less uses alt-screen
    // unless `-X` is passed, so we expect to see `\x1b[?1049h` in
    // the byte stream.
    attach.send_bytes(b"seq 1 80 | less\n")?;
    // Wait long enough for less to render its first frame.
    std::thread::sleep(Duration::from_millis(400));
    let less_open_bytes = drain_attach_output_bytes(attach.master_mut())?;
    assert!(
        contains_bytes(&less_open_bytes, b"\x1b[?1049h"),
        "less must emit alt-screen-enter when rendering; bytes={:?}",
        String::from_utf8_lossy(&less_open_bytes)
    );

    // Quit less and wait for the shell prompt to return.
    attach.send_bytes(b"q")?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// A program that emits output without a trailing newline triggers
/// zsh/bash's missing-newline indicator (the inverse-video `%` or `$`).
/// Verify the inner shell's marker reaches the client and the next
/// prompt redraws cleanly.
#[test]
fn passthrough_handles_output_without_trailing_newline() -> TestResult {
    let harness = CliHarness::new("passthrough-no-trailing-newline")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"printf 'NO-NEWLINE-HERE'\n")?;
    let bytes = read_until_contains(attach.master_mut(), "NO-NEWLINE-HERE", IO_TIMEOUT)?;
    assert!(
        bytes.contains("NO-NEWLINE-HERE"),
        "no-newline output must still reach the client; got {bytes:?}"
    );

    // The next prompt should still render — the shell's % indicator
    // should not throw off rmux's forwarder.
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Five windows created in succession, then a round-trip through all of
/// them via Ctrl-B 0..4. Each window has a distinct marker; replay must
/// surface the right one for each landing.
#[test]
fn passthrough_five_windows_round_trip_via_prefix_digits() -> TestResult {
    let harness = CliHarness::new("passthrough-five-window-round-trip")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    // Create windows 1..4 (window 0 already exists). Each prints a
    // marker on startup.
    for n in 1..=4 {
        let marker = format!("WINDOW-{n}-MARKER");
        let cmd = format!("printf {marker}\\n; exec /bin/sh");
        assert!(harness
            .run(&["new-window", "-t", "alpha", "-d", "sh", "-c", cmd.as_str()])?
            .status
            .success());
    }

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    for n in 1..=4 {
        let digit = char::from(b'0' + n as u8);
        attach.send_bytes(&[0x02, digit as u8])?;
        let marker = format!("WINDOW-{n}-MARKER");
        let after = read_until_contains(attach.master_mut(), &marker, IO_TIMEOUT)?;
        assert!(
            after.contains(&marker),
            "Ctrl-B {n} must surface {marker} via replay; got {after:?}"
        );
    }

    // Return to window 0.
    attach.send_bytes(b"\x020")?;
    std::thread::sleep(Duration::from_millis(300));
    let listed = harness.run(&[
        "list-windows",
        "-t",
        "alpha",
        "-F",
        "#{window_index}:#{window_active}",
    ])?;
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        stdout.contains("0:1"),
        "Ctrl-B 0 must return to window 0 after the round trip; list: {stdout:?}"
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Resize while a program is running. The inner pane must see the new
/// dimensions; the forwarder must not corrupt output across the resize.
#[test]
fn passthrough_resize_during_running_program_does_not_corrupt_output() -> TestResult {
    let harness = CliHarness::new("passthrough-resize-during-program")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Start an output-on-demand loop. After resize we send another
    // sentinel and verify it reaches us cleanly.
    attach
        .send_bytes(b"printf 'BEFORE-RESIZE\\n'\n")?;
    let _ = read_until_contains(attach.master_mut(), "BEFORE-RESIZE", IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.resize(TerminalSize::new(132, 50))?;
    std::thread::sleep(Duration::from_millis(200));

    attach.send_bytes(b"printf 'AFTER-RESIZE\\n'\n")?;
    let after = read_until_contains(attach.master_mut(), "AFTER-RESIZE", IO_TIMEOUT)?;
    assert!(
        after.contains("AFTER-RESIZE"),
        "post-resize output must still reach the client; got {after:?}"
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// ANSI SGR sequences (colours, bold, etc.) must flow through verbatim
/// for tools like `ls --color=always`, `git diff`, syntax-highlighted
/// REPLs, etc.
#[test]
fn passthrough_forwards_ansi_sgr_colour_sequences_verbatim() -> TestResult {
    let harness = CliHarness::new("passthrough-sgr-colours")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Red text, then reset.
    attach.send_bytes(b"printf '\\033[31mRED\\033[0m'\n")?;
    let consumed = run_with_prompt_redraw(&mut attach)?;
    assert!(
        contains_bytes(&consumed, b"\x1b[31m"),
        "SGR set-red must flow through; got {:?}",
        String::from_utf8_lossy(&consumed)
    );
    assert!(
        contains_bytes(&consumed, b"\x1b[0m"),
        "SGR reset must flow through; got {:?}",
        String::from_utf8_lossy(&consumed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Bracketed paste mode (`\x1b[?2004h`) is a stdio convention where
/// the terminal wraps pasted text in `\x1b[200~ ... \x1b[201~` so the
/// inner program knows it's paste vs typed input. Modern shells
/// (zsh/bash with bracketed-paste enabled) and editors (vim, neovim)
/// depend on it. Passthrough must let the inner program enable and
/// receive bracketed-paste bytes verbatim.
#[test]
fn passthrough_forwards_bracketed_paste_enable_sequence_verbatim() -> TestResult {
    let harness = CliHarness::new("passthrough-bracketed-paste")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // A user program enables bracketed paste.
    attach.send_bytes(b"printf '\\033[?2004h'\n")?;
    let consumed = run_with_prompt_redraw(&mut attach)?;
    assert!(
        contains_bytes(&consumed, b"\x1b[?2004h"),
        "bracketed-paste enable must flow through verbatim; got {:?}",
        String::from_utf8_lossy(&consumed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Detach with `Ctrl-B d` then re-attach to the same passthrough
/// session must restore the user to the same window with content
/// preserved via replay.
#[test]
fn passthrough_detach_then_reattach_restores_state_via_replay() -> TestResult {
    let harness = CliHarness::new("passthrough-detach-reattach")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;

    attach.send_bytes(b"printf 'PRE-DETACH-SENTINEL\\n'\n")?;
    let _ = read_until_contains(attach.master_mut(), "PRE-DETACH-SENTINEL", IO_TIMEOUT)?;

    // Detach.
    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    attach.assert_restored()?;
    drop(attach);

    // Re-attach.
    let mut attach2 = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach2.wait_for_raw_mode(IO_TIMEOUT)?;
    // The replay log should re-emit the sentinel that the inner shell
    // printed in the previous attach session.
    let after = read_until_contains(attach2.master_mut(), "PRE-DETACH-SENTINEL", IO_TIMEOUT)
        .map_err(|err| format!("re-attach did not replay pre-detach content: {err}"))?;
    assert!(
        after.contains("PRE-DETACH-SENTINEL"),
        "re-attach replay must include pre-detach output; got {after:?}"
    );

    attach2.send_bytes(b"\x02d")?;
    let _ = attach2.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Killing the active window via `rmux kill-window` while attached:
/// the daemon should auto-select the next remaining window and the
/// forwarder should switch to it cleanly (rather than tearing down
/// the attach when the only-killed window happens to be the active
/// one).
#[test]
fn passthrough_kill_active_window_falls_through_to_next_window() -> TestResult {
    let harness = CliHarness::new("passthrough-kill-active-window")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "alpha",
            "-d",
            "sh",
            "-c",
            "printf NEXT-WINDOW-MARKER\\n; exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Kill the active window (window 0). The daemon should keep the
    // session alive on window 1.
    assert!(harness
        .run(&["kill-window", "-t", "alpha:0"])?
        .status
        .success());

    // Wait for window-1's content to appear in our attach.
    let after = read_until_contains(attach.master_mut(), "NEXT-WINDOW-MARKER", IO_TIMEOUT)
        .map_err(|err| format!("kill-active-window did not surface next window: {err}"))?;
    assert!(
        after.contains("NEXT-WINDOW-MARKER"),
        "after kill-window of the active window, the next window's content must \
         appear in the attach via switch_passthrough_target replay; got {after:?}",
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// A long-running inner command (sleep) must not cause the attach to
/// time out, detach, or otherwise misbehave. Verifies no idle-based
/// shutdown trigger fires while a session has an active pane.
#[test]
fn passthrough_long_running_command_keeps_attach_alive() -> TestResult {
    let harness = CliHarness::new("passthrough-long-running")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // 6-second sleep, then a sentinel. The retained-exited-outputs
    // TTL we previously fixed is ~5s; this verifies a live attach is
    // protected from that timer.
    attach.send_bytes(b"sleep 6 && printf 'SLEEP-DONE\\n'\n")?;
    // Verify the attach is still alive halfway through.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "attach must survive a 6s sleep without spontaneous detach"
    );
    let _ = read_until_contains(attach.master_mut(), "SLEEP-DONE", Duration::from_secs(8))?;

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Ctrl-B w (choose-tree window picker) in passthrough must:
///   1. enter alt-screen (`\x1b[?1049h`) so the picker doesn't draw
///      on top of the inner shell's content,
///   2. render the choose-tree overlay (visible session/window list),
///   3. on dismissal (Enter or Escape), exit alt-screen
///      (`\x1b[?1049l`) so the host terminal is back where it was.
///
/// Before the alt-screen-bracketed-overlay fix this test fails:
/// pressing Ctrl-B w silently does nothing — the daemon sends an
/// `AttachControl::Overlay` to the forwarder which the passthrough
/// loop drops on the floor.
#[test]
fn passthrough_prefix_w_opens_choose_tree_overlay_in_alt_screen() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-w-choose-tree")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    // Two windows so the picker has something to list.
    assert!(harness
        .run(&[
            "new-window",
            "-t",
            "alpha",
            "-d",
            "sh",
            "-c",
            "exec /bin/sh",
        ])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;

    // Wait long enough for the daemon to render and send the overlay.
    std::thread::sleep(Duration::from_millis(500));
    let opened = drain_attach_output_bytes(attach.master_mut())?;

    assert!(
        contains_bytes(&opened, b"\x1b[?1049h"),
        "Ctrl-B w in passthrough must enter alt-screen for the picker; got {:?}",
        String::from_utf8_lossy(&opened)
    );
    // The choose-tree overlay should contain the session name 'alpha'
    // as one of its rows.
    assert!(
        contains_bytes(&opened, b"alpha"),
        "Ctrl-B w must render the choose-tree overlay listing 'alpha'; got {:?}",
        String::from_utf8_lossy(&opened)
    );

    // Dismiss the overlay with 'q' (the input handler accepts both
    // Escape and 'q' for cancel; 'q' avoids escape-sequence parsing
    // ambiguity that can hold a bare \x1b waiting for more bytes).
    attach.send_bytes(b"q")?;
    std::thread::sleep(Duration::from_millis(400));
    let closed = drain_attach_output_bytes(attach.master_mut())?;
    assert!(
        contains_bytes(&closed, b"\x1b[?1049l"),
        "dismissing the choose-tree overlay must exit alt-screen; got {:?}",
        String::from_utf8_lossy(&closed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Open the picker, hit Enter on whatever's highlighted by default
/// (typically the current window). The picker must dismiss cleanly:
/// alt-screen exits, no crash, the session keeps running.
///
/// We deliberately don't navigate first — choose-tree's row ordering
/// (session/window/pane nesting + collapsed-by-default) is fragile
/// to drive from a test. Picking the default item just exercises
/// the Enter → dismiss → switch round-trip.
#[test]
fn passthrough_prefix_w_enter_dismisses_picker_and_session_survives() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-w-enter-default-pick")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    assert!(harness
        .run(&["new-window", "-t", "alpha", "-d", "sh", "-c", "exec /bin/sh"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    // The picker can re-render several times before reaching idle
    // (initial frame, refresh after attached_count update, etc.).
    // Wait until alt-screen-enter has actually landed.
    let opened = read_until_contains(
        attach.master_mut(),
        "alpha: 2 windows",
        IO_TIMEOUT,
    )?;
    assert!(
        contains_bytes(opened.as_bytes(), b"\x1b[?1049h"),
        "picker open must enter alt-screen; got {opened:?}"
    );

    attach.send_bytes(b"\r")?;
    // Poll up to IO_TIMEOUT for the alt-screen-exit signal. Enter
    // routes through accept_mode_tree_selection → dismiss_mode_tree
    // → AdvancePersistentOverlayState, which can take multiple
    // tokio tasks to deliver.
    let mut accumulated: Vec<u8> = Vec::new();
    let deadline = std::time::Instant::now() + IO_TIMEOUT;
    while std::time::Instant::now() < deadline {
        let chunk = drain_attach_output_bytes(attach.master_mut())?;
        accumulated.extend_from_slice(&chunk);
        if contains_bytes(&accumulated, b"\x1b[?1049l") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        contains_bytes(&accumulated, b"\x1b[?1049l"),
        "Enter on the picker must release alt-screen within {IO_TIMEOUT:?}; got {:?}",
        String::from_utf8_lossy(&accumulated)
    );

    // Session must survive — attach client still alive.
    assert!(
        attach.child_mut().try_wait()?.is_none(),
        "picker Enter must not tear down the attach"
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Open + close the picker twice. We must see exactly two
/// alt-screen-enter and two alt-screen-exit transitions — no leaks,
/// no doubled emissions per cycle.
#[test]
fn passthrough_prefix_w_open_close_cycles_balance_alt_screen() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-w-balance")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Drain after every event so we can attribute transitions to
    // the cycle that produced them (and so a slow daemon doesn't
    // backpressure into our timing).
    let mut totals = Vec::new();
    for cycle in 1..=2 {
        attach.send_bytes(b"\x02w")?;
        std::thread::sleep(Duration::from_millis(600));
        let opened = drain_attach_output_bytes(attach.master_mut())?;
        attach.send_bytes(b"q")?;
        std::thread::sleep(Duration::from_millis(600));
        let closed = drain_attach_output_bytes(attach.master_mut())?;
        let open_enters = opened
            .windows(b"\x1b[?1049h".len())
            .filter(|w| *w == b"\x1b[?1049h")
            .count();
        let close_exits = closed
            .windows(b"\x1b[?1049l".len())
            .filter(|w| *w == b"\x1b[?1049l")
            .count();
        totals.push((cycle, open_enters, close_exits, opened.clone(), closed.clone()));
    }
    for (cycle, enters, exits, opened, closed) in &totals {
        assert_eq!(
            *enters, 1,
            "cycle {cycle}: Ctrl-B w must emit exactly 1 alt-screen-enter; saw {enters}. \
             opened-bytes={:?}",
            String::from_utf8_lossy(opened)
        );
        assert_eq!(
            *exits, 1,
            "cycle {cycle}: q must emit exactly 1 alt-screen-exit; saw {exits}. \
             closed-bytes={:?}",
            String::from_utf8_lossy(closed)
        );
    }

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Navigating inside the picker (↓ ↑) triggers re-renders. Each
/// re-render emits a new overlay frame — but we must NOT emit a
/// fresh `\x1b[?1049h` for each one (that would re-enter alt-screen
/// every keystroke). Only the first frame opens the bracket.
#[test]
fn passthrough_prefix_w_navigation_does_not_repeat_alt_screen_enter() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-w-no-repeat-enter")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());
    assert!(harness
        .run(&["new-window", "-t", "alpha", "-d", "sh", "-c", "exec /bin/sh"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    attach.send_bytes(b"\x02w")?;
    std::thread::sleep(Duration::from_millis(350));
    // Expand and navigate several times.
    for keys in [
        b"\x1b[C".as_slice(), // expand
        b"\x1b[B",            // down
        b"\x1b[B",            // down
        b"\x1b[A",            // up
        b"\x1b[A",            // up
    ] {
        attach.send_bytes(keys)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    attach.send_bytes(b"q")?;
    std::thread::sleep(Duration::from_millis(300));

    let bytes = drain_attach_output_bytes(attach.master_mut())?;
    let enters = bytes
        .windows(b"\x1b[?1049h".len())
        .filter(|w| *w == b"\x1b[?1049h")
        .count();
    assert_eq!(
        enters, 1,
        "alt-screen enter must fire exactly ONCE per overlay session, not per \
         re-render. saw {enters} enters in: {:?}",
        String::from_utf8_lossy(&bytes)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// `Ctrl-B t` (clock-mode) wraps in alt-screen on entry and
/// releases it on dismissal.
///
/// The clock-mode lifecycle is: entry sends a non-persistent
/// overlay frame, then per-second ticks send PERSISTENT frames to
/// refresh the clock display, then dismissal sends another
/// non-persistent frame (the "restore" content). Entry and exit
/// look byte-identical to the forwarder *except* for whether we're
/// already in alt-screen when they arrive — that's the signal the
/// overlay handler keys off.
#[test]
fn passthrough_prefix_t_clock_mode_uses_alt_screen() -> TestResult {
    let harness = CliHarness::new("passthrough-prefix-t-clock-mode")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Ctrl-B t opens clock-mode.
    attach.send_bytes(b"\x02t")?;
    std::thread::sleep(Duration::from_millis(500));
    let opened = drain_attach_output_bytes(attach.master_mut())?;
    assert!(
        contains_bytes(&opened, b"\x1b[?1049h"),
        "Ctrl-B t (clock-mode) must wrap in alt-screen; got {:?}",
        String::from_utf8_lossy(&opened)
    );

    // Clock-mode is dismissed by any key.
    attach.send_bytes(b"q")?;
    std::thread::sleep(Duration::from_millis(400));
    let closed = drain_attach_output_bytes(attach.master_mut())?;
    assert!(
        contains_bytes(&closed, b"\x1b[?1049l"),
        "dismissing clock-mode must exit alt-screen; got {:?}",
        String::from_utf8_lossy(&closed)
    );

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// While the overlay is up, inner-pane output (e.g. a background
/// process printing) must NOT paint on top of the picker — those
/// bytes get dropped by the forwarder. They still land in the
/// passthrough replay log, so after dismissal a window-switch
/// round-trip would recover them.
///
/// Verification: trigger background output in window 1, open the
/// picker while it's spewing. The bytes that arrive while the
/// picker is up must NOT contain the background marker (because
/// the forwarder suppresses pane output during the overlay).
#[test]
fn passthrough_overlay_suppresses_inner_pane_output_while_visible() -> TestResult {
    let harness = CliHarness::new("passthrough-overlay-suppresses-output")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert!(harness
        .run(&["new-session", "--passthrough", "-d", "-s", "alpha"])?
        .status
        .success());

    let mut attach = AttachedSession::spawn(&harness, "alpha", TerminalSize::new(120, 40))?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    drain_attach_output(attach.master_mut())?;

    // Background spewer that *starts after a delay* (sleep 1 first)
    // so we have time to open the picker before the first BG-SPEW
    // ever lands in the attach stream. That isolates "leak through
    // the overlay" from "raced ahead of the overlay".
    attach
        .send_bytes(b"(sleep 1; for i in 1 2 3 4 5 6; do printf 'BG-SPEW-%s\\n' \"$i\"; sleep 0.2; done) &\n")?;
    std::thread::sleep(Duration::from_millis(200));
    drain_attach_output(attach.master_mut())?;

    // Open the picker before the spewer wakes up. Then wait long
    // enough that the spewer fires several iterations while we're
    // in alt-screen.
    attach.send_bytes(b"\x02w")?;
    std::thread::sleep(Duration::from_millis(2000));
    let while_visible = drain_attach_output_bytes(attach.master_mut())?;

    // While the picker is visible, the background marker must NOT
    // appear (the forwarder dropped those bytes to avoid painting
    // under the overlay).
    let bg_count = (1..=6)
        .filter(|i| {
            let needle = format!("BG-SPEW-{i}").into_bytes();
            contains_bytes(&while_visible, &needle)
        })
        .count();
    assert_eq!(
        bg_count, 0,
        "inner-pane output must be suppressed while the overlay owns alt-screen; \
         saw {bg_count} background markers in: {:?}",
        String::from_utf8_lossy(&while_visible)
    );

    attach.send_bytes(b"q")?;
    std::thread::sleep(Duration::from_millis(400));
    // Wait for the background process to die so it doesn't keep
    // spewing after detach.
    std::thread::sleep(Duration::from_millis(1500));

    attach.send_bytes(b"\x02d")?;
    let _ = attach.wait_for_exit(IO_TIMEOUT)?;
    let _ = harness.run(&["kill-session", "-t", "alpha"]);
    terminate_child(daemon.child_mut())?;
    Ok(())
}

/// Reads from the attach until the shell's next prompt redraw, then
/// returns the consumed bytes. Used by tests that send a command and
/// want to capture the executed output without false-matching on the
/// shell's typed-input echo (which contains the literal command text).
/// The prompt redraw only happens AFTER the command runs and never
/// appears inside the typed command line.
fn run_with_prompt_redraw(attach: &mut AttachedSession) -> Result<Vec<u8>, Box<dyn Error>> {
    let s = read_until_contains(attach.master_mut(), SHELL_PROMPT_MARKER, IO_TIMEOUT)?;
    Ok(s.into_bytes())
}

fn which(cmd: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(cmd);
            if candidate.is_file() {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

