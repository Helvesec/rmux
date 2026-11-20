use super::*;
use crate::pane_io::{PaneOutputReceiver, PaneOutputSender};
use crate::pane_transcript::{PaneTranscript, SharedPaneTranscript};
use rmux_core::events::OutputCursorItem;

/// The tail ConPTY leaves after its startup DA1 is answered
/// (`rmux-pty/src/backend/windows/startup.rs`). Spawn reads it before respawn
/// returns, but the pane reader publishes it later: in the Windows CI of
/// 2026-11-17 it landed between the sequence snapshot and the stale commit.
const CONPTY_STARTUP_TAIL: &[u8] = b"\x1b[1t\x1b[?1004h\x1b[?9001h";

/// The `remain-on-exit-format` of the exiting generation. The replacement must
/// never show it, on screen or in its ring.
pub(super) const STALE_EXIT_MARKER: &str = "STALE_EXIT_MARKER";

fn contains_stale_exit_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(STALE_EXIT_MARKER.len())
        .any(|part| part == STALE_EXIT_MARKER.as_bytes())
}

#[tokio::test]
async fn stale_exit_reports_replacement_bytes_at_the_historical_assertion() {
    diagnose_stale_exit(None).await;
}

#[tokio::test]
async fn replacement_output_between_snapshot_and_stale_commit_advances_only_its_generation() {
    diagnose_stale_exit(Some(b"\r\nNEW_GENERATION_DIAGNOSTIC\r\n")).await;
}

#[tokio::test]
async fn late_conpty_startup_tail_is_not_attributed_to_the_stale_exit() {
    diagnose_stale_exit(Some(CONPTY_STARTUP_TAIL)).await;
}

/// The replacement's own publications, counted from one sequence snapshot.
///
/// Both sequence spaces are read under the publication lock order, so a reader
/// publication falls before both reads or after both. Each non-empty
/// publication advances the transcript once; a stale remain-on-exit footer
/// advances it without any publication.
pub(super) struct ReplacementPublications {
    output: PaneOutputSender,
    transcript: SharedPaneTranscript,
    raw: PaneOutputReceiver,
    snapshot_ring: u64,
    snapshot_transcript: u64,
}

/// The retained events below one ring boundary and the transcript read with it.
pub(super) struct PublicationBoundary<T> {
    next_ring: u64,
    transcript: u64,
    captured: T,
    events: Vec<(u64, Vec<u8>)>,
}

impl<T> PublicationBoundary<T> {
    /// Every retained event below the boundary, oldest first: the stream a
    /// client subscribing from the oldest retained event replays. The sequence
    /// snapshot plays no part here, so bytes published before it count too.
    fn retained_stream(&self) -> Vec<u8> {
        self.events
            .iter()
            .flat_map(|(_, bytes)| bytes.iter().copied())
            .collect()
    }
}

impl ReplacementPublications {
    pub(super) async fn snapshot(handler: &RequestHandler, target: &PaneTarget) -> Self {
        let state = handler.state.lock().await;
        let output = state
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("replacement output");
        let transcript = state
            .transcript_handle(target)
            .expect("replacement transcript");
        let raw = output.subscribe_from_oldest();
        let (snapshot_ring, snapshot_transcript) =
            output.capture_with_next_sequence(|| transcript.lock().output_sequence());
        Self {
            output,
            transcript,
            raw,
            snapshot_ring,
            snapshot_transcript,
        }
    }

    pub(super) fn boundary<T>(
        &mut self,
        capture: impl FnOnce(&PaneTranscript) -> T,
    ) -> PublicationBoundary<T> {
        let transcript = &self.transcript;
        let (next_ring, (sequence, captured)) = self.output.capture_with_next_sequence(|| {
            let transcript = transcript.lock();
            (transcript.output_sequence(), capture(&transcript))
        });
        let mut events = Vec::new();
        while let Some(item) = self.raw.try_recv() {
            match item {
                OutputCursorItem::Event(event) if event.sequence() < next_ring => {
                    events.push((event.sequence(), event.bytes().to_vec()));
                }
                OutputCursorItem::Event(_) => {}
                gap => panic!("diagnostic cannot attribute bytes across a gap: {gap:?}"),
            }
        }
        PublicationBoundary {
            next_ring,
            transcript: sequence,
            captured,
            events,
        }
    }

    /// The historical assertion, without assuming the replacement is silent.
    ///
    /// Publications are excused only by their content, read on the whole
    /// retained stream the replacement shows: a stale marker split across
    /// publications, even across the sequence snapshot, is still the stale exit
    /// reaching the replacement through its ring. The snapshot only bounds the
    /// publications counted against the transcript.
    pub(super) fn assert_only_publications_advanced<T>(&self, boundary: &PublicationBoundary<T>) {
        let stream = boundary.retained_stream();
        assert!(
            !contains_stale_exit_marker(&stream),
            "stale remain-on-exit marker reached the replacement's retained stream: {stream:?}"
        );
        let publications = boundary
            .events
            .iter()
            .filter(|(sequence, bytes)| *sequence >= self.snapshot_ring && !bytes.is_empty())
            .count();
        let publications = u64::try_from(publications).expect("publication count fits u64");
        assert_eq!(
            boundary.transcript.checked_sub(self.snapshot_transcript),
            Some(publications),
            "stale exit must not append its remain-on-exit marker to the replacement transcript"
        );
    }
}

/// `replacement_output` is published by the replacement generation after the
/// sequence snapshot and before the stale commit. `None` leaves only what the
/// real replacement child publishes on its own, at its own time.
async fn diagnose_stale_exit(replacement_output: Option<&'static [u8]>) {
    let handler = Arc::new(RequestHandler::new());
    let session = create_session(&handler, "a01-kept-exit-byte-diagnostic").await;
    handler.wait_for_initial_panes_for_test().await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists")
    };
    for (name, value) in [
        ("remain-on-exit", "on"),
        ("remain-on-exit-format", STALE_EXIT_MARKER),
    ] {
        let response = handler
            .handle(Request::PaneOptionSet(PaneOptionSetRequest {
                target: PaneTargetRef::slot(target.clone()),
                name: name.to_owned(),
                value: Some(value.to_owned()),
                mode: SetOptionMode::Replace,
                unset: false,
            }))
            .await;
        assert!(
            matches!(response, Response::PaneOptionSet(_)),
            "{response:?}"
        );
    }

    let exited_generation = {
        let mut state = handler.state.lock().await;
        let generation = state.pane_output_generation_for_target(&target, pane_id);
        state
            .mark_pane_dead_without_exit_details(&target)
            .expect("mark initial generation dead");
        generation
    };
    let mut lifecycle_events = handler.subscribe_lifecycle_events();
    while lifecycle_events.try_recv().is_ok() {}
    let pause = handler.install_pane_exit_commit_pause();
    let exit_handler = Arc::clone(&handler);
    let exit_session = session.clone();
    let exiting = tokio::spawn(async move {
        exit_handler
            .handle_pane_exit_event(PaneExitEvent::eof_published(
                exit_session,
                pane_id,
                Some(exited_generation),
            ))
            .await;
    });
    tokio::time::timeout(Duration::from_secs(1), pause.reached.notified())
        .await
        .expect("kept exit reaches post-plan pause");

    let respawned = handler
        .handle(Request::RespawnPane(Box::new(RespawnPaneRequest {
            target: target.clone(),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(vec![crate::test_shell::stdin_discard_command()]),
            process_command: None,
        })))
        .await;
    assert!(
        matches!(respawned, Response::RespawnPane(_)),
        "{respawned:?}"
    );
    let current_generation = {
        let mut state = handler.state.lock().await;
        let generation = state.pane_output_generation_for_target(&target, pane_id);
        state
            .append_bytes_to_runtime_pane_transcript(&session, pane_id, b"NEW_GENERATION_MARKER")
            .expect("append marker to replacement transcript");
        generation
    };
    let mut replacement = ReplacementPublications::snapshot(&handler, &target).await;
    assert_ne!(current_generation, exited_generation);
    let (subscription_id, subscription_revision, subscribed_pane_id) =
        subscribe(&handler, 1004, target.clone(), true).await;
    assert_eq!(subscribed_pane_id, pane_id);

    // This optional, explicitly synthetic publication is ordered after the
    // sequence snapshot and before the stale commit, as a live replacement's
    // own output may be.
    if let Some(replacement_bytes) = replacement_output {
        let transcript = &replacement.transcript;
        replacement
            .output
            .publish_for_generation(
                Some(current_generation),
                replacement_bytes.to_vec(),
                |bytes| {
                    transcript.lock().append_bytes(bytes);
                    ((), Vec::new())
                },
            )
            .expect("current generation accepts the deterministic diagnostic output");
    }

    pause.release.notify_one();
    exiting.await.expect("stale kept-exit task joins");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_output_generation_for_target(&target, pane_id),
            current_generation,
            "stale exit must not replace or rewind the replacement generation"
        );
        let boundary = replacement
            .boundary(|transcript| transcript.capture_main(Default::default(), Default::default()));
        let screen = &boundary.captured;
        let mut injected_event_seen = false;
        for (sequence, bytes) in &boundary.events {
            eprintln!(
                "STALE_EXIT_DIAGNOSTIC generation={current_generation} ring_sequence={sequence} next_ring_sequence={} raw={bytes:?}",
                boundary.next_ring
            );
            injected_event_seen |= Some(bytes.as_slice()) == replacement_output;
        }
        eprintln!(
            "STALE_EXIT_DIAGNOSTIC synthetic={replacement_output:?} snapshot_ring_sequence={} old_generation={exited_generation} new_generation={current_generation} transcript_before={} transcript_after={} screen={screen:?}",
            replacement.snapshot_ring, replacement.snapshot_transcript, boundary.transcript
        );
        for (origin, bytes) in [("screen", screen), ("output", &boundary.retained_stream())] {
            assert!(
                !contains_stale_exit_marker(bytes),
                "stale remain-on-exit marker reached replacement {origin}: {bytes:?}"
            );
        }
        if replacement_output.is_some() {
            assert!(
                injected_event_seen,
                "the exact injected bytes must be retained"
            );
        }
        replacement.assert_only_publications_advanced(&boundary);
        assert!(!state.pane_is_dead(&session, pane_id));
    }

    let changed = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target),
            name: "@new-generation".to_owned(),
            value: Some("open".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(matches!(changed, Response::PaneOptionSet(_)), "{changed:?}");
    match read_cursor(&handler, 1004, subscription_id, subscription_revision).await {
        Response::PaneStateCursor(response) => assert!(matches!(
            response.events.as_slice(),
            [PaneStateEventDto::OptionSet {
                name, new_value, ..
            }] if name == "@new-generation" && new_value == "open"
        )),
        response => panic!("replacement subscription was closed by stale exit: {response:?}"),
    }

    while let Ok(event) = lifecycle_events.try_recv() {
        assert!(
            !matches!(
                event.event,
                LifecycleEvent::PaneDied {
                    pane_id: Some(event_pane_id),
                    ..
                } if event_pane_id == pane_id.as_u32()
            ),
            "stale exit emitted PaneDied for the replacement generation"
        );
    }
}
