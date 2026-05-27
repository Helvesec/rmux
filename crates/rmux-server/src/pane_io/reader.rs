use std::io;

use rmux_core::PaneId;
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::{PtyIo, PtyMaster};
use tracing::warn;

#[cfg(unix)]
use super::wire::{open_pane_writer, read_from_pane};
use super::{
    PaneAlertCallback, PaneAlertEvent, PaneExitCallback, PaneExitEvent, PaneOutputSender,
    READ_BUFFER_SIZE,
};
use crate::pane_transcript::SharedPaneTranscript;
use crate::passthrough_replay::{append_to_log, SharedPassthroughReplayLog};

#[allow(clippy::too_many_arguments)]
#[cfg(unix)]
pub(crate) fn spawn_pane_output_reader(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
    replay_log: Option<SharedPassthroughReplayLog>,
) {
    tokio::spawn(async move {
        if let Err(error) = read_pane_output(
            pane_master,
            session_name.clone(),
            pane_id,
            transcript,
            pane_output,
            generation,
            pane_alert_callback,
            pane_exit_callback,
            replay_log,
        )
        .await
        {
            warn!(
                session = %session_name,
                pane_id = pane_id.as_u32(),
                "pane output reader stopped: {error}"
            );
        }
    });
}

#[cfg(windows)]
pub(crate) fn spawn_pane_exit_watcher(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    mut child: PtyChild,
    generation: Option<u64>,
    pane_exit_callback: Option<PaneExitCallback>,
) {
    let Some(pane_exit_callback) = pane_exit_callback else {
        return;
    };
    let thread_name = format!("rmux-pane-exit-{}", pane_id.as_u32());
    let session_for_log = session_name.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            let _ = child.wait();
            child.close_pseudoconsole();
            pane_exit_callback(PaneExitEvent {
                session_name,
                pane_id,
                generation,
            });
        })
    {
        warn!(
            session = %session_for_log,
            pane_id = pane_id.as_u32(),
            thread = %thread_name,
            "failed to spawn pane exit watcher: {error}"
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(windows)]
pub(crate) fn spawn_pane_output_reader(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
    replay_log: Option<SharedPassthroughReplayLog>,
) {
    let thread_name = format!("rmux-pane-reader-{}", pane_id.as_u32());
    let session_for_log = session_name.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            if let Err(error) = read_pane_output_blocking(
                pane_master,
                session_name.clone(),
                pane_id,
                transcript,
                pane_output,
                generation,
                pane_alert_callback,
                pane_exit_callback,
                replay_log,
            ) {
                warn!(
                    session = %session_name,
                    pane_id = pane_id.as_u32(),
                    "pane output reader stopped: {error}"
                );
            }
        })
    {
        warn!(
            session = %session_for_log,
            pane_id = pane_id.as_u32(),
            thread = %thread_name,
            "failed to spawn pane output reader: {error}"
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(unix)]
async fn read_pane_output(
    pane_master: PtyMaster,
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
    replay_log: Option<SharedPassthroughReplayLog>,
) -> io::Result<()> {
    let (pane_reader, reply_writer) = open_pane_writer(pane_master)?;
    let mut buffer = [0_u8; READ_BUFFER_SIZE];

    loop {
        let bytes_read = read_from_pane(&pane_reader, &mut buffer).await?;
        if bytes_read == 0 {
            let _ = pane_output.send_for_generation(generation, Vec::new());
            if let Some(callback) = &pane_exit_callback {
                callback(PaneExitEvent {
                    session_name: session_name.clone(),
                    pane_id,
                    generation,
                });
            }
            return Ok(());
        }

        let bytes = buffer[..bytes_read].to_vec();
        // Order is load-bearing: feed the transcript FIRST so that the
        // replay log's snapshot refresh (triggered by over-budget here,
        // or by alt-screen-exit detection later) sees a screen state
        // that already reflects these bytes — not the previous chunk.
        let append_result = apply_bytes_to_transcript(&transcript, &bytes);
        append_to_log(replay_log.as_ref(), &transcript, &bytes);
        let replies = publish_pane_bytes(
            &session_name,
            pane_id,
            &pane_output,
            generation,
            pane_alert_callback.as_ref(),
            bytes,
            append_result,
        );
        write_parser_replies_to_pane(&reply_writer, replies).await?;
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(windows)]
fn read_pane_output_blocking(
    pane_master: PtyMaster,
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<PaneAlertCallback>,
    _pane_exit_callback: Option<PaneExitCallback>,
    replay_log: Option<SharedPassthroughReplayLog>,
) -> io::Result<()> {
    let pane_reader = pane_master.into_io();
    let mut buffer = [0_u8; READ_BUFFER_SIZE];

    loop {
        let bytes_read = match pane_reader.read(&mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if bytes_read == 0 {
            let _ = pane_output.send_for_generation(generation, Vec::new());
            return Ok(());
        }

        let bytes = buffer[..bytes_read].to_vec();
        // See unix sibling for the load-bearing transcript-first order.
        let append_result = apply_bytes_to_transcript(&transcript, &bytes);
        append_to_log(replay_log.as_ref(), &transcript, &bytes);
        let replies = publish_pane_bytes(
            &session_name,
            pane_id,
            &pane_output,
            generation,
            pane_alert_callback.as_ref(),
            bytes,
            append_result,
        );
        write_parser_replies_to_pane_blocking(&pane_reader, replies)?;
    }
}

/// Feeds a chunk of inner-PTY bytes through the pane's `PaneTranscript`
/// and returns the side effects (parser replies, terminal passthroughs,
/// bell count). Pulled out of [`publish_pane_bytes`] so the reader loop
/// can do this BEFORE [`append_to_log`] runs — that way a snapshot
/// refresh inside the replay log sees a transcript that already
/// reflects the bytes that just arrived.
fn apply_bytes_to_transcript(
    transcript: &SharedPaneTranscript,
    bytes: &[u8],
) -> crate::pane_transcript::PaneAppendResult {
    let mut transcript = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned");
    transcript.append_bytes_with_effects(bytes)
}

/// Publishes `bytes` (already applied to the transcript via
/// [`apply_bytes_to_transcript`]) to the pane's output channel and
/// fires alert / dropped-passthrough side effects. Returns the parser
/// replies the caller must write back to the inner PTY.
fn publish_pane_bytes(
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    pane_output: &PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<&PaneAlertCallback>,
    bytes: Vec<u8>,
    append_result: crate::pane_transcript::PaneAppendResult,
) -> Vec<u8> {
    if !pane_output.accepts_generation(generation) {
        return Vec::new();
    }
    let replies = append_result.replies;
    let dropped_passthrough_count = append_result.dropped_passthrough_count;
    if pane_output
        .send_for_generation_with_passthroughs(generation, bytes, append_result.passthroughs)
        .is_none()
    {
        return Vec::new();
    }
    if dropped_passthrough_count > 0 {
        warn!(
            session = %session_name,
            pane_id = pane_id.as_u32(),
            dropped = dropped_passthrough_count,
            "dropped terminal passthrough events due to parser safety limits"
        );
    }
    if let Some(callback) = pane_alert_callback {
        callback(PaneAlertEvent {
            session_name: session_name.clone(),
            pane_id,
            bell_count: append_result.bell_count,
            generation,
        });
    }
    replies
}

#[cfg(unix)]
async fn write_parser_replies_to_pane(pane_writer: &PtyIo, replies: Vec<u8>) -> io::Result<()> {
    if replies.is_empty() {
        return Ok(());
    }
    let pane_writer = pane_writer.try_clone().map_err(io::Error::other)?;
    tokio::task::spawn_blocking(move || pane_writer.write_all(&replies))
        .await
        .map_err(|error| io::Error::other(format!("parser reply task failed: {error}")))?
}

#[cfg(windows)]
fn write_parser_replies_to_pane_blocking(pane_writer: &PtyIo, replies: Vec<u8>) -> io::Result<()> {
    if replies.is_empty() {
        return Ok(());
    }
    pane_writer.write_all(&replies)
}

#[cfg(all(test, unix))]
mod unix_tests {
    use std::error::Error;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use rmux_core::PaneId;
    use rmux_proto::{SessionName, TerminalSize};
    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::spawn_pane_output_reader;
    use crate::pane_io::pane_output_channel;
    use crate::pane_transcript::PaneTranscript;

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
    ready, _, _ = select.select([0], [], [], 2.0)
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

        spawn_pane_output_reader(
            SessionName::new("terminal-reply").expect("valid session name"),
            PaneId::new(1),
            output_reader,
            transcript,
            pane_output,
            None,
            None,
            None,
            None,
        );

        let contents = wait_for_file_contents(&output, Duration::from_secs(4)).await?;
        let _ = spawned.child_mut().wait();
        let _ = fs::remove_file(&output);

        assert_eq!(contents, b"\x1b[?1;2c");
        Ok(())
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
        timeout: Duration,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            match fs::read(path) {
                Ok(contents) => return Ok(contents),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => {
                    return Err(format!("timed out waiting for {}: {error}", path.display()).into());
                }
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::error::Error;
    use std::time::{Duration, Instant};

    use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
    use rmux_proto::{SessionName, TerminalSize};
    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::spawn_pane_output_reader;
    use crate::pane_io::pane_output_channel;
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn windows_output_reader_updates_transcript_after_written_input() -> Result<(), Box<dyn Error>>
    {
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
            None,
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
                    .expect("pane transcript mutex must not be poisoned")
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
}
