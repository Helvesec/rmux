use super::*;
use crate::pane_io::pane_output_channel;
use crate::pane_transcript::PaneTranscript;
use rmux_proto::{SessionName, TerminalSize};

#[test]
fn program_status_eof_and_stale_generation_preserve_lifecycle() {
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 80, rows: 24 });
    let output = pane_output_channel();
    output.set_generation(1);
    let session = SessionName::new("status-generation").unwrap();
    let context = |generation| PanePublishContext {
        session_name: &session,
        pane_id: PaneId::new(1),
        transcript: &transcript,
        pane_output: &output,
        generation: Some(generation),
        pane_alert_callback: None,
        status_terminal: None,
        emit_no_bell_alert: false,
    };
    publish_pane_bytes(
        context(1),
        b"\x1b]7501;state=working\x07\x1b]7501;state=done:id=result\x07".to_vec(),
    );
    output.reset_generation(2);
    publish_pane_eof(&transcript, &output, Some(1));
    publish_pane_bytes(context(1), b"\x1b]7501;state=clear\x07".to_vec());
    assert_eq!(
        transcript.lock().unwrap().program_status().records().len(),
        2
    );
    publish_pane_eof(&transcript, &output, Some(2));
    publish_pane_bytes(context(2), b"\x1b]7501;state=blocked\x07".to_vec());
    let transcript = transcript.lock().unwrap();
    assert_eq!(transcript.program_status().records().len(), 1);
    assert_eq!(
        transcript
            .program_status()
            .summary()
            .unwrap()
            .state
            .as_str(),
        "done"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn program_status_inline_and_blocking_batches_drain_before_new_reports() {
    for padding in [0, PANE_BLOCKING_PARSE_MIN_BYTES] {
        let transcript = PaneTranscript::shared(10, TerminalSize { cols: 80, rows: 24 });
        let output = pane_output_channel();
        output.set_generation(1);
        let pair = rmux_pty::PtyPair::open().unwrap();
        let context = || OwnedPanePublishContext {
            session_name: SessionName::new("status-batch").unwrap(),
            pane_id: PaneId::new(1),
            transcript: Arc::clone(&transcript),
            pane_output: output.clone(),
            generation: Some(1),
            pane_alert_callback: None,
            status_terminal: Some(pair.master().io().try_clone().unwrap()),
            emit_no_bell_alert: false,
        };
        {
            let mut transcript = transcript.lock().unwrap();
            transcript.observe_foreground_group(Some(100));
            transcript.observe_foreground_group(Some(200));
        }
        // The kernel queue is empty, but a reader-owned batch is still pending.
        // Its final report must be suppressed before the drain can be released.
        let mut bytes = vec![b'x'; padding];
        bytes.extend_from_slice(b"\x1b]7501;state=working\x07");
        publish_pane_bytes_on_blocking_pool(context(), bytes)
            .await
            .unwrap();
        {
            let transcript = transcript.lock().unwrap();
            assert!(transcript.program_status().records().is_empty());
            assert!(!transcript.program_status_transition_pending());
        }
        publish_pane_bytes_on_blocking_pool(context(), b"\x1b]7501;state=working\x07".to_vec())
            .await
            .unwrap();
        assert_eq!(
            transcript.lock().unwrap().program_status().records().len(),
            1
        );

        transcript
            .lock()
            .unwrap()
            .observe_foreground_group(Some(300));
        publish_pane_bytes_on_blocking_pool(context(), b"\x1b]7501;state=wor".to_vec())
            .await
            .unwrap();
        assert!(transcript
            .lock()
            .unwrap()
            .program_status_transition_pending());
        publish_pane_bytes_on_blocking_pool(context(), b"king\x07".to_vec())
            .await
            .unwrap();
        let transcript = transcript.lock().unwrap();
        assert!(transcript.program_status().records().is_empty());
        assert!(!transcript.program_status_transition_pending());
    }
}
