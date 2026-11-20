use super::*;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

// The first poll is the opportunistic read. For the select case it must
// return Pending; the following awaited read observes EOF. No sleeps or
// scheduler timing select which production exit is exercised.
struct EofStream {
    select_exit: bool,
    reads: Arc<AtomicUsize>,
    output: Arc<Mutex<Vec<u8>>>,
}

impl AsyncRead for EofStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        if self.select_exit && read == 0 {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

impl AsyncWrite for EofStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.output.lock().unwrap().extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn decoded_input_before_eof(select_exit: bool) {
    let cases = [
        ("data", vec![AttachMessage::Data(b"y\r".to_vec())]),
        (
            "key",
            vec![AttachMessage::Keystroke(AttachedKeystroke::new(
                b"y\r".to_vec(),
            ))],
        ),
        (
            "coalesced",
            vec![
                AttachMessage::Data(vec![b'y']),
                AttachMessage::Data(vec![b'\r']),
            ],
        ),
    ];
    let mut observed = Vec::new();
    for (index, (label, messages)) in cases.into_iter().enumerate() {
        let handler = Arc::new(RequestHandler::new());
        let pane = create_attach_input_test_session(
            &handler,
            &format!(
                "eof-{}-{label}",
                if select_exit { "select" } else { "immediate" }
            ),
        )
        .await;
        let attach_pid = 914_000 + index as u32 + u32::from(select_exit) * 10;
        // Keep refresh notifications on their registered channel. The
        // forwarder's empty control queue isolates socket EOF from repaint.
        let (registered_tx, _registered_rx) = mpsc::unbounded_channel();
        handler
            .register_attach(attach_pid, pane.session_name().clone(), registered_tx)
            .await;
        let live_input =
            LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
        let (_control_tx, control_rx) = mpsc::unbounded_channel();
        let (_shutdown_tx, shutdown_rx) = watch::channel(());
        let output = Arc::new(Mutex::new(Vec::new()));
        let reads = Arc::new(AtomicUsize::new(0));
        let stream = AttachTransport::from_io(EofStream {
            select_exit,
            reads: Arc::clone(&reads),
            output: Arc::clone(&output),
        });
        let target = test_attach_target(pane.session_name(), b"EOF-BASE", None);
        let stop = target.outer_terminal.attach_stop_sequence();
        let mut initial_bytes = Vec::new();
        if select_exit {
            // Unlock is a production inter-frame barrier: it leaves the
            // following complete input in the decoder before select reads EOF.
            initial_bytes.extend(encode_attach_message(&AttachMessage::Unlock).unwrap());
        }
        for message in messages {
            initial_bytes.extend(encode_attach_message(&message).unwrap());
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            forward_attach(
                stream,
                target,
                initial_bytes,
                shutdown_rx,
                control_rx,
                Arc::new(AtomicUsize::new(0)),
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU64::new(0)),
                live_input,
                false,
            ),
        )
        .await
        .expect("EOF must finish promptly")
        .expect("EOF must finish cleanly");
        assert_eq!(
            reads.load(Ordering::SeqCst),
            if select_exit { 2 } else { 1 },
            "production EOF exit"
        );
        assert!(
            output.lock().unwrap().ends_with(&stop),
            "EOF must emit the attach stop sequence"
        );
        observed.push((
            label,
            handler
                .attached_input_capture_for_test(&pane)
                .await
                .unwrap(),
        ));
    }
    assert_eq!(
        observed,
        vec![
            ("data", b"y\r".to_vec()),
            ("key", b"y\r".to_vec()),
            ("coalesced", b"y\r".to_vec())
        ],
        "all complete input must reach the pane before EOF stop"
    );
}

#[tokio::test]
async fn attach_half_close_immediate_eof_delivers_decoded_input() {
    decoded_input_before_eof(false).await;
}

#[tokio::test]
async fn attach_half_close_select_eof_delivers_decoded_input() {
    decoded_input_before_eof(true).await;
}

async fn osc_suffix_before_eof(select_exit: bool) {
    let handler = Arc::new(RequestHandler::new());
    let pane = create_attach_input_test_session(
        &handler,
        if select_exit {
            "osc-eof-select"
        } else {
            "osc-eof-immediate"
        },
    )
    .await;
    let attach_pid = 916_000 + u32::from(select_exit);
    let (registered_tx, _registered_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, pane.session_name().clone(), registered_tx)
        .await;
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let output = Arc::new(Mutex::new(Vec::new()));
    let reads = Arc::new(AtomicUsize::new(0));
    let stream = AttachTransport::from_io(EofStream {
        select_exit,
        reads: Arc::clone(&reads),
        output: Arc::clone(&output),
    });
    let target = test_attach_target(pane.session_name(), b"OSC-EOF-BASE", None);
    let stop = target.outer_terminal.attach_stop_sequence();
    let mut initial_bytes = Vec::new();
    if select_exit {
        initial_bytes.extend(encode_attach_message(&AttachMessage::Unlock).unwrap());
    }
    let input = b"\x1b]52;c;x\x1b";
    initial_bytes.extend(encode_attach_message(&AttachMessage::Data(input.to_vec())).unwrap());
    tokio::time::timeout(
        Duration::from_secs(2),
        forward_attach(
            stream,
            target,
            initial_bytes,
            shutdown_rx,
            control_rx,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            live_input,
            false,
        ),
    )
    .await
    .expect("EOF suffix drain must finish promptly")
    .expect("EOF suffix drain must finish cleanly");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        if select_exit { 2 } else { 1 }
    );
    assert!(
        output.lock().unwrap().ends_with(&stop),
        "EOF must emit stop"
    );
    // D72: every admitted decoded byte, including a suffix retained by the
    // OSC flush, reaches the pane before the attach stop sequence.
    assert_eq!(
        handler
            .attached_input_capture_for_test(&pane)
            .await
            .unwrap(),
        input,
        "EOF must drain the ESC retained when an incomplete OSC is flushed"
    );
}

#[tokio::test]
async fn attach_eof_immediate_drains_retained_osc_suffix() {
    osc_suffix_before_eof(false).await;
}

#[tokio::test]
async fn attach_eof_select_drains_retained_osc_suffix() {
    osc_suffix_before_eof(true).await;
}

async fn eof_tail_fixture(
    admission: &str,
    messages: Vec<AttachMessage>,
    partial_tail: bool,
) -> (std::io::Result<()>, Vec<u8>) {
    let handler = Arc::new(RequestHandler::new());
    let pane = create_attach_input_test_session(&handler, &format!("eof-tail-{admission}")).await;
    let attach_pid = 915_000;
    let (registered_tx, _registered_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(
            attach_pid,
            pane.session_name().clone(),
            registered_tx.clone(),
        )
        .await;
    let live_input =
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await;
    if admission == "closed" {
        handler.close_normal_request_admission();
    } else if admission == "stale" {
        handler
            .register_attach(attach_pid, pane.session_name().clone(), registered_tx)
            .await;
    }
    let (stream, mut peer) = tokio::net::UnixStream::pair().unwrap();
    let stream = AttachTransport::from(stream);
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let mut target = open_attach_target(
        test_attach_target(pane.session_name(), b"BASE", None),
        false,
    )
    .unwrap();
    let stop = target.outer_terminal.attach_stop_sequence();
    let mut decoder = AttachFrameDecoder::new();
    for message in messages {
        decoder.push_bytes(&encode_attach_message(&message).unwrap());
    }
    if partial_tail {
        let frame = encode_attach_message(&AttachMessage::Data(b"uncommitted".to_vec())).unwrap();
        decoder.push_bytes(&frame[..3]);
    }
    let mut pending_input = Vec::new();
    let mut active_emit_cache = None;
    let mut locked = false;
    let mut pending_escape_flush = PendingEscapeFlush::default();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        crate::pane_io::attach_eof::finish_attach_eof(
            &stream,
            &mut decoder,
            &live_input,
            &shutdown_rx,
            &AtomicBool::new(false),
            &mut target,
            &mut pending_input,
            &mut active_emit_cache,
            &mut locked,
            &mut pending_escape_flush,
        ),
    )
    .await
    .expect("EOF tail must finish promptly");
    drop(stream);
    let mut output = Vec::new();
    peer.read_to_end(&mut output).await.unwrap();
    assert!(
        output.ends_with(&stop),
        "stop must be emitted even when the decoded tail is rejected"
    );
    let captured = handler
        .attached_input_capture_for_test(&pane)
        .await
        .unwrap();
    (result, captured)
}

#[tokio::test]
async fn attach_eof_tail_crosses_unlock_barriers_and_flushes_escape_before_stop() {
    let (result, captured) = eof_tail_fixture(
        "open",
        vec![
            AttachMessage::Data(vec![b'a']),
            AttachMessage::Unlock,
            AttachMessage::Unlock,
            AttachMessage::Keystroke(AttachedKeystroke::new(vec![b'b'])),
            AttachMessage::Data(vec![b'\x1b']),
        ],
        true,
    )
    .await;
    result.unwrap();
    assert_eq!(
        captured, b"ab\x1b",
        "complete input survives barriers; partial wire input is never delivered"
    );
}

#[tokio::test]
async fn attach_eof_closed_shutdown_admission_rejects_decoded_input_and_still_stops() {
    let (result, captured) = eof_tail_fixture(
        "closed",
        vec![AttachMessage::Data(b"rejected".to_vec())],
        false,
    )
    .await;
    result.unwrap();
    assert!(captured.is_empty());
}

#[tokio::test]
async fn attach_eof_stale_identity_returns_input_error_and_still_stops() {
    let (result, captured) = eof_tail_fixture(
        "stale",
        vec![AttachMessage::Data(b"rejected".to_vec())],
        false,
    )
    .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("stale attach forwarder input"));
    assert!(captured.is_empty());
}

// GH#222 reproduction, verbatim from the issue: the complete frame and the
// peer half-close are both pending before forward_attach runs.
async fn run_preclosed_attach_input(
    name: &str,
    wire_bytes: &[u8],
) -> (std::io::Result<()>, Vec<u8>) {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = 910_030;
    let target = create_attach_input_test_session(&handler, name).await;
    let session_name = target.session_name().clone();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;

    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    // Both the complete frame and the EOF are pending before forward_attach runs.
    let (stream, mut peer) = tokio::io::duplex(4096);
    peer.write_all(wire_bytes)
        .await
        .expect("write final attach input");
    peer.shutdown()
        .await
        .expect("orderly-close the attach input half");
    let attach_task = tokio::spawn(forward_attach(
        AttachTransport::from_io(stream),
        test_attach_target(&session_name, b"BASE", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        LiveAttachInputContext::current_for_test(Arc::clone(&handler), attach_pid).await,
        false,
    ));

    let result = tokio::time::timeout(Duration::from_secs(2), attach_task)
        .await
        .expect("attach must stop after peer input EOF")
        .expect("attach task join");
    let captured = handler
        .attached_input_capture_for_test(&target)
        .await
        .expect("input capture remains installed");
    (result, captured)
}

#[tokio::test]
async fn complete_input_frame_is_dispatched_before_orderly_eof() {
    let input = b"y\r";
    let wire = encode_attach_message(&AttachMessage::Data(input.to_vec()))
        .expect("encode final attach input");
    let (result, captured) =
        run_preclosed_attach_input("complete-input-before-orderly-eof", &wire).await;
    result.expect("attach exits cleanly");
    assert_eq!(
        captured, input,
        "every complete frame read before EOF must mutate the pane"
    );
}

#[tokio::test]
async fn truncated_only_input_frame_before_orderly_eof_never_reaches_pane() {
    let wire = encode_attach_message(&AttachMessage::Data(b"uncommitted".to_vec()))
        .expect("encode attach input");
    let (result, captured) =
        run_preclosed_attach_input("truncated-input-before-orderly-eof", &wire[..3]).await;
    // A truncated final wire frame stays uncommitted and the close remains
    // orderly, matching finish_attach_eof's existing contract.
    result.expect("truncated tail is dropped, not reported");
    assert!(
        captured.is_empty(),
        "a partial wire frame must never mutate the pane"
    );
}

#[derive(Clone, Copy, Debug)]
enum DropExit {
    Shutdown,
    StartError,
    ReadError,
    CancelStart,
    CancelRead,
}

type DropObservation = (bool, usize, usize, usize);

struct DropObservedStream {
    exit: DropExit,
    controls: mpsc::UnboundedSender<AttachControl>,
    output: super::super::types::PaneOutputSender,
    backlog: Arc<AtomicUsize>,
    shutdown: watch::Sender<()>,
    observed: Arc<Mutex<Option<DropObservation>>>,
    polled: Arc<AtomicBool>,
}

impl Drop for DropObservedStream {
    fn drop(&mut self) {
        // Observe synchronously at transport destruction, before client EOF can
        // race with the remaining destructors on another executor thread.
        *self.observed.lock().unwrap() = Some((
            self.controls.is_closed(),
            self.output.receiver_count_for_test(),
            self.backlog.load(Ordering::Acquire),
            self.shutdown.receiver_count(),
        ));
    }
}

impl AsyncRead for DropObservedStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.polled.store(true, Ordering::Release);
        if matches!(self.exit, DropExit::ReadError) {
            Poll::Ready(Err(io::Error::other("drop-order read failure")))
        } else {
            Poll::Pending
        }
    }
}

impl AsyncWrite for DropObservedStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.exit {
            DropExit::StartError => Poll::Ready(Err(io::Error::other("drop-order write failure"))),
            DropExit::CancelStart => {
                self.polled.store(true, Ordering::Release);
                Poll::Pending
            }
            _ => Poll::Ready(Ok(bytes.len())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn forward_attach_preserves_parent_drop_order_on_every_exit() {
    for exit in [
        DropExit::Shutdown,
        DropExit::StartError,
        DropExit::ReadError,
        DropExit::CancelStart,
        DropExit::CancelRead,
    ] {
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        if matches!(exit, DropExit::Shutdown) {
            shutdown_tx.send(()).unwrap();
        }
        let output = pane_output_channel();
        let target = test_attach_target_with_output(
            &SessionName::new("drop-order").unwrap(),
            b"BASE",
            None,
            output.clone(),
            false,
        );
        let observed = Arc::new(Mutex::new(None));
        let polled = Arc::new(AtomicBool::new(false));
        let backlog = Arc::new(AtomicUsize::new(7));
        let stream = AttachTransport::from_io(DropObservedStream {
            exit,
            controls: control_tx,
            output,
            backlog: Arc::clone(&backlog),
            shutdown: shutdown_tx,
            observed: Arc::clone(&observed),
            polled: Arc::clone(&polled),
        });
        let mut forward = Box::pin(forward_attach(
            stream,
            target,
            Vec::new(),
            shutdown_rx,
            control_rx,
            Arc::clone(&backlog),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            LiveAttachInputContext::unregistered_for_test(Arc::new(RequestHandler::new()), 917_000),
            false,
        ));
        if matches!(exit, DropExit::CancelStart | DropExit::CancelRead) {
            std::future::poll_fn(|cx| {
                assert!(forward.as_mut().poll(cx).is_pending(), "{exit:?}");
                assert!(
                    polled.load(Ordering::Acquire),
                    "must reach the selected suspension"
                );
                Poll::Ready(())
            })
            .await;
        } else {
            let result = tokio::time::timeout(Duration::from_secs(2), &mut forward)
                .await
                .unwrap();
            assert_eq!(
                result.is_ok(),
                matches!(exit, DropExit::Shutdown),
                "{exit:?}"
            );
        }
        drop(forward);
        assert_eq!(*observed.lock().unwrap(), Some((true, 0, 7, 1)), "{exit:?}");
        assert_eq!(backlog.load(Ordering::Acquire), 0, "{exit:?}");
    }
}
