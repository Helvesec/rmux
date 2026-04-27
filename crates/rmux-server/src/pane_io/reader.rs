use std::io;

use rmux_core::PaneId;
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
    let _ = std::thread::Builder::new()
        .name(format!("rmux-pane-reader-{}", pane_id.as_u32()))
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
        });
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
