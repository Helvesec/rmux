use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{
    expire_due_pane_transcript_timers, osc52_clipboard_write_payload,
    osc52_payload_is_clipboard_set, publish_pane_bytes, publish_pane_eof,
    schedule_pane_transcript_timer_with_sender, PanePublishContext, PaneTranscriptTimer,
    PaneTranscriptTimerJob,
};
use rmux_core::{
    input::InputEndType, GridRenderOptions, PaneId, ScreenCaptureRange, TerminalClipboardQuery,
    TerminalPassthrough, TerminalPassthroughKind,
};
use rmux_proto::{SessionName, TerminalSize};

use crate::pane_io::{pane_output_channel, PaneAlertCallback};
use crate::pane_transcript::PaneTranscript;

fn transcript_screen_contains(
    transcript: &crate::pane_transcript::SharedPaneTranscript,
    presentation: bool,
    needle: &[u8],
) -> bool {
    let transcript = transcript.lock();
    let screen = if presentation {
        transcript.presentation_screen()
    } else {
        transcript.screen()
    };
    screen
        .capture_transcript(ScreenCaptureRange::default(), GridRenderOptions::default())
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn synchronized_output_timeout_invalidates_and_publishes_pending_state() {
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 48, rows: 4 });
    let output = pane_output_channel();
    let mut observer = output.subscribe();
    let mut timer = {
        let mut transcript = transcript.lock();
        transcript.append_bytes(b"\x1b[2J\x1b[HSTABLE_BEFORE_SYNC");
        transcript
            .append_bytes_with_effects(b"\x1b[?2026h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY")
            .synchronized_output_timer
            .expect("mode 2026 admission arms its fail-open timer")
    };
    assert!(transcript_screen_contains(
        &transcript,
        true,
        b"STABLE_BEFORE_SYNC"
    ));
    assert!(transcript_screen_contains(
        &transcript,
        false,
        b"PAUSED_PARTIAL_ONLY"
    ));

    timer.deadline = Instant::now() - Duration::from_millis(1);
    let mut jobs = vec![PaneTranscriptTimerJob {
        transcript: transcript.clone(),
        pane_output: output,
        timer: PaneTranscriptTimer::SynchronizedOutput(timer),
    }];
    expire_due_pane_transcript_timers(&mut jobs);

    assert!(jobs.is_empty());
    assert!(transcript_screen_contains(
        &transcript,
        true,
        b"PAUSED_PARTIAL_ONLY"
    ));
    let Some(crate::pane_io::PaneObservationItem::Invalidated(invalidation)) =
        observer.try_recv_observed()
    else {
        panic!("timeout release must wake structured subscribers");
    };
    assert_eq!(
        invalidation.reason,
        crate::pane_io::PaneInvalidationReason::ParserStateExpired
    );
}

#[test]
fn synchronized_output_timer_send_failure_does_not_latch_job_pending() {
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 48, rows: 4 });
    let timer = transcript
        .lock()
        .append_bytes_with_effects(b"\x1b[?2026hFIRST_PARTIAL")
        .synchronized_output_timer
        .expect("first synchronized admission arms a timer");
    let original_deadline = timer.deadline;
    let (timer_tx, timer_rx) = mpsc::channel();
    drop(timer_rx);
    let session_name = SessionName::new("sync-send-failure").expect("valid session name");

    schedule_pane_transcript_timer_with_sender(
        &timer_tx,
        &session_name,
        PaneId::new(1),
        transcript.clone(),
        pane_output_channel(),
        PaneTranscriptTimer::SynchronizedOutput(timer),
    );

    let rearmed = transcript
        .lock()
        .append_bytes_with_effects(b"SECOND_PARTIAL")
        .synchronized_output_timer
        .expect("a failed scheduler send must return timer ownership to the transcript");
    assert_eq!(
        rearmed.deadline, original_deadline,
        "re-arming an unchanged interval must retain its original fail-open deadline"
    );
}

#[test]
fn synchronized_output_timer_panic_does_not_latch_job_pending() {
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 48, rows: 4 });
    let mut timer = transcript
        .lock()
        .append_bytes_with_effects(b"\x1b[?2026hFIRST_PARTIAL")
        .synchronized_output_timer
        .expect("first synchronized admission arms a timer");
    timer.deadline = Instant::now() - Duration::from_millis(1);
    let pane_output = pane_output_channel();
    pane_output.poison_state_for_test();
    let mut jobs = vec![PaneTranscriptTimerJob {
        transcript: transcript.clone(),
        pane_output,
        timer: PaneTranscriptTimer::SynchronizedOutput(timer),
    }];

    expire_due_pane_transcript_timers(&mut jobs);

    assert!(jobs.is_empty(), "the panicked timer job must be consumed");
    let rearmed = transcript
        .lock()
        .append_bytes_with_effects(b"SECOND_PARTIAL")
        .synchronized_output_timer;
    assert!(
        rearmed.is_some(),
        "a job lost across the panic boundary must release timer ownership"
    );
}

#[test]
fn pane_eof_releases_unclosed_synchronized_output_before_close() {
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 48, rows: 4 });
    let output = pane_output_channel();
    let mut legacy = output.subscribe();
    {
        let mut transcript = transcript.lock();
        transcript.append_bytes(b"\x1b[2J\x1b[HSTABLE_BEFORE_SYNC");
        transcript.append_bytes(b"\x1b[?2026h\x1b[2J\x1b[HFINAL_WITHOUT_MODE_RELEASE");
    }

    publish_pane_eof(&transcript, &output, None);

    assert!(transcript_screen_contains(
        &transcript,
        true,
        b"FINAL_WITHOUT_MODE_RELEASE"
    ));
    let rmux_core::events::OutputCursorItem::Event(close) = legacy
        .try_recv()
        .expect("legacy subscriber receives the exact EOF event")
    else {
        panic!("EOF must remain a strict pane-output event");
    };
    assert!(close.is_empty());

    let rearmed = transcript
        .lock()
        .append_bytes_with_effects(b"\x1b[?2026hREPLACEMENT_PROCESS_PARTIAL")
        .synchronized_output_timer;
    assert!(
        rearmed.is_some(),
        "EOF must not leave a discarded timer latched across replacement"
    );
}

#[test]
fn synchronized_output_keeps_strict_data_bytes_while_structured_state_is_stable() {
    let transcript = PaneTranscript::shared(100, TerminalSize { cols: 48, rows: 4 });
    transcript
        .lock()
        .append_bytes(b"\x1b[2J\x1b[HSTABLE_BEFORE_SYNC");
    let output = pane_output_channel();
    let mut raw = output.subscribe();
    let session_name = SessionName::new("sync-strict-data").expect("valid session name");
    let context = || PanePublishContext {
        session_name: &session_name,
        pane_id: PaneId::new(1),
        transcript: &transcript,
        pane_output: &output,
        generation: None,
        pane_alert_callback: None,
        status_terminal: None,
        emit_no_bell_alert: false,
    };
    let paused = b"\x1b[?2026h\x1b[2J\x1b[HPAUSED_PARTIAL_ONLY";
    let released = b"\x1b[2J\x1b[HPAUSED_FINAL\x1b[?2026l";

    let _ = publish_pane_bytes(context(), paused.to_vec());
    let rmux_core::events::OutputCursorItem::Event(paused_event) = raw
        .try_recv()
        .expect("raw subscriber receives paused bytes")
    else {
        panic!("paused bytes remain a strict output event");
    };
    assert_eq!(paused_event.bytes(), paused);
    assert!(transcript_screen_contains(
        &transcript,
        true,
        b"STABLE_BEFORE_SYNC"
    ));

    let _ = publish_pane_bytes(context(), released.to_vec());
    let rmux_core::events::OutputCursorItem::Event(released_event) = raw
        .try_recv()
        .expect("raw subscriber receives release bytes")
    else {
        panic!("release bytes remain a strict output event");
    };
    assert_eq!(released_event.bytes(), released);
    assert!(transcript_screen_contains(
        &transcript,
        true,
        b"PAUSED_FINAL"
    ));
}

#[test]
fn osc52_clipboard_set_requires_write_payload() {
    assert!(osc52_payload_is_clipboard_set(b"\x1b]52;c;aGk=\x07"));
    assert!(osc52_payload_is_clipboard_set(b"\x1b]52;c;aGk\x1b\\"));
    assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;?\x07"));
    assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;%%\x07"));
    assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;abcde\x07"));
    // The classifier must reject the same payloads the write decoder
    // rejects, otherwise the PaneSetClipboard hook fires without a stored
    // buffer (diverges from the frozen tmux 3.7b oracle: paste_add and
    // notify_pane must fire together or not at all).
    assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;\x07"));
    assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;==\x07"));
    assert!(!osc52_payload_is_clipboard_set(b"\x1b]52;c;!!!\x07"));
}

#[test]
fn decode_base64_standard_handles_padding_and_missing_padding() {
    assert_eq!(
        crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"aGVsbG8=").as_deref(),
        Some(&b"hello"[..])
    );
    // OSC 52 producers sometimes omit the trailing padding.
    assert_eq!(
        crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"aGVsbG8").as_deref(),
        Some(&b"hello"[..])
    );
    assert_eq!(
        crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"aGk=").as_deref(),
        Some(&b"hi"[..])
    );
    assert_eq!(
        crate::clipboard_protocol::decode_pane_clipboard_write_payload(b""),
        None
    );
    // Invalid symbols / lengths are rejected rather than decoded to garbage.
    assert_eq!(
        crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"%%"),
        None
    );
    assert_eq!(
        crate::clipboard_protocol::decode_pane_clipboard_write_payload(b"abcde"),
        None
    );
}

#[test]
fn osc52_clipboard_write_payload_decodes_writes_and_skips_queries() {
    assert_eq!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
            b"\x1b]52;c;aGVsbG8=\x07".to_vec()
        ))
        .as_deref(),
        Some(&b"hello"[..])
    );
    // The ST terminator form decodes identically.
    assert_eq!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
            b"\x1b]52;c;aGk\x1b\\".to_vec()
        ))
        .as_deref(),
        Some(&b"hi"[..])
    );
    // A query carries no write payload.
    assert!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(b"\x1b]52;c;?\x07".to_vec()))
            .is_none()
    );
    // A non-clipboard passthrough is ignored.
    assert!(osc52_clipboard_write_payload(&TerminalPassthrough::raw(
        0,
        0,
        b"\x1b]52;c;aGk=\x07".to_vec()
    ))
    .is_none());
}

#[test]
fn osc52_empty_payload_is_dropped_matching_oracle() {
    // tmux 3.7b's input_osc_52 returns early on an empty payload — no
    // paste_add, no outer forward. rmux must not create an empty buffer.
    assert!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(b"\x1b]52;c;\x07".to_vec()))
            .is_none()
    );
    // Neither should a base64 payload that decodes to zero bytes (a lone
    // padding sequence).
    assert!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
            b"\x1b]52;c;==\x07".to_vec()
        ))
        .is_none()
    );
}

#[test]
fn osc52_invalid_base64_payload_is_dropped_matching_oracle() {
    assert!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
            b"\x1b]52;c;!!!\x07".to_vec()
        ))
        .is_none()
    );
    assert!(
        osc52_clipboard_write_payload(&TerminalPassthrough::clipboard(
            b"\x1b]52;c;@@@@\x07".to_vec()
        ))
        .is_none()
    );
}

#[test]
fn title_alert_callback_runs_inside_the_transcript_publication_boundary() {
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let callback_transcript = transcript.clone();
    let callback_observed = Arc::new(AtomicBool::new(false));
    let callback_observed_clone = Arc::clone(&callback_observed);
    let callback: PaneAlertCallback = Arc::new(crate::pane_io::PaneEffectSink::new(
        move |event| {
            assert!(event.title_change.is_some(), "OSC 2 must change the title");
            assert!(
                callback_transcript.lock_unavailable_for_test(),
                "the title callback must run before the transcript publication lock is released"
            );
            callback_observed_clone.store(true, Ordering::Release);
        },
        Arc::new(crate::pane_rename_queue::PaneRenameQueue::new()),
    ));
    let output = pane_output_channel();
    let session_name = SessionName::new("title-linearization").expect("valid session name");

    let _ = publish_pane_bytes(
        PanePublishContext {
            session_name: &session_name,
            pane_id: PaneId::new(1),
            transcript: &transcript,
            pane_output: &output,
            generation: None,
            pane_alert_callback: Some(&callback),
            status_terminal: None,
            emit_no_bell_alert: false,
        },
        b"\x1b]2;linearized-title\x07".to_vec(),
    );

    assert!(callback_observed.load(Ordering::Acquire));
}

#[test]
fn mouse_mode_alert_is_stamped_by_production_pane_publication() {
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let callback_transcript = transcript.clone();
    let callback_observed = Arc::new(AtomicBool::new(false));
    let callback_observed_clone = Arc::clone(&callback_observed);
    let callback: PaneAlertCallback = Arc::new(crate::pane_io::PaneEffectSink::new(
        move |event| {
            assert!(
                event.mouse_mode_changed,
                "the reader publication path must stamp a real mouse-mode transition"
            );
            assert!(
                callback_transcript.lock_unavailable_for_test(),
                "mouse-mode comparison and callback must share the transcript boundary"
            );
            callback_observed_clone.store(true, Ordering::Release);
        },
        Arc::new(crate::pane_rename_queue::PaneRenameQueue::new()),
    ));
    let output = pane_output_channel();
    let session_name = SessionName::new("mouse-mode-stamping").expect("valid session name");

    let _ = publish_pane_bytes(
        PanePublishContext {
            session_name: &session_name,
            pane_id: PaneId::new(1),
            transcript: &transcript,
            pane_output: &output,
            generation: None,
            pane_alert_callback: Some(&callback),
            status_terminal: None,
            emit_no_bell_alert: false,
        },
        b"\x1b[?1003h".to_vec(),
    );

    assert!(callback_observed.load(Ordering::Acquire));
}

#[test]
fn production_publication_invalidates_recovery_at_the_post_rep_boundary() {
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 8, rows: 2 });
    let output = pane_output_channel();
    let mut recovery = output.subscribe();
    let mut legacy = output.subscribe();
    let session_name = SessionName::new("rep-recovery").expect("valid session name");
    let context = || PanePublishContext {
        session_name: &session_name,
        pane_id: PaneId::new(1),
        transcript: &transcript,
        pane_output: &output,
        generation: None,
        pane_alert_callback: None,
        status_terminal: None,
        emit_no_bell_alert: false,
    };

    let _ = publish_pane_bytes(context(), b"X".to_vec());
    assert!(matches!(
        recovery.try_recv_observed(),
        Some(crate::pane_io::PaneObservationItem::Output(
            rmux_core::events::OutputCursorItem::Event(event)
        )) if event.bytes() == b"X"
    ));
    let _ = legacy.try_recv().expect("legacy subscriber receives X");

    let _ = publish_pane_bytes(context(), b"\x1b[2b".to_vec());

    let Some(crate::pane_io::PaneObservationItem::Invalidated(invalidation)) =
        recovery.try_recv_observed()
    else {
        panic!("recoverable subscriber must skip REP and rebase");
    };
    assert_eq!(
        invalidation.reason,
        crate::pane_io::PaneInvalidationReason::TranscriptMutation
    );
    assert_eq!(invalidation.boundary.next_output_sequence, 2);
    assert!(
        recovery.try_recv_observed().is_none(),
        "the non-replayable REP event must not follow its invalidation"
    );

    let rmux_core::events::OutputCursorItem::Event(event) =
        legacy.try_recv().expect("legacy attach still receives REP")
    else {
        panic!("legacy REP must remain an output event");
    };
    assert_eq!(event.bytes(), b"\x1b[2b");
}

#[test]
fn clipboard_query_is_typed_by_production_pane_publication() {
    let transcript = PaneTranscript::shared(2_000, TerminalSize { cols: 80, rows: 24 });
    let callback_observed = Arc::new(AtomicBool::new(false));
    let callback_observed_clone = Arc::clone(&callback_observed);
    let callback: PaneAlertCallback = Arc::new(crate::pane_io::PaneEffectSink::new(
        move |event| {
            assert!(
                !event.clipboard_set,
                "a query must not be classified as a write"
            );
            assert!(event.clipboard_writes.is_empty());
            assert_eq!(
                event.clipboard_queries,
                vec![TerminalClipboardQuery::new("zzpc", InputEndType::St)]
            );
            callback_observed_clone.store(true, Ordering::Release);
        },
        Arc::new(crate::pane_rename_queue::PaneRenameQueue::new()),
    ));
    let output = pane_output_channel();
    let mut output_rx = output.subscribe();
    let session_name = SessionName::new("clipboard-query-stamping").expect("valid session name");

    let _ = publish_pane_bytes(
        PanePublishContext {
            session_name: &session_name,
            pane_id: PaneId::new(1),
            transcript: &transcript,
            pane_output: &output,
            generation: None,
            pane_alert_callback: Some(&callback),
            status_terminal: None,
            emit_no_bell_alert: false,
        },
        b"\x1b]52;zzpc;?\x1b\\".to_vec(),
    );

    assert!(callback_observed.load(Ordering::Acquire));
    let frame = output_rx
        .try_recv()
        .expect("published pane frame remains observable");
    let rmux_core::events::OutputCursorItem::Event(frame) = frame else {
        panic!("published pane frame must be an event");
    };
    assert_eq!(frame.passthroughs().len(), 1);
    assert_eq!(
        frame.passthroughs()[0].kind(),
        TerminalPassthroughKind::Clipboard
    );
    assert!(
        frame.passthroughs()[0].render_sequence().is_empty(),
        "the generic passthrough renderer must never leak a query outward"
    );
}
