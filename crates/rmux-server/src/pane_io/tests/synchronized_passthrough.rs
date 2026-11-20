//! DEC 2026 freezes the presentation screen every render reader resolves
//! through; live passthroughs preserve `text -> image` order in `forward_attach`.

use super::*;
#[path = "graphics_effects.rs"]
mod graphics_effects;
#[path = "graphics_restore.rs"]
mod graphics_restore;
use crate::pane_io::live_render::LivePaneRender;
use crate::pane_io::reader::{publish_pane_bytes_for_test, publish_pane_eof_for_test};
use crate::pane_io::take_passthrough_frame_for_client;
use crate::pane_io::types::{OpenAttachTarget, PaneOutputSender};
use crate::pane_transcript::{PaneTranscript, SharedPaneTranscript};
use crate::renderer::{render_pane_screen, PaneRenderDelta};
const PANE_SIZE: TerminalSize = TerminalSize { cols: 40, rows: 5 };
const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Emitted immediately even inside a frozen interval, so both the corrected
/// and the uncorrected server reach this point before the test continues.
const CLIPBOARD_MARKER: &[u8] = b"\x1b]52;c;QQ==\x07";
const KITTY_MARKER: &[u8] = b"\x1b_Gf=100;SU1H\x1b\\";
const SIXEL_MARKER: &[u8] = b"\x1bPq#0!10~\x1b\\";
const FINAL_TEXT: &[u8] = b"FINALTEXT";
/// Body of the transcript a respawned child writes, so the client's frame
/// after the respawn is distinguishable from the dead process's.
const NEW_PROCESS_TEXT: &[u8] = b"NEWPROCESS";
/// Distinguishes the replacement pane from the pane that produced the image.
const FOREIGN_PANE_TEXT: &[u8] = b"FOREIGNPANE";
/// Positive proof that the replacement pane's scheduled refresh was serviced.
const FOREIGN_PANE_REFRESH_PROBE: &[u8] = b"FOREIGNREFRESH";
/// Text drained with EOF from the pane that owns `close_after_render`.
const CLOSE_OWNER_TEXT: &[u8] = b"CLOSEOWNER";
/// A live side effect published by the replacement pane after its frame lands.
const FOREIGN_PANE_LIVE_CLIPBOARD: &[u8] = b"\x1b]52;c;Qg==\x07";
/// Runs after a pending Refresh arm has applied its close decision.
const CLOSE_APPLIED_BARRIER: &[u8] = b"CLOSEAPPLIED";
/// Wakes the same-source receiver while its presentation remains frozen.
const SAME_SOURCE_PENDING_PROBE: &[u8] = b"SAMESOURCEPENDING";
/// A replacement-generation side effect used as a FIFO completion probe.
const GENERATION_DRAIN_PROBE: &[u8] = b"\x1b]52;c;RFJBSU5FRA==\x07";

/// Accumulates decoded attach payload across several waits. The stock helper
/// returns and then drops what it consumed, which would hide whichever marker
/// arrived in an earlier frame.
struct AttachReader {
    peer: tokio::net::UnixStream,
    decoder: AttachFrameDecoder,
    collected: Vec<u8>,
}

impl AttachReader {
    fn new(peer: tokio::net::UnixStream) -> Self {
        Self {
            peer,
            decoder: AttachFrameDecoder::new(),
            collected: Vec::new(),
        }
    }

    fn contains(&self, needle: &[u8]) -> bool {
        self.position(needle).is_some()
    }

    fn position(&self, needle: &[u8]) -> Option<usize> {
        self.collected
            .windows(needle.len())
            .position(|window| window == needle)
    }

    async fn read_until(&mut self, needle: &[u8]) {
        if self.contains(needle) {
            return;
        }
        let mut frame_bytes = [0_u8; 4096];
        tokio::time::timeout(READ_TIMEOUT, async {
            loop {
                let bytes_read = self
                    .peer
                    .read(&mut frame_bytes)
                    .await
                    .expect("read peer bytes");
                assert!(bytes_read > 0, "attach stream closed before expected data");
                self.decoder.push_bytes(&frame_bytes[..bytes_read]);
                while let Some(message) = self.decoder.next_message().expect("decode attach frame")
                {
                    if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
                        self.collected.extend_from_slice(&bytes);
                    }
                }
                if self.contains(needle) {
                    return;
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {needle:?} in {:?}",
                String::from_utf8_lossy(&self.collected)
            )
        });
    }

    /// Waits for both markers regardless of which the server emits first, then
    /// reports the order the client actually observed.
    async fn ordered_positions(&mut self, first: &[u8], second: &[u8]) -> (usize, usize) {
        self.read_until(first).await;
        self.read_until(second).await;
        (
            self.position(first).expect("first marker was awaited"),
            self.position(second).expect("second marker was awaited"),
        )
    }
}

fn clipboard_options() -> OptionStore {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::SetClipboard,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set-clipboard set succeeds");
    options
}

fn pane_transcript_for(session: rmux_core::ManagedSessionView<'_>) -> SharedPaneTranscript {
    let pane = session.window().active_pane().expect("active pane");
    PaneTranscript::shared(
        128,
        TerminalSize {
            cols: pane.geometry().cols(),
            rows: pane.geometry().rows().saturating_sub(1).max(1),
        },
    )
}

fn append(transcript: &SharedPaneTranscript, bytes: &[u8]) {
    transcript.lock().append_bytes(bytes);
}

async fn wait_for_receiver_count(pane_output: &PaneOutputSender, expected: usize) {
    tokio::time::timeout(READ_TIMEOUT, async {
        while pane_output.receiver_count_for_test() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "timed out waiting for {expected} pane-output receivers; observed {}",
            pane_output.receiver_count_for_test()
        )
    });
}

fn synchronized_passthrough_target(
    session_name: &SessionName,
    pane_output: &PaneOutputSender,
    term: &str,
) -> (AttachTarget, SharedPaneTranscript) {
    let session = crate::session_fixture::SessionFixture::new(session_name.clone(), PANE_SIZE);
    let pane = session
        .window()
        .active_pane()
        .expect("session has an active pane")
        .clone();
    let transcript = pane_transcript_for(session.view());
    // The literal frame below was not rendered from a presentation screen, so
    // there is no frozen interval for the client's first frame to be part of.
    let live_pane = LivePaneRender::new_from_transcript(
        transcript.clone(),
        session.view(),
        OptionStore::new(),
        pane,
    )
    .expect("attach fixture has a live render snapshot");
    let pty = PtyPair::open().expect("open pty pair");
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: session_name.clone(),
        pane_master: Some(pty.into_master()),
        pane_output,
        pane_output_start_sequence,
        render_frame: b"BASE-0".to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &clipboard_options(),
            OuterTerminalContext::from_pairs(&[("TERM", term)]),
        ),
        client_title: None,
        cursor_style: 0.into(),
        active_pane_geometry: PaneGeometry::new(0, 0, PANE_SIZE.cols, PANE_SIZE.rows),
        raw_passthrough: true,
        kitty_graphics_passthrough: true,
        sixel_passthrough: true,
        persistent_overlay_state_id: None,
        live_pane: Some(live_pane),
    };
    (target, transcript)
}

/// A same-source plain render refresh whose prebuilt frame is rendered from
/// the transcript's current presentation screen — exactly what the attach
/// target builder produces, including the state of that read.
fn same_source_switch_target(
    session_name: &SessionName,
    pane_output: &PaneOutputSender,
    transcript: &SharedPaneTranscript,
    term: &str,
) -> AttachTarget {
    let session = crate::session_fixture::SessionFixture::new(session_name.clone(), PANE_SIZE);
    let pane = session
        .window()
        .active_pane()
        .expect("session has an active pane")
        .clone();
    let capture = transcript.lock().capture_presentation_screen();
    let render_frame =
        render_pane_screen(session.view(), &OptionStore::new(), &pane, capture.screen());
    let live_pane = LivePaneRender::new_from_presentation_capture(
        transcript.clone(),
        session.view(),
        &OptionStore::new(),
        &pane,
        capture,
    )
    .expect("switch target has a live render snapshot");
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    AttachTarget {
        session_name: session_name.clone(),
        pane_master: None,
        pane_output,
        pane_output_start_sequence,
        render_frame,
        outer_terminal: OuterTerminal::resolve(
            &clipboard_options(),
            OuterTerminalContext::from_pairs(&[("TERM", term)]),
        ),
        client_title: None,
        cursor_style: 0.into(),
        active_pane_geometry: PaneGeometry::new(0, 0, PANE_SIZE.cols, PANE_SIZE.rows),
        raw_passthrough: true,
        kitty_graphics_passthrough: true,
        sixel_passthrough: true,
        persistent_overlay_state_id: None,
        live_pane: Some(live_pane),
    }
}

struct AttachFixture {
    reader: AttachReader,
    transcript: SharedPaneTranscript,
    pane_output: PaneOutputSender,
    control_tx: mpsc::UnboundedSender<AttachControl>,
    attach_task: tokio::task::JoinHandle<std::io::Result<()>>,
    shutdown_tx: watch::Sender<()>,
}

impl AttachFixture {
    async fn start(name: &str, term: &str) -> Self {
        let handler = Arc::new(RequestHandler::new());
        let session_name = SessionName::new(name).expect("valid session name");
        let (stream, peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let pane_output = pane_output_channel();
        let (target, transcript) =
            synchronized_passthrough_target(&session_name, &pane_output, term);
        let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());
        let attach_task = tokio::spawn(forward_attach(
            stream,
            target,
            Vec::new(),
            shutdown_rx,
            control_rx,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            live_input,
            false,
        ));
        let mut fixture = Self {
            reader: AttachReader::new(peer),
            transcript,
            pane_output,
            control_tx,
            attach_task,
            shutdown_tx,
        };
        fixture.reader.read_until(b"BASE-0").await;
        fixture
    }

    fn publish(&self, bytes: &[u8]) {
        publish_pane_bytes_for_test(&self.transcript, &self.pane_output, bytes.to_vec());
    }

    fn send(&self, control: AttachControl) {
        self.control_tx.send(control).expect("send attach control");
    }

    fn switch_target(&self, name: &str, term: &str) -> AttachTarget {
        let session_name = SessionName::new(name).expect("valid session name");
        same_source_switch_target(&session_name, &self.pane_output, &self.transcript, term)
    }

    /// The refresh a respawn sends to attached clients: the same retained
    /// pane-output source, but a converged transcript belonging to the
    /// replacement process rather than the dead one.
    fn respawn_refresh_target(&self, name: &str, term: &str) -> AttachTarget {
        let session_name = SessionName::new(name).expect("valid session name");
        let session = crate::session_fixture::SessionFixture::new(session_name.clone(), PANE_SIZE);
        let replacement_transcript = pane_transcript_for(session.view());
        append(&replacement_transcript, NEW_PROCESS_TEXT);
        same_source_switch_target(
            &session_name,
            &self.pane_output,
            &replacement_transcript,
            term,
        )
    }

    async fn shutdown(self) {
        self.shutdown_tx.send(()).expect("request attach shutdown");
        drop(self.control_tx);
        let result = self.attach_task.await.expect("attach task join");
        assert!(
            result.is_ok(),
            "forward_attach should stay healthy: {result:?}"
        );
    }
}

/// `?2026h`, the final text, a positioned image and an OSC 52 write, exactly
/// the shape the certification measured. The clipboard write is the barrier:
/// it is not position dependent, so it leaves immediately either way.
fn held_interval_chunk(image: &[u8]) -> Vec<u8> {
    let mut chunk = b"\x1b[?2026h\x1b[2J\x1b[H".to_vec();
    chunk.extend_from_slice(FINAL_TEXT);
    chunk.extend_from_slice(image);
    chunk.extend_from_slice(CLIPBOARD_MARKER);
    chunk
}

async fn assert_split_release_keeps_child_order(name: &str, term: &str, image: &[u8]) {
    let mut fixture = AttachFixture::start(name, term).await;

    fixture.publish(&held_interval_chunk(image));
    // The interval is open and the image is now owned by the server. Waiting
    // on the clipboard write proves the chunk was fully dispatched before the
    // release is published, so the two never coalesce into one batch.
    fixture.reader.read_until(CLIPBOARD_MARKER).await;

    fixture.publish(b"\x1b[?2026l");
    let (final_at, image_at) = fixture.reader.ordered_positions(FINAL_TEXT, image).await;

    assert!(
        final_at < image_at,
        "child order text -> image must survive a synchronized interval released in a later \
         read: text at {final_at}, image at {image_at}"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn forward_attach_orders_a_split_release_before_its_kitty_image() {
    assert_split_release_keeps_child_order("sync-split-kitty", "xterm-kitty", KITTY_MARKER).await;
}

#[tokio::test]
async fn forward_attach_orders_a_split_release_before_its_sixel_image() {
    assert_split_release_keeps_child_order("sync-split-sixel", "foot", SIXEL_MARKER).await;
}

#[tokio::test]
async fn forward_attach_keeps_a_same_chunk_release_ordered() {
    let mut fixture = AttachFixture::start("sync-same-chunk", "xterm-kitty").await;

    // The interval has already ended when this batch is rendered, so nothing
    // is held. This path was correct before the correction and must stay so.
    let mut chunk = held_interval_chunk(KITTY_MARKER);
    chunk.extend_from_slice(b"\x1b[?2026l");
    fixture.publish(&chunk);

    let (final_at, image_at) = fixture
        .reader
        .ordered_positions(FINAL_TEXT, KITTY_MARKER)
        .await;

    assert!(
        final_at < image_at,
        "a release arriving in the same read must stay ordered: text at {final_at}, image at \
         {image_at}"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn forward_attach_orders_an_unclosed_interval_after_its_fail_open_deadline() {
    let mut fixture = AttachFixture::start("sync-timeout", "xterm-kitty").await;

    // The child never sends `?2026l`. The bounded synchronized-output timer
    // converges the screen, and the held image must follow that render.
    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    fixture.reader.read_until(CLIPBOARD_MARKER).await;

    let (final_at, image_at) = fixture
        .reader
        .ordered_positions(FINAL_TEXT, KITTY_MARKER)
        .await;

    assert!(
        final_at < image_at,
        "the fail-open deadline must publish the converged screen before the held image: text \
         at {final_at}, image at {image_at}"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn forward_attach_orders_the_exit_drain_after_eof_releases_the_interval() {
    let mut fixture = AttachFixture::start("sync-eof", "xterm-kitty").await;

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    fixture.reader.read_until(CLIPBOARD_MARKER).await;

    // EOF releases the unclosed interval under the generation-guarded output
    // boundary, so the exit drain renders converged before it flushes.
    publish_pane_eof_for_test(&fixture.transcript, &fixture.pane_output);
    fixture
        .control_tx
        .send(AttachControl::Exited)
        .expect("send exited control");

    fixture.reader.read_until(b"[exited]\r\n").await;
    let final_at = fixture
        .reader
        .position(FINAL_TEXT)
        .expect("the exit drain must publish the converged screen");
    let image_at = fixture
        .reader
        .position(KITTY_MARKER)
        .expect("the exit drain must not discard the held image");
    let banner_at = fixture
        .reader
        .position(b"[exited]\r\n")
        .expect("exit banner was awaited");

    assert!(
        final_at < image_at,
        "text at {final_at} must precede the image at {image_at} in the exit drain"
    );
    assert!(image_at < banner_at, "the image must precede the banner");
    assert!(fixture.attach_task.await.expect("attach task join").is_ok());
}

/// The coalescing control dispatcher owns its own switch. It drains the
/// passthroughs the old receiver still held and emits them after the
/// replacement frame, so it must make the same hold decision as every other
/// dispatch site rather than rendering them directly.
#[tokio::test]
async fn pending_control_switch_holds_a_positioned_image_until_the_interval_ends() {
    let mut fixture = AttachFixture::start("sync-control-switch", "xterm-kitty").await;

    // Nothing awaits between these three, so the loop is still parked when all
    // of them are queued: the select's control arm takes the write, and the
    // switch is left for `apply_pending_attach_controls` at the top of the
    // next iteration — the production coalescing path.
    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    fixture.send(AttachControl::Write(b"CONTROL-BARRIER".to_vec()));
    let next_target = fixture.switch_target("sync-control-switch", "xterm-kitty");
    fixture.send(AttachControl::switch(next_target));

    // The switch emitted the passthroughs the old receiver held: the clipboard
    // write left immediately, so reaching it proves that batch was dispatched.
    fixture.reader.read_until(CLIPBOARD_MARKER).await;
    assert!(
        !fixture.reader.contains(KITTY_MARKER),
        "a pending-control switch emitted the positioned Kitty payload against the frozen frame"
    );

    fixture.publish(b"\x1b[?2026l");
    let (final_at, image_at) = fixture
        .reader
        .ordered_positions(FINAL_TEXT, KITTY_MARKER)
        .await;
    assert!(
        final_at < image_at,
        "the switch's held image must follow the converged render: text at {final_at}, image at \
         {image_at}"
    );
    fixture.shutdown().await;
}

/// A switch that retains positioned work must arm the deadline that releases
/// it. The synchronized-output timer never wakes a live attach receiver, so
/// without that re-arm the image waits for the child to speak again — and here
/// the child never does.
#[tokio::test]
async fn switch_retained_image_wakes_without_further_child_output() {
    let mut fixture = AttachFixture::start("sync-switch-wake", "xterm-kitty").await;

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    let next_target = fixture.switch_target("sync-switch-wake", "xterm-kitty");
    fixture.send(AttachControl::switch(next_target));

    fixture.reader.read_until(CLIPBOARD_MARKER).await;
    assert!(
        !fixture.reader.contains(KITTY_MARKER),
        "the switch published a frozen frame, so the image must still be held"
    );

    // No further publish, no control, no client input: only the bounded
    // fail-open deadline and the refresh this switch had to re-arm.
    let (final_at, image_at) = fixture
        .reader
        .ordered_positions(FINAL_TEXT, KITTY_MARKER)
        .await;
    assert!(
        final_at < image_at,
        "the re-armed refresh must publish the converged screen before the held image: text at \
         {final_at}, image at {image_at}"
    );
    fixture.shutdown().await;
}

/// The target builder captures the pane body for `render_frame` and constructs
/// the live renderer separately. A release landing between those two reads must
/// not make the switch report a converged frame while it emits the frozen one.
#[test]
fn a_switch_frame_carries_the_state_of_the_capture_it_was_built_from() {
    let session_name = SessionName::new("sync-switch-frame").expect("valid session name");
    let session = crate::session_fixture::SessionFixture::new(session_name.clone(), PANE_SIZE);
    let pane = session
        .window()
        .active_pane()
        .expect("session has an active pane")
        .clone();
    let transcript = pane_transcript_for(session.view());
    let pane_output = pane_output_channel();

    // 1. An interval is open, and the builder reads the pane body for the
    //    replacement frame while the presentation is frozen.
    append(&transcript, b"\x1b[?2026h\x1b[2J\x1b[H");
    append(&transcript, FINAL_TEXT);
    let capture = transcript.lock().capture_presentation_screen();
    assert!(
        capture.presentation().is_frozen(),
        "the fixture must capture the replacement frame inside the interval"
    );
    let render_frame =
        render_pane_screen(session.view(), &OptionStore::new(), &pane, capture.screen());

    // 2. The child converges before the rest of the target is built.
    append(&transcript, b"\x1b[?2026l");

    // 3. Construction happens against a newer transcript, but the retained
    //    capture still seeds the renderer beside the older frame.
    let live_pane = LivePaneRender::new_from_presentation_capture(
        transcript.clone(),
        session.view(),
        &OptionStore::new(),
        &pane,
        capture,
    )
    .expect("switch target has a live render snapshot");
    let (pane_output_start_sequence, subscribed_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name,
        pane_master: None,
        pane_output: subscribed_output,
        pane_output_start_sequence,
        render_frame,
        outer_terminal: OuterTerminal::resolve(
            &clipboard_options(),
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
        ),
        client_title: None,
        cursor_style: 0.into(),
        active_pane_geometry: PaneGeometry::new(0, 0, PANE_SIZE.cols, PANE_SIZE.rows),
        raw_passthrough: true,
        kitty_graphics_passthrough: true,
        sixel_passthrough: true,
        persistent_overlay_state_id: None,
        live_pane: Some(live_pane),
    };

    let mut open_target = open_attach_target(target, false).expect("open switch target");
    assert!(
        open_target.presentation_frozen(),
        "the emitted frame came from the frozen presentation, so a positioned passthrough must \
         still be held"
    );

    // A render of its own is what moves the answer: from then on the client is
    // looking at the converged screen this renderer just published.
    let delta = open_target
        .live_pane
        .as_mut()
        .expect("switch target keeps a live renderer")
        .render_frame_from_transcript(true);
    assert!(
        matches!(delta, PaneRenderDelta::Incremental(_)),
        "{delta:?}"
    );
    assert!(
        !open_target.presentation_frozen(),
        "a converged render must release what the interval was holding"
    );
}

/// A respawn keeps the pane's `PaneOutputSender` allocation and only fences its
/// generation, so the refresh that follows is a same-source plain render
/// refresh: `shares_pane_source_with` reports the very shape a switch between
/// two live views of one pane has. An image the dead process queued while its
/// interval was open must not survive that boundary — it would be anchored on
/// the replacement process's frame, which is exactly the `OLD -> image` order
/// this queue exists to prevent, with the wrong process's pixels.
#[tokio::test]
async fn a_generation_change_discards_an_old_positioned_passthrough() {
    let mut fixture = AttachFixture::start("sync-respawn-generation", "xterm-kitty").await;

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    // The clipboard write is not position dependent, so reaching it proves the
    // whole chunk was dispatched while the image was still being held.
    fixture.reader.read_until(CLIPBOARD_MARKER).await;
    assert!(
        !fixture.reader.contains(KITTY_MARKER),
        "the image must still be held while the interval is open"
    );

    // The production respawn boundary: the retained sender is fenced to the
    // replacement process's generation
    // (`pane_terminals/pane_outputs/spawn.rs` -> `reset_generation`).
    fixture.pane_output.reset_generation(1);

    let next_target = fixture.respawn_refresh_target("sync-respawn-generation", "xterm-kitty");
    fixture.send(AttachControl::switch(next_target));
    // FIFO barrier: this write is queued behind the switch, so observing it
    // proves the switch's queue decision has fully dispatched.
    fixture.send(AttachControl::Write(b"RESPAWN-BARRIER".to_vec()));
    fixture.reader.read_until(b"RESPAWN-BARRIER").await;

    let image_at = fixture.reader.position(KITTY_MARKER);
    let replacement_at = fixture.reader.position(NEW_PROCESS_TEXT);
    assert!(
        replacement_at.is_some(),
        "the replacement process's frame must reach the client"
    );
    assert!(
        image_at.is_none(),
        "an old-generation positioned payload was drawn on the new process's frame: image at \
         {image_at:?}, replacement frame at {replacement_at:?}"
    );
    fixture.shutdown().await;
}

/// A pane-output batch owns the generation under which the ring yielded it,
/// even when attach-control awaits delay queueing or dispatch until after a
/// retained-sender respawn. Otherwise the old image is mislabeled as new and
/// drawn over the replacement process's frame.
#[tokio::test]
async fn a_drained_batch_keeps_its_generation_across_a_respawn() {
    let name = "sync-drain-generation";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    let pause = crate::pane_io::install_pane_output_batch_drain_pause(
        SessionName::new(name).expect("valid session name"),
    );

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    pause.reached.notified().await;

    fixture.pane_output.reset_generation(1);
    fixture.send(AttachControl::switch(
        fixture.respawn_refresh_target(name, "xterm-kitty"),
    ));
    pause.release.notify_one();

    fixture.publish(GENERATION_DRAIN_PROBE);
    fixture.reader.read_until(GENERATION_DRAIN_PROBE).await;

    let replacement_at = fixture
        .reader
        .position(NEW_PROCESS_TEXT)
        .expect("the replacement process's frame must reach the client");
    let image_at = fixture.reader.position(KITTY_MARKER);
    assert!(
        image_at.is_none(),
        "a batch born under the dead generation was drawn on the replacement frame: image at \
         {image_at:?}, replacement frame at {replacement_at}"
    );
    fixture.shutdown().await;
}

/// Pane-output generation counters are meaningful only within one sender
/// allocation. When a pending cross-pane switch is followed by a refresh, an
/// already-drained image from pane A must be invalidated before pane B's equal
/// counter can make it look current.
#[tokio::test]
async fn cross_pane_switch_then_refresh_drops_drained_positioned_passthrough_with_equal_generations(
) {
    let name = "sync-cross-pane-refresh-a";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    let pause = crate::pane_io::install_pane_output_batch_drain_pause(
        SessionName::new(name).expect("valid session name"),
    );

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    pause.reached.notified().await;

    let foreign_name =
        SessionName::new("sync-cross-pane-refresh-b").expect("valid foreign session name");
    let foreign_session =
        crate::session_fixture::SessionFixture::new(foreign_name.clone(), PANE_SIZE);
    let foreign_transcript = pane_transcript_for(foreign_session.view());
    append(&foreign_transcript, FOREIGN_PANE_TEXT);
    let foreign_pane_output = pane_output_channel();
    assert_eq!(fixture.pane_output.current_generation(), 0);
    assert_eq!(
        fixture.pane_output.current_generation(),
        foreign_pane_output.current_generation(),
        "the regression requires equal counters from distinct pane-output allocations"
    );
    let foreign_target = same_source_switch_target(
        &foreign_name,
        &foreign_pane_output,
        &foreign_transcript,
        "xterm-kitty",
    );
    fixture.send(AttachControl::switch(foreign_target));
    fixture.send(AttachControl::Refresh);
    pause.release.notify_one();

    publish_pane_bytes_for_test(
        &foreign_transcript,
        &foreign_pane_output,
        FOREIGN_PANE_REFRESH_PROBE.to_vec(),
    );
    fixture.reader.read_until(FOREIGN_PANE_REFRESH_PROBE).await;
    // This side effect shares pane A's drained batch with the image. It is
    // safe across a source change and positively proves that the batch was
    // handled; absence of the image alone is not treated as completion.
    fixture.reader.read_until(CLIPBOARD_MARKER).await;

    let foreign_frame_at = fixture
        .reader
        .position(FOREIGN_PANE_TEXT)
        .expect("pane B's replacement frame must reach the client");
    let foreign_refresh_at = fixture
        .reader
        .position(FOREIGN_PANE_REFRESH_PROBE)
        .expect("pane B's scheduled refresh must reach the client");
    let image_at = fixture.reader.position(KITTY_MARKER);
    assert!(
        image_at.is_none(),
        "pane A's positioned payload was drawn on pane B: pane B frame at {foreign_frame_at}, \
         refreshed frame at {foreign_refresh_at}, pane A image at {image_at:?}"
    );
    fixture.shutdown().await;
}

/// `target_changed` is true for every handled switch, including a refresh of
/// the same pane source. Such a switch must keep the already-drained image and
/// release it only after the synchronized presentation converges.
#[tokio::test]
async fn same_source_switch_then_refresh_retains_drained_positioned_passthrough_until_refreshed_frame(
) {
    let name = "sync-same-source-refresh";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    let pause = crate::pane_io::install_pane_output_batch_drain_pause(
        SessionName::new(name).expect("valid session name"),
    );

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    pause.reached.notified().await;

    fixture.send(AttachControl::switch(
        fixture.switch_target(name, "xterm-kitty"),
    ));
    fixture.send(AttachControl::Refresh);
    pause.release.notify_one();

    // The explicit refresh is serviced through the test fixture's no-op
    // handler, so wake the replacement receiver while the presentation is
    // still frozen. The non-positioned side effect proves dispatch completed.
    fixture.publish(SAME_SOURCE_PENDING_PROBE);
    fixture.reader.read_until(CLIPBOARD_MARKER).await;
    assert!(
        !fixture.reader.contains(KITTY_MARKER),
        "the same-source image must remain held against a frozen refresh frame"
    );

    fixture.publish(b"\x1b[?2026l");
    let (final_at, image_at) = fixture
        .reader
        .ordered_positions(FINAL_TEXT, KITTY_MARKER)
        .await;
    assert!(
        final_at < image_at,
        "the valid refreshed frame must precede the retained image: frame at {final_at}, image \
         at {image_at}"
    );
    fixture.shutdown().await;
}

/// A positioned payload can already belong to the deferred queue before a
/// same-source switch is applied. The Refresh that follows the switch must not
/// treat the new view of the same pane-output source as an ownership change.
#[tokio::test]
async fn same_source_switch_then_refresh_retains_queue_resident_positioned_passthrough() {
    let name = "sync-same-source-queued-refresh";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    let queued_generation = fixture.pane_output.current_generation();

    fixture.publish(&held_interval_chunk(KITTY_MARKER));
    // The clipboard side effect leaves immediately. Observing it while the
    // image is absent proves the image is already resident in the deferred
    // queue, rather than still waiting in the pane-output ring.
    fixture.reader.read_until(CLIPBOARD_MARKER).await;
    assert!(
        !fixture.reader.contains(KITTY_MARKER),
        "the positioned payload must be queue-resident while the interval is frozen"
    );

    // FIFO forces the leading write through the control path before the
    // Switch. Switch + Refresh are then applied together by a prologue control
    // drain, and the trailing write proves that Refresh arm completed.
    fixture.send(AttachControl::Write(b"QUEUE-SEAM".to_vec()));
    fixture.send(AttachControl::switch(
        fixture.switch_target(name, "xterm-kitty"),
    ));
    fixture.send(AttachControl::Refresh);
    fixture.send(AttachControl::Write(b"QUEUE-REFRESH-APPLIED".to_vec()));
    fixture.reader.read_until(b"QUEUE-REFRESH-APPLIED").await;
    assert!(
        !fixture.reader.contains(KITTY_MARKER),
        "the same-source refresh must keep the queued image held against the frozen frame"
    );
    assert_eq!(
        fixture.pane_output.current_generation(),
        queued_generation,
        "a same-source refresh must retain the queue's generation owner"
    );

    fixture.publish(b"\x1b[?2026l");
    let (refreshed_frame_at, image_at) = fixture
        .reader
        .ordered_positions(FINAL_TEXT, KITTY_MARKER)
        .await;
    assert!(
        refreshed_frame_at < image_at,
        "the converged refreshed frame must precede its queue-resident image: frame at \
         {refreshed_frame_at}, image at {image_at}"
    );
    assert_eq!(fixture.pane_output.current_generation(), queued_generation);
    fixture.shutdown().await;
}

/// An EOF belongs to the pane-output batch that carried it. A cross-pane
/// switch processed before that batch's refresh must not apply pane A's close
/// bit to pane B's newly installed receiver.
#[tokio::test]
async fn cross_pane_switch_then_refresh_does_not_close_replacement_pane_output() {
    let name = "sync-cross-pane-close-a";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    let pause = crate::pane_io::install_pane_output_batch_drain_pause(
        SessionName::new(name).expect("valid session name"),
    );

    let mut source_batch = CLOSE_OWNER_TEXT.to_vec();
    source_batch.extend_from_slice(CLIPBOARD_MARKER);
    fixture.publish(&source_batch);
    publish_pane_eof_for_test(&fixture.transcript, &fixture.pane_output);
    pause.reached.notified().await;

    let foreign_name =
        SessionName::new("sync-cross-pane-close-b").expect("valid foreign session name");
    let foreign_session =
        crate::session_fixture::SessionFixture::new(foreign_name.clone(), PANE_SIZE);
    let foreign_transcript = pane_transcript_for(foreign_session.view());
    append(&foreign_transcript, FOREIGN_PANE_TEXT);
    let foreign_pane_output = pane_output_channel();
    let foreign_target = same_source_switch_target(
        &foreign_name,
        &foreign_pane_output,
        &foreign_transcript,
        "xterm-kitty",
    );
    fixture.send(AttachControl::switch(foreign_target));
    fixture.send(AttachControl::Refresh);
    pause.release.notify_one();

    // The replacement frame is a positive barrier proving the switch ran
    // before pane B publishes output of its own.
    fixture.reader.read_until(FOREIGN_PANE_TEXT).await;
    publish_pane_bytes_for_test(
        &foreign_transcript,
        &foreign_pane_output,
        FOREIGN_PANE_LIVE_CLIPBOARD.to_vec(),
    );
    fixture.reader.read_until(FOREIGN_PANE_LIVE_CLIPBOARD).await;
    assert_eq!(foreign_pane_output.receiver_count_for_test(), 1);
    fixture.shutdown().await;
}

/// The ownership guard must remain narrow: without a source-invalidating
/// switch, a genuine EOF still closes the pane's live output after refresh.
#[tokio::test]
async fn refresh_after_eof_still_closes_the_same_pane_output() {
    let name = "sync-same-pane-close";
    let mut fixture = AttachFixture::start(name, "xterm-kitty").await;
    let pause = crate::pane_io::install_pane_output_batch_drain_pause(
        SessionName::new(name).expect("valid session name"),
    );

    let mut source_batch = CLOSE_OWNER_TEXT.to_vec();
    source_batch.extend_from_slice(CLIPBOARD_MARKER);
    fixture.publish(&source_batch);
    publish_pane_eof_for_test(&fixture.transcript, &fixture.pane_output);
    pause.reached.notified().await;
    fixture.send(AttachControl::Refresh);
    fixture.send(AttachControl::Write(CLOSE_APPLIED_BARRIER.to_vec()));
    pause.release.notify_one();

    fixture.reader.read_until(CLOSE_APPLIED_BARRIER).await;
    wait_for_receiver_count(&fixture.pane_output, 0).await;
    fixture.shutdown().await;
}

/// Reproduces the attach-builder window in which the delivered frame is read
/// while DEC 2026 is frozen but the live renderer is constructed after the
/// release. The target returned here therefore has a fresh private baseline
/// and an older delivered frame on the uncorrected production code.
fn target_built_across_synchronized_release(name: &str) -> OpenAttachTarget {
    let session_name = SessionName::new(name).expect("valid session name");
    let session = crate::session_fixture::SessionFixture::new(session_name.clone(), PANE_SIZE);
    let pane = session
        .window()
        .active_pane()
        .expect("session has an active pane")
        .clone();
    let transcript = pane_transcript_for(session.view());

    append(&transcript, b"\x1b[?2026h\x1b[2J\x1b[H");
    append(&transcript, FINAL_TEXT);
    let capture = transcript.lock().capture_presentation_screen();
    assert!(capture.presentation().is_frozen());
    let render_frame =
        render_pane_screen(session.view(), &OptionStore::new(), &pane, capture.screen());
    assert!(
        !render_frame
            .windows(FINAL_TEXT.len())
            .any(|window| window == FINAL_TEXT),
        "the frame handed to the client must still show the pre-interval screen"
    );

    append(&transcript, b"\x1b[?2026l");
    let live_pane = LivePaneRender::new_from_presentation_capture(
        transcript,
        session.view(),
        &OptionStore::new(),
        &pane,
        capture,
    )
    .expect("switch target has a live render snapshot");
    let pane_output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    open_attach_target(
        AttachTarget {
            session_name,
            pane_master: None,
            pane_output,
            pane_output_start_sequence,
            render_frame,
            outer_terminal: OuterTerminal::resolve(
                &clipboard_options(),
                OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
            ),
            client_title: None,
            cursor_style: 0.into(),
            active_pane_geometry: PaneGeometry::new(0, 0, PANE_SIZE.cols, PANE_SIZE.rows),
            raw_passthrough: true,
            kitty_graphics_passthrough: true,
            sixel_passthrough: true,
            persistent_overlay_state_id: None,
            live_pane: Some(live_pane),
        },
        false,
    )
    .expect("open switch target")
}

#[test]
fn a_nonreplaceable_render_publishes_the_frame_missing_from_its_fresh_baseline() {
    let mut target = target_built_across_synchronized_release("sync-nonreplaceable");
    assert!(
        target.presentation_frozen(),
        "the emitted frame came from the frozen presentation"
    );

    match target
        .live_pane
        .as_mut()
        .expect("switch target keeps a live renderer")
        .render_frame_from_transcript(false)
    {
        PaneRenderDelta::RequiresFullRefresh => {}
        PaneRenderDelta::Incremental(delta) => {
            let publishes_final = delta
                .frame()
                .windows(FINAL_TEXT.len())
                .any(|window| window == FINAL_TEXT);
            assert!(
                publishes_final || target.presentation_frozen(),
                "the first non-replaceable render published {} bytes without the converged text, \
                 yet reports the client's frame as converged; the client never receives FINALTEXT \
                 (delta: {:?})",
                delta.frame().len(),
                String::from_utf8_lossy(delta.frame())
            );
        }
    }
}

#[test]
fn a_render_that_publishes_nothing_must_not_release_a_held_positioned_payload() {
    let mut target = target_built_across_synchronized_release("sync-empty-release");
    let mut deferred = DeferredPassthroughQueue::default();
    let mut pane_refresh = AttachRefreshScheduler::default();
    // The payload is born under the target's own generation, so the respawn
    // fence retains it and the only thing left to decide is whether the frame
    // the client received was frozen.
    let birth_generation = target.pane_output_generation();
    let held = take_passthrough_frame_for_client(
        &target,
        &mut deferred,
        &mut pane_refresh,
        birth_generation,
        vec![TerminalPassthrough::kitty_graphics(
            0,
            2,
            b"Gf=100;SU1H".to_vec(),
        )],
    );
    assert!(held.is_empty(), "held while frozen, got {held:?}");
    assert!(!deferred.is_empty());

    let delta = target
        .live_pane
        .as_mut()
        .expect("switch target keeps a live renderer")
        .render_frame_from_transcript(false);
    let published = match &delta {
        PaneRenderDelta::Incremental(delta) => delta.frame().to_vec(),
        PaneRenderDelta::RequiresFullRefresh => FINAL_TEXT.to_vec(),
    };
    let released = take_passthrough_frame_for_client(
        &target,
        &mut deferred,
        &mut pane_refresh,
        birth_generation,
        Vec::new(),
    );
    if !released.is_empty() {
        assert!(
            published
                .windows(FINAL_TEXT.len())
                .any(|window| window == FINAL_TEXT),
            "the dispatcher released the positioned payload ({} bytes) after a render that \
             published only {:?}; the image is anchored on a client frame that still shows the \
             pre-interval screen",
            released.len(),
            String::from_utf8_lossy(&published)
        );
    }
}
