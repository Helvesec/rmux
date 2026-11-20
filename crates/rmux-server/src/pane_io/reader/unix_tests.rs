use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
use rmux_proto::{SessionName, TerminalSize};
use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

use super::{spawn_pane_output_reader, PaneOutputReaderTask};
use crate::pane_io::pane_output_channel;
use crate::pane_reader_runtime::PaneReaderRuntime;
use crate::pane_transcript::PaneTranscript;

#[test]
fn output_reader_uses_64k_read_buffer_for_dense_pty_bursts() {
    assert_eq!(super::READ_BUFFER_SIZE, 64 * 1024);
}

#[test]
fn output_reader_batches_from_first_available_byte() {
    assert_eq!(super::PANE_READ_BATCH_TRIGGER_BYTES, 1);
}

#[test]
fn small_read_yield_detector_ignores_short_bursts() {
    let mut coalescer = super::SustainedReadCoalescer::default();
    let start = tokio::time::Instant::now();

    for index in 0..super::PANE_SUSTAINED_READ_MIN_BATCHES {
        assert!(!coalescer.should_yield_at(128, start + Duration::from_millis(u64::from(index))));
    }
}

#[test]
fn small_read_yield_detector_yields_after_sustained_small_output() {
    let mut coalescer = super::SustainedReadCoalescer::default();
    let start = tokio::time::Instant::now();

    for index in 0..super::PANE_SUSTAINED_READ_MIN_BATCHES - 1 {
        assert!(!coalescer.should_yield_at(128, start + Duration::from_millis(u64::from(index))));
    }
    assert!(coalescer.should_yield_at(128, start + super::PANE_SUSTAINED_READ_MIN_DURATION));
}

#[test]
fn small_read_yield_detector_resets_on_large_read() {
    let mut coalescer = super::SustainedReadCoalescer::default();
    let start = tokio::time::Instant::now();

    for index in 0..super::PANE_SUSTAINED_READ_MIN_BATCHES - 1 {
        let _ = coalescer.should_yield_at(128, start + Duration::from_millis(u64::from(index)));
    }
    assert!(coalescer.should_yield_at(128, start + super::PANE_SUSTAINED_READ_MIN_DURATION));
    assert!(!coalescer.should_yield_at(
        super::PANE_SUSTAINED_SMALL_READ_MAX_BYTES + 1,
        start + super::PANE_SUSTAINED_READ_MIN_DURATION * 2
    ));
    assert!(!coalescer.should_yield_at(128, start + super::PANE_SUSTAINED_READ_MIN_DURATION * 3));
}

#[test]
fn activity_alert_throttle_bounds_no_bell_event_rate() {
    let mut throttle = super::PaneActivityAlertThrottle::default();
    let start = Instant::now();

    assert!(throttle.should_emit_no_bell_alert_at(start));
    assert!(
        !throttle.should_emit_no_bell_alert_at(start + super::PANE_ACTIVITY_ALERT_MIN_INTERVAL / 2)
    );
    assert!(throttle.should_emit_no_bell_alert_at(start + super::PANE_ACTIVITY_ALERT_MIN_INTERVAL));
}

#[tokio::test]
async fn output_reader_writes_terminal_replies_back_to_pane() -> Result<(), Box<dyn Error>> {
    if !python3_available() {
        eprintln!("skipping terminal reply PTY test because python3 is unavailable");
        return Ok(());
    }
    let output = unique_temp_path("terminal-reply");
    let script = r#"
import os, select, sys, termios, tty
old = termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1, b"\x1b[c")
    ready, _, _ = select.select([0], [], [], 10.0)
    data = os.read(0, 64) if ready else b""
    with open(sys.argv[1], "wb") as output:
        output.write(data)
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
"#;
    let mut spawned = ChildCommand::new("python3")
        .args(["-c", script, &output.display().to_string()])
        .size(PtyTerminalSize::new(80, 24))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let pane_output = pane_output_channel();

    let output_reader_task = spawn_pane_output_reader(
        SessionName::new("terminal-reply").expect("valid session name"),
        PaneId::new(1),
        output_reader,
        transcript,
        pane_output,
        None,
        None,
        None,
        PaneReaderRuntime::current().expect("test runtime is active"),
    );

    let contents =
        wait_for_file_contents(&output, b"\x1b[?1;2c".len(), Duration::from_secs(30)).await?;
    let _ = spawned.child_mut().wait();
    output_reader_task.abort();
    let _ = fs::remove_file(&output);

    assert_eq!(contents, b"\x1b[?1;2c");
    Ok(())
}

#[tokio::test]
async fn output_reader_resolves_cursor_blink_from_real_pty() -> Result<(), Box<dyn Error>> {
    let mut spawned = ChildCommand::new("/bin/sh")
        .args(["-c", r#"printf '\033[6 q\033[?12h%s' PTY_CURSOR_OK"#])
        .size(PtyTerminalSize::new(80, 24))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let pane_output = pane_output_channel();

    let output_reader_task = spawn_pane_output_reader(
        SessionName::new("cursor-real-pty").expect("valid session name"),
        PaneId::new(1),
        output_reader,
        transcript.clone(),
        pane_output,
        None,
        None,
        None,
        PaneReaderRuntime::current().expect("test runtime is active"),
    );

    let captured = wait_for_transcript(&transcript, "PTY_CURSOR_OK", Duration::from_secs(4)).await;
    let (raw_style, resolved_style) = {
        let transcript = transcript.lock();
        (
            transcript.screen().cursor_style(),
            transcript.screen().resolved_cursor_style(),
        )
    };
    let _ = spawned.child_mut().wait();
    output_reader_task.abort();

    assert!(captured.contains("PTY_CURSOR_OK"), "got {captured:?}");
    assert_eq!(raw_style, 6);
    assert_eq!(resolved_style, 5);
    Ok(())
}

#[tokio::test]
async fn output_reader_preserves_default_cursor_blink_from_real_pty() -> Result<(), Box<dyn Error>>
{
    let mut spawned = ChildCommand::new("/bin/sh")
        .args([
            "-c",
            r#"printf '\033[0 q\033[?12h%s' PTY_DEFAULT_CURSOR_OK"#,
        ])
        .size(PtyTerminalSize::new(80, 24))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let pane_output = pane_output_channel();

    let output_reader_task = spawn_pane_output_reader(
        SessionName::new("cursor-default-real-pty").expect("valid session name"),
        PaneId::new(1),
        output_reader,
        transcript.clone(),
        pane_output,
        None,
        None,
        None,
        PaneReaderRuntime::current().expect("test runtime is active"),
    );

    let captured =
        wait_for_transcript(&transcript, "PTY_DEFAULT_CURSOR_OK", Duration::from_secs(4)).await;
    let cursor_state = {
        let transcript = transcript.lock();
        rmux_core::resolve_cursor_style_state(
            transcript.screen().cursor_style(),
            transcript.screen().mode(),
            transcript.screen().cursor_default_style(),
        )
    };
    let _ = spawned.child_mut().wait();
    output_reader_task.abort();

    assert!(
        captured.contains("PTY_DEFAULT_CURSOR_OK"),
        "got {captured:?}"
    );
    assert_eq!(cursor_state.style(), 0);
    assert_eq!(cursor_state.default_blink(), Some(true));
    Ok(())
}

#[tokio::test]
async fn async_output_reader_uses_server_runtime_when_spawned_from_temporary_runtime(
) -> Result<(), Box<dyn Error>> {
    // Gate marker production on PTY input without starting an interactive
    // prompt: echoed input can otherwise interleave with shell startup.
    let spawned = ChildCommand::new("sh")
        .args([
            "-c",
            "read _; printf '%s\\n' RMUX_SERVER_RUNTIME_OK; read _",
        ])
        .size(PtyTerminalSize::new(80, 24))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let writer = spawned.master().try_clone()?;
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let pane_output = pane_output_channel();
    let server_runtime = tokio::runtime::Handle::current();
    let transcript_for_assertion = transcript.clone();

    let output_reader_task = std::thread::spawn(move || -> Result<PaneOutputReaderTask, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(async move {
            Ok(spawn_pane_output_reader(
                SessionName::new("temporary-runtime").expect("valid session name"),
                PaneId::new(1),
                output_reader,
                transcript,
                pane_output,
                None,
                None,
                None,
                PaneReaderRuntime::from_handle(server_runtime),
            ))
        })
    })
    .join()
    .map_err(|_| "temporary runtime thread panicked")?
    .map_err(io::Error::other)?;

    writer.write_all(b"start\n")?;
    let captured = wait_for_transcript(
        &transcript_for_assertion,
        "RMUX_SERVER_RUNTIME_OK",
        Duration::from_secs(4),
    )
    .await;

    spawned.child().terminate_forcefully()?;
    output_reader_task.abort();
    drop(writer);
    let (master, mut child) = spawned.into_parts();
    drop(master);

    let reap_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= reap_deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "temporary-runtime PTY child did not exit after forceful termination",
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert!(
        captured.contains("RMUX_SERVER_RUNTIME_OK"),
        "expected marker in transcript, got {captured:?}"
    );
    Ok(())
}

#[tokio::test]
async fn output_reader_keeps_serving_a_pane_with_a_poisoned_transcript(
) -> Result<(), Box<dyn Error>> {
    // Each marker is gated on PTY input, so every published batch follows the
    // poison and the second one needs the reader to survive the first.
    let mut spawned = ChildCommand::new("sh")
        .args([
            "-c",
            "read _; printf '%s\\n' POISON_FIRST_OK; read _; printf '%s\\n' POISON_SECOND_OK; read _",
        ])
        .size(PtyTerminalSize::new(80, 24))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let writer = spawned.master().try_clone()?;
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let pane_output = pane_output_channel();
    transcript.poison_for_test();

    let output_reader_task = spawn_pane_output_reader(
        SessionName::new("poisoned-reader").expect("valid session name"),
        PaneId::new(1),
        output_reader,
        transcript.clone(),
        pane_output.clone(),
        None,
        None,
        None,
        PaneReaderRuntime::current().expect("test runtime is active"),
    );
    let mut captured = Vec::new();
    for (input, needle) in [
        (b"one\n", "POISON_FIRST_OK"),
        (b"two\n", "POISON_SECOND_OK"),
    ] {
        writer.write_all(input)?;
        captured.push(wait_for_recovered_transcript(&transcript, needle).await);
    }
    spawned.child().terminate_forcefully()?;
    output_reader_task.abort();
    let _ = spawned.child_mut().wait();

    assert!(captured[0].contains("POISON_FIRST_OK"), "got {captured:?}");
    assert!(captured[1].contains("POISON_SECOND_OK"), "got {captured:?}");
    drop(pane_output.subscribe());
    assert!(std::panic::catch_unwind(|| drop(transcript.lock())).is_err());
    Ok(())
}

async fn wait_for_recovered_transcript(
    transcript: &crate::pane_transcript::SharedPaneTranscript,
    needle: &str,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let captured = String::from_utf8_lossy(
            &transcript
                .lock_for_recovery_capture()
                .capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .into_owned();
        if captured.contains(needle) || Instant::now() >= deadline {
            return captured;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn unique_temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-pane-reader-{label}-{}-{unique}",
        std::process::id()
    ))
}

async fn wait_for_file_contents(
    path: &Path,
    minimum_len: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        match fs::read(path) {
            Ok(contents) if contents.len() >= minimum_len => return Ok(contents),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Ok(contents) => {
                return Err(format!(
                    "timed out waiting for {} to contain at least {minimum_len} bytes; got {}",
                    path.display(),
                    contents.len()
                )
                .into());
            }
            Err(error) => {
                return Err(format!("timed out waiting for {}: {error}", path.display()).into());
            }
        }
    }
}

async fn wait_for_transcript(
    transcript: &crate::pane_transcript::SharedPaneTranscript,
    needle: &str,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut captured = String::new();
    while Instant::now() < deadline {
        captured = String::from_utf8_lossy(
            &transcript
                .lock()
                .capture_main(ScreenCaptureRange::default(), GridRenderOptions::default()),
        )
        .into_owned();
        if captured.contains(needle) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    captured
}
