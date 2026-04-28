use std::io;

use rmux_core::PaneId;
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::PtyMaster;
use tracing::warn;

#[cfg(unix)]
use super::wire::{open_pane_writer, read_from_pane};
use super::{
    PaneAlertCallback, PaneAlertEvent, PaneExitCallback, PaneExitEvent, PaneOutputSender,
    READ_BUFFER_SIZE,
};
use crate::pane_transcript::SharedPaneTranscript;

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
) -> io::Result<()> {
    let pane_reader = open_pane_writer(pane_master)?;
    let mut buffer = [0_u8; READ_BUFFER_SIZE];

    loop {
        let bytes_read = read_from_pane(&pane_reader, &mut buffer).await?;
        if bytes_read == 0 {
            let _ = pane_output.send(Vec::new());
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
        publish_pane_bytes(
            &session_name,
            pane_id,
            &transcript,
            &pane_output,
            generation,
            pane_alert_callback.as_ref(),
            bytes,
        );
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
    pane_exit_callback: Option<PaneExitCallback>,
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
            let _ = pane_output.send(Vec::new());
            if let Some(callback) = &pane_exit_callback {
                callback(PaneExitEvent {
                    session_name: session_name.clone(),
                    pane_id,
                    generation,
                });
            }
            return Ok(());
        }

        publish_pane_bytes(
            &session_name,
            pane_id,
            &transcript,
            &pane_output,
            generation,
            pane_alert_callback.as_ref(),
            buffer[..bytes_read].to_vec(),
        );
    }
}

fn publish_pane_bytes(
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
    transcript: &SharedPaneTranscript,
    pane_output: &PaneOutputSender,
    generation: Option<u64>,
    pane_alert_callback: Option<&PaneAlertCallback>,
    bytes: Vec<u8>,
) {
    let bell_count = {
        let mut transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        transcript.append_bytes(&bytes)
    };
    if let Some(callback) = pane_alert_callback {
        callback(PaneAlertEvent {
            session_name: session_name.clone(),
            pane_id,
            bell_count,
            generation,
        });
    }
    let _ = pane_output.send(bytes);
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
