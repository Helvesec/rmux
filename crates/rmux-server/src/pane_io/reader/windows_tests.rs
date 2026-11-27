use std::error::Error;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
use rmux_proto::{SessionName, TerminalSize};
use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

use super::{
    expire_due_pane_transcript_timers, spawn_pane_output_reader, PaneOutputEofState,
    PaneTranscriptTimer, PaneTranscriptTimerJob,
};
use crate::pane_io::pane_output_channel;
use crate::pane_transcript::PaneTranscript;

#[test]
fn pane_ground_timer_worker_survives_poisoned_transcript_mutex() {
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let mut timer = transcript
        .lock()
        .append_bytes_with_effects(b"\x1bPunterminated")
        .ground_timer
        .expect("unterminated DCS arms parser ground timer");
    timer.deadline = Instant::now() - Duration::from_millis(1);

    let poisoned = transcript.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.lock();
        panic!("poison pane transcript mutex for timer worker test");
    });

    let mut jobs = vec![PaneTranscriptTimerJob {
        transcript,
        pane_output: pane_output_channel(),
        timer: PaneTranscriptTimer::Ground(timer),
    }];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        expire_due_pane_transcript_timers(&mut jobs);
    }));

    assert!(result.is_ok(), "timer worker must survive poisoned jobs");
    assert!(jobs.is_empty(), "due poisoned job should be consumed");
}

#[test]
fn windows_output_reader_updates_transcript_after_written_input() -> Result<(), Box<dyn Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(PtyTerminalSize::new(100, 30))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let writer = spawned.master().try_clone()?;
    let transcript = PaneTranscript::shared(
        2_000,
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    );
    let pane_output = pane_output_channel();

    spawn_pane_output_reader(
        SessionName::new("alpha").expect("valid session name"),
        PaneId::new(1),
        output_reader,
        transcript.clone(),
        pane_output,
        None,
        PaneOutputEofState::default(),
        None,
        None,
    );

    writer.write_all(b"echo RMUX_READER_OK\r\n")?;
    let captured = wait_for_transcript(&transcript, "RMUX_READER_OK", Duration::from_secs(4));

    spawned.child().terminate_forcefully()?;
    let _ = spawned.child_mut().wait()?;

    assert!(
        captured.contains("RMUX_READER_OK"),
        "expected marker in transcript, got {captured:?}"
    );
    Ok(())
}

#[test]
fn windows_output_reader_publishes_eof_exit_event_after_child_exit() -> Result<(), Box<dyn Error>> {
    let mut spawned = ChildCommand::new("C:\\Windows\\System32\\cmd.exe")
        .args(["/D", "/K"])
        .size(PtyTerminalSize::new(100, 30))
        .spawn()?;
    let output_reader = spawned.master().try_clone()?;
    let writer = spawned.master().try_clone_io()?;
    let transcript = PaneTranscript::shared(
        2_000,
        TerminalSize {
            cols: 100,
            rows: 30,
        },
    );
    let pane_output = pane_output_channel();
    pane_output.set_generation(7);
    let (tx, rx) = mpsc::channel();
    let callback: crate::pane_io::PaneExitCallback = Arc::new(move |event| {
        let _ = tx.send(event.output_eof_published());
    });

    spawn_pane_output_reader(
        SessionName::new("alpha").expect("valid session name"),
        PaneId::new(1),
        output_reader,
        transcript,
        pane_output,
        Some(7),
        PaneOutputEofState::default(),
        None,
        Some(callback),
    );

    writer.write_all(b"exit\r\n")?;
    let _ = spawned.child_mut().wait()?;
    spawned.child().close_pseudoconsole();

    let published = rx.recv_timeout(Duration::from_secs(2))?;
    assert!(
        published,
        "Windows reader must report EOF as already published"
    );
    Ok(())
}

fn wait_for_transcript(
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
        std::thread::sleep(Duration::from_millis(20));
    }
    captured
}
