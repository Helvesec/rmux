#![cfg(unix)]

mod common;

use std::error::Error;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use common::{
    assert_success, drain_attach_output_bytes, read_until_contains, stdout, terminate_child,
    wait_for_socket, AttachedSession, CliHarness,
};
use rmux_pty::TerminalSize;
use rustix::event::{poll, PollFd, PollFlags, Timespec};

const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// The issue #216 reporter's ghostty geometry.
const REPORTER_SIZE: TerminalSize = TerminalSize::new(225, 54);
const DRAIN_SLICE: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 20_000_000,
};
/// The attached client must still be present this long after the burst.
const ATTACHED_HOLD: Duration = Duration::from_secs(2);
/// Paints all 54 popup rows 40 times in a single ~500 KiB write, then keeps
/// the popup open long enough for the attached client to be observed.
const FULL_SCREEN_BURST_PROGRAM: &str = concat!(
    "python3 -c 'import sys,time;",
    "s=\"\".join(\"\\x1b[%d;1H\\x1b[31m\"%r+\"x\"*225 for r in range(1,55));",
    "sys.stdout.write(s*40);sys.stdout.flush();time.sleep(3)'"
);

/// A painted run no shell prompt or status line produces.
const PAINTED_RUN: &str = "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";

/// Issue #216: a full-screen popup painting ~500 KiB at once must coalesce
/// into bounded overlay frames instead of overflowing the attach-control
/// backlog, whose prune detaches the client with `attach-control-detach`.
#[test]
fn popup_full_screen_500k_burst_keeps_client_attached() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("popup-burst-attach")?;
    let exit_log = harness.tmpdir().join("attach-exit.log");
    let mut daemon = harness
        .hidden_daemon_command()
        .env("RMUX_ATTACH_EXIT_LOG", &exit_log)
        .spawn()?;
    wait_for_socket(harness.socket_path(), &mut daemon)?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        REPORTER_SIZE,
        &[("TERM", "xterm-256color")],
    )?;
    attach.wait_for_raw_mode(IO_TIMEOUT)?;
    let _ = read_until_contains(attach.master_mut(), "[alpha]", IO_TIMEOUT)?;
    let client_tty = wait_for_single_client_tty(&harness)?;

    let mut popup = harness
        .base_command()
        .args([
            "display-popup",
            "-c",
            &client_tty,
            "-B",
            "-E",
            "-w",
            "100%",
            "-h",
            "100%",
            FULL_SCREEN_BURST_PROGRAM,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    let mut painted = Vec::new();
    let paint_deadline = Instant::now() + IO_TIMEOUT;
    while !contains_bytes(&painted, PAINTED_RUN.as_bytes()) {
        if Instant::now() >= paint_deadline {
            return Err("the popup burst never reached the attached terminal".into());
        }
        drain_while_attached(&mut attach, &exit_log, &mut painted)?;
    }
    let hold_deadline = Instant::now() + ATTACHED_HOLD;
    while Instant::now() < hold_deadline {
        drain_while_attached(&mut attach, &exit_log, &mut painted)?;
    }

    let clients = stdout(&harness.run(&["list-clients", "-F", "#{client_tty}"])?);
    assert_eq!(
        clients.trim(),
        client_tty,
        "the attached client must survive the popup burst"
    );
    let exit_log_text = std::fs::read_to_string(&exit_log).unwrap_or_default();
    assert!(
        !exit_log_text.contains("attach-control-detach"),
        "the popup burst pruned the attach-control backlog: {exit_log_text:?}"
    );

    // Keep reading like a live terminal until the popup command returns.
    let popup_deadline = Instant::now() + IO_TIMEOUT;
    let popup_status = loop {
        drain_while_attached(&mut attach, &exit_log, &mut painted)?;
        if let Some(status) = popup.try_wait()? {
            break status;
        }
        if Instant::now() >= popup_deadline {
            terminate_child(&mut popup)?;
            return Err("display-popup did not return after its burst".into());
        }
    };
    assert!(
        popup_status.success(),
        "display-popup must succeed: {popup_status}; exit log: {:?}",
        std::fs::read_to_string(&exit_log).unwrap_or_default()
    );

    assert_success(&harness.run(&["kill-session", "-t", "alpha"])?);
    let status = attach.wait_for_exit(IO_TIMEOUT)?;
    assert_eq!(status.code(), Some(0));
    attach.assert_restored()?;

    terminate_child(&mut daemon)?;
    Ok(())
}

/// Reads attached output as a live terminal does, waking on readiness rather
/// than sleeping, and fails if the client has exited.
fn drain_while_attached(
    attach: &mut AttachedSession,
    exit_log: &Path,
    painted: &mut Vec<u8>,
) -> Result<(), Box<dyn Error>> {
    let ready = {
        let mut fds = [PollFd::new(
            &*attach.master_mut(),
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        poll(&mut fds, Some(&DRAIN_SLICE))?
    };
    if ready > 0 {
        painted.extend(drain_attach_output_bytes(attach.master_mut())?);
    }
    if let Some(status) = attach.child_mut().try_wait()? {
        return Err(format!(
            "attach client exited during the popup burst ({status}); exit log: {:?}",
            std::fs::read_to_string(exit_log).unwrap_or_default()
        )
        .into());
    }
    Ok(())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn wait_for_single_client_tty(harness: &CliHarness) -> Result<String, Box<dyn Error>> {
    let deadline = Instant::now() + IO_TIMEOUT;
    loop {
        let clients = stdout(&harness.run(&["list-clients", "-F", "#{client_tty}"])?);
        let clients = clients.trim();
        if !clients.is_empty() && !clients.contains('\n') {
            return Ok(clients.to_owned());
        }
        if Instant::now() >= deadline {
            return Err(format!("expected one attached client, got {clients:?}").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}
