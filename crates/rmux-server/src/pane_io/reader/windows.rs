use super::*;

#[allow(clippy::too_many_arguments)]
#[cfg(windows)]
pub(super) fn spawn_blocking_pane_output_reader_inner(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
    pane_master: PtyMaster,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    generation: Option<u64>,
    eof_state: PaneOutputEofState,
    pane_alert_callback: Option<PaneAlertCallback>,
    pane_exit_callback: Option<PaneExitCallback>,
) {
    let thread_name = format!("rmux-pane-reader-{}", pane_id.as_u32());
    let session_for_log = session_name.clone();
    let lifecycle_pane_output = pane_output.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(thread_name.clone())
        .spawn(move || {
            #[cfg(test)]
            let _reader_scope = pane_output.reader_probe_for_test().enter(false);
            if let Err(error) = read_pane_output_blocking(
                pane_master,
                session_name.clone(),
                pane_id,
                transcript,
                pane_output,
                generation,
                eof_state,
                pane_alert_callback,
                pane_exit_callback,
            ) {
                lifecycle_pane_output.retire_reader_for_generation(generation);
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
