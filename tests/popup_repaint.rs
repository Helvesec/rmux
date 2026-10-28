#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::time::{Duration, Instant};

use common::{
    assert_success, drain_attach_output_bytes, read_until_contains, AttachedSession, CliHarness,
};
use rmux_core::{input::InputParser, Screen};
use rmux_pty::TerminalSize;

const TIMEOUT: Duration = Duration::from_secs(10);

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn collect(attach: &mut AttachedSession, duration: Duration) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = Instant::now() + duration;
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
        assert!(
            attach.child_mut().try_wait()?.is_none(),
            "client detached during popup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    bytes.extend(drain_attach_output_bytes(attach.master_mut())?);
    Ok(bytes)
}

fn screen_text(frames: &[u8]) -> Result<String, Box<dyn Error>> {
    let mut screen = Screen::new(rmux_proto::TerminalSize { cols: 80, rows: 24 }, 0);
    InputParser::new().parse(frames, &mut screen);
    Ok(String::from_utf8(screen.capture_transcript(
        Default::default(),
        Default::default(),
    ))?)
}

fn open_popup(
    harness: &CliHarness,
    attach: &mut AttachedSession,
    command: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    assert_success(&harness.run(&[
        "bind-key",
        "s",
        "display-popup",
        "-B",
        "-E",
        "-x",
        "0",
        "-y",
        "0",
        "-w",
        "100%",
        "-h",
        "100%",
        command,
    ])?);
    attach.send_bytes(b"\x02s")?;
    let mut bytes =
        read_until_contains(attach.master_mut(), "POPUP_VISIBLE", TIMEOUT)?.into_bytes();
    bytes.extend(collect(attach, Duration::from_millis(200))?);
    Ok(bytes)
}

#[test]
fn popup_hides_active_pane_output_and_restores_the_final_screen() -> Result<(), Box<dyn Error>> {
    check_background_output(false)
}

#[test]
fn popup_hides_inactive_pane_output_and_restores_the_final_screen() -> Result<(), Box<dyn Error>> {
    check_background_output(true)
}

fn check_background_output(split: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("popup-background-pty")?;
    let _daemon = harness.start_hidden_daemon()?;
    let start = harness.tmpdir().join("start");
    let done = harness.tmpdir().join("done");
    let producer = harness.tmpdir().join("producer.sh");
    fs::write(&producer, format!(
        "while [ ! -e {} ]; do sleep 0.02; done\n\
         i=0\nwhile [ $i -lt 20 ]; do printf 'BACKGROUND_UPDATE\\r\\n'; i=$((i+1)); sleep 0.02; done\n\
         printf 'BACKGROUND_FINAL\\r\\n'\n: > {}\nexec sleep 60\n",
        quote(start.to_str().ok_or("non-UTF8 start path")?),
        quote(done.to_str().ok_or("non-UTF8 done path")?),
    ))?;
    let command = format!(
        "exec /bin/sh {}",
        quote(producer.to_str().ok_or("non-UTF8 producer path")?)
    );
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "alpha",
        if split { "exec sleep 60" } else { &command },
    ])?);
    if split {
        assert_success(&harness.run(&["split-window", "-d", "-t", "alpha", &command])?);
    }
    assert_success(&harness.run(&["set-option", "-g", "status", "off"])?);
    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        TerminalSize::new(80, 24),
        &[("TERM", "xterm-256color")],
    )?;
    attach.wait_for_raw_mode(TIMEOUT)?;
    let mut frames = open_popup(&harness, &mut attach, "printf POPUP_VISIBLE; exec sleep 60")?;

    fs::write(&start, "")?;
    let deadline = Instant::now() + TIMEOUT;
    let mut hidden = Vec::new();
    let target = if split { "alpha:0.1" } else { "alpha:0.0" };
    loop {
        hidden.extend(collect(&mut attach, Duration::from_millis(50))?);
        if done.exists() {
            let captured = harness.run(&["capture-pane", "-p", "-t", target])?;
            assert!(captured.status.success(), "{}", common::stderr(&captured));
            if common::stdout(&captured).contains("BACKGROUND_FINAL") {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "producer did not reach its final output"
        );
    }
    hidden.extend(collect(&mut attach, Duration::from_millis(200))?);
    let hidden_text = String::from_utf8_lossy(&hidden);
    assert!(
        !hidden_text.contains("BACKGROUND_"),
        "background output leaked, split={split}: {hidden_text:?}"
    );
    assert!(
        !hidden.windows(4).any(|bytes| bytes == b"\x1b[2J"),
        "background refresh cleared the popup, split={split}"
    );

    assert_success(&harness.run(&["display-popup", "-C"])?);
    // The producer is now sleeping: no future pane output can repair a stale restore.
    frames.extend(
        read_until_contains(attach.master_mut(), "BACKGROUND_FINAL", TIMEOUT)?.into_bytes(),
    );
    frames.extend(collect(&mut attach, Duration::from_millis(200))?);
    let visible = screen_text(&frames)?;
    assert!(
        visible.contains("BACKGROUND_FINAL"),
        "stale restore, split={split}: {visible:?}"
    );
    assert!(
        !visible.contains("POPUP_VISIBLE"),
        "popup remains after close, split={split}: {visible:?}"
    );
    Ok(())
}

#[test]
fn switching_from_inside_a_popup_paints_an_idle_destination() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("popup-switch-pty")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "exec sleep 60"])?);
    assert_success(&harness.run(&[
        "new-session",
        "-d",
        "-s",
        "beta",
        "printf DESTINATION_READY; exec sleep 60",
    ])?);
    assert_success(&harness.run(&["set-option", "-g", "status", "off"])?);
    // Make sure the destination has produced its only output before the switch.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let captured = harness.run(&["capture-pane", "-p", "-t", "beta"])?;
        assert!(captured.status.success(), "{}", common::stderr(&captured));
        if common::stdout(&captured).contains("DESTINATION_READY") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "destination did not become ready"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut attach = AttachedSession::spawn_with_env(
        &harness,
        "alpha",
        TerminalSize::new(80, 24),
        &[("TERM", "xterm-256color")],
    )?;
    attach.wait_for_raw_mode(TIMEOUT)?;
    let clients = harness.run(&["list-clients", "-F", "#{client_name}"])?;
    assert!(clients.status.success(), "{}", common::stderr(&clients));
    let client_name = common::stdout(&clients);
    let trigger = harness.tmpdir().join("switch");
    let command = format!(
        "printf POPUP_VISIBLE; while [ ! -e {} ]; do sleep 0.02; done; exec {} -S {} switch-client -c {} -t beta",
        quote(trigger.to_str().ok_or("non-UTF8 trigger path")?),
        quote(env!("CARGO_BIN_EXE_rmux")),
        quote(harness.socket_path().to_str().ok_or("non-UTF8 socket path")?),
        quote(client_name.trim()),
    );
    let mut frames = open_popup(&harness, &mut attach, &command)?;
    fs::write(trigger, "")?;
    frames.extend(
        read_until_contains(attach.master_mut(), "DESTINATION_READY", TIMEOUT)?.into_bytes(),
    );
    frames.extend(collect(&mut attach, Duration::from_millis(200))?);
    let visible = screen_text(&frames)?;
    assert!(
        visible.contains("DESTINATION_READY"),
        "destination was not drawn: {visible:?}"
    );
    assert!(
        !visible.contains("POPUP_VISIBLE"),
        "retired popup remains on screen: {visible:?}"
    );
    let clients = harness.run(&["list-clients", "-F", "#{client_session}"])?;
    assert!(clients.status.success(), "{}", common::stderr(&clients));
    assert_eq!(common::stdout(&clients).trim(), "beta");
    Ok(())
}
