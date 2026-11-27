#[cfg(any(unix, windows))]
mod queue;

#[cfg(any(unix, windows))]
use super::attach_transport::AttachTransport;
#[cfg(any(unix, windows))]
use super::passthrough::{passthrough_requires_cursor_position, render_passthroughs};
#[cfg(any(unix, windows))]
use super::refresh_scheduler::AttachRefreshScheduler;
#[cfg(any(unix, windows))]
use super::types::OpenAttachTarget;
#[cfg(any(unix, windows))]
use super::wire::emit_attach_bytes;
#[cfg(all(test, any(unix, windows)))]
use queue::DEFERRED_PASSTHROUGH_LIMIT;
#[cfg(any(unix, windows))]
pub(super) use queue::{DeferredPassthroughQueue, PaneOutputGeneration};
#[cfg(any(unix, windows))]
use rmux_core::TerminalPassthrough;
#[cfg(any(unix, windows))]
use std::io;

/// Queues live passthroughs under the pane-output generation captured at ring drain.
///
/// This is the deferral entry point the attach loop uses when it decides,
/// before any render, that the client's frame is the wrong place to draw on —
/// today the overlay routes. The batch's drain stamp follows the work through
/// every later queue decision.
#[cfg(any(unix, windows))]
pub(super) fn defer_passthroughs(
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    birth_generation: PaneOutputGeneration,
    passthroughs: Vec<TerminalPassthrough>,
) {
    deferred_passthroughs.defer(birth_generation, passthroughs);
}

/// Retains only source-independent side effects from a batch whose pane source
/// was invalidated before the attach loop could queue it.
#[cfg(any(unix, windows))]
pub(super) fn defer_passthroughs_after_source_invalidation(
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    passthroughs: Vec<TerminalPassthrough>,
) {
    deferred_passthroughs.defer_after_source_invalidation(passthroughs);
}

/// Reports whether a batch still holds a passthrough whose rendered form
/// carries an absolute cursor position, and therefore needs a converged render
/// before it is released.
#[cfg(any(unix, windows))]
pub(super) fn holds_cursor_positioned_passthrough(passthroughs: &[TerminalPassthrough]) -> bool {
    passthroughs
        .iter()
        .any(|passthrough| passthrough_requires_cursor_position(passthrough.kind()))
}

/// The one dispatcher every attach path uses to turn passthroughs into bytes
/// for the frame the client is currently showing.
///
/// It owns the whole decision: which generation the frame belongs to, whether
/// that frame is converged, what is released, what is re-queued, and the
/// refresh deadline that bounds how long the rest waits. No other site may
/// call `render_passthroughs` for live or queued work — a second emitter would
/// hold a payload no scheduler is watching, or release one against a frame
/// this queue was built to protect.
#[cfg(any(unix, windows))]
pub(super) fn take_passthrough_frame_for_client(
    current_target: &OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    pane_refresh: &mut AttachRefreshScheduler,
    birth_generation: PaneOutputGeneration,
    live_passthroughs: Vec<TerminalPassthrough>,
) -> Vec<u8> {
    let frame = take_passthrough_frame_with_live_batches(
        current_target,
        deferred_passthroughs,
        current_target.pane_output_generation(),
        vec![(birth_generation, live_passthroughs)],
        current_target.presentation_frozen(),
    );
    rearm_refresh_for_held_passthroughs(pane_refresh, deferred_passthroughs);
    frame
}

/// Keeps the loop coming back while the queue still owns a positioned
/// passthrough. A live attach receiver is not woken by the synchronized-output
/// timer — that invalidation only reaches recoverable observers — so without
/// this the held payload would wait for the child to speak again instead of
/// for the interval to end. Re-arming bounds the wait by the fail-open
/// deadline whether or not the child ever sends its release.
#[cfg(any(unix, windows))]
fn rearm_refresh_for_held_passthroughs(
    pane_refresh: &mut AttachRefreshScheduler,
    deferred_passthroughs: &DeferredPassthroughQueue,
) {
    if deferred_passthroughs.holds_cursor_positioned() {
        pane_refresh.schedule_sustained();
    }
}

/// The exit drain's dispatcher. It never holds: no later flush exists, so
/// holding would discard the queue instead of deferring it, and no refresh
/// deadline outlives the attach. `finish_pending_attach_exit_with_batch`
/// converges the presentation first and then calls this.
///
/// `current_generation` is read from the receiver retained by the exit drain
/// immediately before dispatch. Each tuple in `live_batches` keeps the birth
/// generation captured when that batch left the ring.
#[cfg(any(unix, windows))]
pub(super) fn drain_passthrough_frame_at_exit(
    current_target: &OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    current_generation: PaneOutputGeneration,
    live_batches: Vec<(PaneOutputGeneration, Vec<TerminalPassthrough>)>,
) -> Vec<u8> {
    take_passthrough_frame_with_live_batches(
        current_target,
        deferred_passthroughs,
        current_generation,
        live_batches,
        false,
    )
}

/// Renders everything this client may see now and re-queues the rest.
#[cfg(any(unix, windows))]
fn take_passthrough_frame_with_live_batches(
    current_target: &OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    current_generation: PaneOutputGeneration,
    live_batches: Vec<(PaneOutputGeneration, Vec<TerminalPassthrough>)>,
    hold_cursor_positioned: bool,
) -> Vec<u8> {
    let renderable = deferred_passthroughs.take_renderable_batches(
        current_generation,
        live_batches,
        hold_cursor_positioned,
    );
    render_passthroughs(current_target, &renderable)
}

#[cfg(any(unix, windows))]
pub(super) fn clear_deferred_passthroughs_if_target_changed(
    target_changed: bool,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
) {
    deferred_passthroughs.clear_if_target_changed(target_changed);
}

/// Releases what a closing overlay was holding. The two deferral reasons are
/// independent, so a positioned passthrough uncovered here stays queued while
/// the presentation screen is still frozen; the render that converges it then
/// carries it out through the ordinary emit path, and the re-armed deadline
/// guarantees that render happens.
#[cfg(any(unix, windows))]
pub(super) async fn flush_deferred_passthroughs(
    stream: &AttachTransport,
    current_target: &OpenAttachTarget,
    deferred_passthroughs: &mut DeferredPassthroughQueue,
    pane_refresh: &mut AttachRefreshScheduler,
    persistent_overlay_visible: bool,
    persistent_overlay_cached: bool,
) -> io::Result<()> {
    if persistent_overlay_visible || persistent_overlay_cached {
        return Ok(());
    }
    let frame = take_passthrough_frame_for_client(
        current_target,
        deferred_passthroughs,
        pane_refresh,
        PaneOutputGeneration::unsubscribed(),
        Vec::new(),
    );
    if frame.is_empty() {
        return Ok(());
    }
    emit_attach_bytes(stream, &frame).await
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use rmux_core::{OptionStore, PaneGeometry, TerminalPaletteIndex, TerminalPassthrough};
    use rmux_proto::{OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize};

    use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
    use crate::pane_io::live_render::LivePaneRender;
    use crate::pane_io::pane_output_channel;
    use crate::pane_transcript::{PaneTranscript, SharedPaneTranscript};

    #[cfg(unix)]
    use super::super::attach_transport::AttachTransport;
    use super::super::refresh_scheduler::AttachRefreshScheduler;
    use super::super::types::{OpenAttachTarget, PaneOutputSender};
    #[cfg(unix)]
    use super::flush_deferred_passthroughs;
    use super::{
        clear_deferred_passthroughs_if_target_changed, defer_passthroughs,
        drain_passthrough_frame_at_exit, take_passthrough_frame_for_client,
        DeferredPassthroughQueue, DEFERRED_PASSTHROUGH_LIMIT,
    };

    const SIZE: TerminalSize = TerminalSize { cols: 40, rows: 4 };

    /// An attached client whose outer terminal accepts all three positioned
    /// protocols plus clipboard writes, backed by a transcript the test drives
    /// through real DEC 2026 transitions.
    fn passthrough_target() -> (OpenAttachTarget, SharedPaneTranscript) {
        let (target, transcript, _pane_output) = passthrough_target_with_pane_output();
        (target, transcript)
    }

    /// The same fixture, keeping the sender so a test can drive the production
    /// respawn boundary (`reset_generation`) on the allocation the target is
    /// still subscribed to.
    fn passthrough_target_with_pane_output(
    ) -> (OpenAttachTarget, SharedPaneTranscript, PaneOutputSender) {
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Global,
                OptionName::SetClipboard,
                "on".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("set-clipboard set succeeds");
        let session_name = SessionName::new("sync-passthrough").expect("valid session name");
        let session = crate::session_fixture::SessionFixture::new(session_name.clone(), SIZE);
        let pane = session
            .window()
            .active_pane()
            .expect("session has an active pane")
            .clone();
        let transcript = PaneTranscript::shared(64, SIZE);
        transcript.lock().append_bytes(b"\x1b[2J\x1b[HOLD");
        let live_pane = LivePaneRender::new_from_transcript(
            transcript.clone(),
            session.view(),
            OptionStore::new(),
            pane,
        )
        .expect("fixture has a live render snapshot");
        let pane_output = pane_output_channel();
        let target = OpenAttachTarget {
            session_name,
            predicted_echo: Default::default(),
            predicted_echo_started_at: None,
            pane_output: Some(pane_output.subscribe()),
            render_frame: Vec::new(),
            outer_terminal: OuterTerminal::resolve(
                &options,
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
            ),
            cursor_style: 0.into(),
            active_pane_geometry: PaneGeometry::new(0, 0, SIZE.cols, SIZE.rows),
            raw_passthrough: true,
            kitty_graphics_passthrough: true,
            sixel_passthrough: true,
            persistent_overlay_state_id: None,
            live_pane: Some(live_pane),
            render_stream: false,
        };
        (target, transcript, pane_output)
    }

    /// Feeds the child bytes and takes the render the client would receive, so
    /// the recorded presentation state describes a frame that was really sent.
    fn feed_and_render(
        target: &mut OpenAttachTarget,
        transcript: &SharedPaneTranscript,
        bytes: &[u8],
    ) {
        transcript.lock().append_bytes(bytes);
        let _ = target
            .live_pane
            .as_mut()
            .expect("fixture keeps a live renderer")
            .render_frame_from_transcript(false);
    }

    fn positioned_passthroughs() -> Vec<TerminalPassthrough> {
        vec![
            TerminalPassthrough::sixel(0, 1, b"q#0!10~".to_vec()),
            TerminalPassthrough::kitty_graphics(0, 2, b"Gf=100;AAAA".to_vec()),
            TerminalPassthrough::raw(0, 3, b"\x1b]52;c;QQ==\x07".to_vec()),
        ]
    }

    #[test]
    fn frozen_presentation_holds_positioned_kinds_and_forwards_side_effects() {
        let (mut target, transcript) = passthrough_target();
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        assert!(
            target.presentation_frozen(),
            "an open synchronized interval must freeze the rendered presentation"
        );

        let mut deferred = DeferredPassthroughQueue::default();
        let mut live = positioned_passthroughs();
        live.push(TerminalPassthrough::clipboard(
            b"\x1b]52;c;QQ==\x07".to_vec(),
        ));
        live.push(TerminalPassthrough::palette_query(
            TerminalPaletteIndex::from(7),
        ));
        let mut pane_refresh = AttachRefreshScheduler::default();
        let frame = take_passthrough_frame_for_client(
            &target,
            &mut deferred,
            &mut pane_refresh,
            target.pane_output_generation(),
            live,
        );

        // Clipboard and palette carry no cursor position, so their side
        // effects stay immediate; a palette query must keep its correlation
        // window.
        assert_eq!(frame, b"\x1b]52;c;QQ==\x07\x1b]4;7;?\x1b\\");
        assert_eq!(deferred.passthroughs(), positioned_passthroughs());
    }

    #[test]
    fn release_flushes_held_passthroughs_in_child_order() {
        let (mut target, transcript) = passthrough_target();
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        let mut deferred = DeferredPassthroughQueue::default();
        let mut pane_refresh = AttachRefreshScheduler::default();
        let held = take_passthrough_frame_for_client(
            &target,
            &mut deferred,
            &mut pane_refresh,
            target.pane_output_generation(),
            positioned_passthroughs(),
        );
        assert!(held.is_empty(), "{held:?}");
        assert!(deferred.holds_cursor_positioned());
        // Retaining positioned work arms the wake that later releases it.
        assert!(
            pane_refresh.is_pending(),
            "a held image must bound its own wait"
        );

        feed_and_render(&mut target, &transcript, b"\x1b[?2026l");
        assert!(
            !target.presentation_frozen(),
            "an explicit release must converge the presentation"
        );
        let released = take_passthrough_frame_for_client(
            &target,
            &mut deferred,
            &mut pane_refresh,
            target.pane_output_generation(),
            Vec::new(),
        );

        assert_eq!(
            released,
            b"\x1b[s\
              \x1b[2;1H\x1bPq#0!10~\x1b\\\
              \x1b[3;1H\x1b_Gf=100;AAAA\x1b\\\
              \x1b[4;1H\x1b]52;c;QQ==\x07\
              \x1b[u"
                .to_vec()
        );
        assert!(deferred.is_empty());
    }

    #[test]
    fn held_passthroughs_stay_bounded_while_the_interval_is_open() {
        let (mut target, transcript) = passthrough_target();
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        let mut deferred = DeferredPassthroughQueue::default();
        let mut pane_refresh = AttachRefreshScheduler::default();

        for index in 0..(DEFERRED_PASSTHROUGH_LIMIT as u32 * 2) {
            let frame = take_passthrough_frame_for_client(
                &target,
                &mut deferred,
                &mut pane_refresh,
                target.pane_output_generation(),
                vec![TerminalPassthrough::sixel(0, index, b"q#0!10~".to_vec())],
            );
            assert!(frame.is_empty(), "{frame:?}");
        }

        assert_eq!(deferred.len(), DEFERRED_PASSTHROUGH_LIMIT);
        // The bound drops from the front, so the newest frames survive.
        assert_eq!(
            deferred.passthroughs().first(),
            Some(&TerminalPassthrough::sixel(
                0,
                DEFERRED_PASSTHROUGH_LIMIT as u32,
                b"q#0!10~".to_vec()
            ))
        );
    }

    #[test]
    fn exit_drain_releases_a_still_frozen_queue_instead_of_discarding_it() {
        let (mut target, transcript) = passthrough_target();
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        let mut deferred = DeferredPassthroughQueue::default();
        let mut pane_refresh = AttachRefreshScheduler::default();
        let _ = take_passthrough_frame_for_client(
            &target,
            &mut deferred,
            &mut pane_refresh,
            target.pane_output_generation(),
            positioned_passthroughs(),
        );
        assert!(!deferred.is_empty());

        // The exit drain has no later flush, so it must not hold even though
        // the child never closed its interval.
        let frame = drain_passthrough_frame_at_exit(
            &target,
            &mut deferred,
            target.pane_output_generation(),
            Vec::new(),
        );

        assert!(!frame.is_empty(), "exit drain must emit the held queue");
        assert!(deferred.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn overlay_close_keeps_positioned_passthroughs_until_the_interval_ends() {
        let (mut target, transcript) = passthrough_target();
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        // The overlay path queues whatever it covers without inspecting kind,
        // so this queue starts holding a clipboard write too.
        let mut deferred = DeferredPassthroughQueue::default();
        let mut covered = positioned_passthroughs();
        covered.push(TerminalPassthrough::clipboard(
            b"\x1b]52;c;QQ==\x07".to_vec(),
        ));
        defer_passthroughs(&mut deferred, target.pane_output_generation(), covered);
        let mut pane_refresh = AttachRefreshScheduler::default();
        let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
        let stream = AttachTransport::from(stream);

        flush_deferred_passthroughs(
            &stream,
            &target,
            &mut deferred,
            &mut pane_refresh,
            true,
            false,
        )
        .await
        .expect("a visible overlay holds everything");
        assert_eq!(deferred.len(), 4);

        // The two deferral reasons are independent: uncovering the pane does
        // not converge the screen the image is positioned against.
        flush_deferred_passthroughs(
            &stream,
            &target,
            &mut deferred,
            &mut pane_refresh,
            false,
            false,
        )
        .await
        .expect("overlay close flush");
        assert_eq!(deferred.passthroughs(), positioned_passthroughs());

        feed_and_render(&mut target, &transcript, b"\x1b[?2026l");
        flush_deferred_passthroughs(
            &stream,
            &target,
            &mut deferred,
            &mut pane_refresh,
            false,
            false,
        )
        .await
        .expect("release flush");
        assert!(deferred.is_empty());
    }

    /// The overlay route is the inherited half of the same root: it queues
    /// whatever an rmux-owned overlay covers, without inspecting kind and
    /// without a target change. A respawn while the overlay is up must still
    /// invalidate the covered image, or the close flush draws the dead
    /// process's pixels on the replacement's pane.
    #[cfg(unix)]
    #[tokio::test]
    async fn overlay_close_discards_covered_work_from_a_replaced_generation() {
        let (mut target, transcript, pane_output) = passthrough_target_with_pane_output();
        // The interval stays open, so the close flush is the site that decides
        // between holding the covered work and discarding it — without a
        // frozen presentation it would release everything either way and the
        // generation would not be observable here.
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        let mut deferred = DeferredPassthroughQueue::default();
        let mut covered = positioned_passthroughs();
        covered.push(TerminalPassthrough::clipboard(
            b"\x1b]52;c;QQ==\x07".to_vec(),
        ));
        defer_passthroughs(&mut deferred, target.pane_output_generation(), covered);
        assert_eq!(deferred.len(), 4);

        // The pane respawns behind the overlay. The target keeps the very same
        // retained sender allocation, so nothing about the source changed.
        pane_output.reset_generation(1);

        let mut pane_refresh = AttachRefreshScheduler::default();
        let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
        let stream = AttachTransport::from(stream);
        flush_deferred_passthroughs(
            &stream,
            &target,
            &mut deferred,
            &mut pane_refresh,
            false,
            false,
        )
        .await
        .expect("overlay close flush");

        assert!(
            deferred.is_empty(),
            "the overlay close kept holding work from a replaced process, which the next \
             converged render would draw on the replacement's pane: {:?}",
            deferred.passthroughs()
        );
        assert!(
            !pane_refresh.is_pending(),
            "nothing is held, so nothing may arm the release deadline"
        );
    }

    #[test]
    fn target_change_discards_a_frozen_queue_except_clipboard_writes() {
        let (mut target, transcript) = passthrough_target();
        feed_and_render(&mut target, &transcript, b"\x1b[?2026h\x1b[2J\x1b[HFINAL");
        let mut deferred = DeferredPassthroughQueue::default();
        let mut pane_refresh = AttachRefreshScheduler::default();
        let mut live = positioned_passthroughs();
        live.insert(
            0,
            TerminalPassthrough::clipboard(b"\x1b]52;c;QQ==\x07".to_vec()),
        );
        let _ = take_passthrough_frame_for_client(
            &target,
            &mut deferred,
            &mut pane_refresh,
            target.pane_output_generation(),
            live,
        );
        // The clipboard write already left immediately; only positioned kinds
        // are queued, and a target change invalidates every one of them.
        assert_eq!(deferred.passthroughs(), positioned_passthroughs());

        clear_deferred_passthroughs_if_target_changed(true, &mut deferred);

        assert!(deferred.is_empty());
        assert!(!deferred.holds_cursor_positioned());
    }

    #[test]
    fn target_change_discards_cursor_position_dependent_passthroughs() {
        let (target, _transcript) = passthrough_target();
        let mut deferred = DeferredPassthroughQueue::default();
        defer_passthroughs(
            &mut deferred,
            target.pane_output_generation(),
            vec![
                TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;AAAA".to_vec()),
                TerminalPassthrough::clipboard(b"\x1b]52;c;QQ==\x07".to_vec()),
            ],
        );

        clear_deferred_passthroughs_if_target_changed(true, &mut deferred);

        assert_eq!(
            deferred.passthroughs(),
            vec![TerminalPassthrough::clipboard(
                b"\x1b]52;c;QQ==\x07".to_vec()
            )]
        );
    }

    #[test]
    fn same_target_keeps_deferred_passthroughs() {
        let (target, _transcript) = passthrough_target();
        let mut deferred = DeferredPassthroughQueue::default();
        defer_passthroughs(
            &mut deferred,
            target.pane_output_generation(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                b"Gf=100;AAAA".to_vec(),
            )],
        );

        clear_deferred_passthroughs_if_target_changed(false, &mut deferred);

        assert_eq!(deferred.len(), 1);
    }
}
