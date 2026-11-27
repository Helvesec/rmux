//! A server shutdown ends every attach stream like tmux's `MSG_SHUTDOWN`.

use super::*;
use crate::pane_io::{forward_attach_with_opt_ins, AttachStreamOptIns};

const SERVER_EXITED_BANNER: &[u8] = b"[server exited]\r\n";

/// Reads the attach stream to EOF: concatenated output bytes, and every
/// non-output message with the output length already received before it.
async fn read_attach_stream_to_eof(
    peer: &mut tokio::net::UnixStream,
) -> (Vec<u8>, Vec<(usize, AttachMessage)>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut output = Vec::new();
        let mut messages = Vec::new();
        let mut decoder = AttachFrameDecoder::new();
        let mut frame_bytes = [0_u8; 4096];
        loop {
            let bytes_read = peer.read(&mut frame_bytes).await.expect("read peer bytes");
            if bytes_read == 0 {
                assert!(decoder.is_empty(), "attach stream ended inside a frame");
                return (output, messages);
            }
            decoder.push_bytes(&frame_bytes[..bytes_read]);
            while let Some(message) = decoder.next_message().expect("decode attach frame") {
                match message {
                    AttachMessage::Data(bytes) | AttachMessage::Render(bytes) => {
                        output.extend_from_slice(&bytes);
                    }
                    other => messages.push((output.len(), other)),
                }
            }
        }
    })
    .await
    .expect("attach stream did not reach EOF after shutdown")
}

async fn shut_down_attached_client(
    opt_ins: AttachStreamOptIns,
) -> (Vec<u8>, Vec<(usize, AttachMessage)>) {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach_with_opt_ins(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        live_input,
        opt_ins,
    ));
    let _ = read_attach_data_until(&mut peer, b"BASE-0").await;

    shutdown_tx.send(()).expect("request attach shutdown");
    let ended = read_attach_stream_to_eof(&mut peer).await;
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "shutdown must end the attach cleanly: {result:?}"
    );
    ended
}

#[tokio::test]
async fn server_shutdown_prints_the_server_exited_banner_after_the_attach_stop() {
    let (output, messages) = shut_down_attached_client(AttachStreamOptIns {
        render: false,
        server_exited: false,
    })
    .await;

    assert_attach_stop_restores_cursor_before_banner(&output, SERVER_EXITED_BANNER);
    assert!(
        output.ends_with(SERVER_EXITED_BANNER),
        "the banner must be the last output of a shut-down attach: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        messages.is_empty(),
        "a client that did not opt in must never receive the typed notice: {messages:?}"
    );
}

#[tokio::test]
async fn server_shutdown_sends_the_opted_in_notice_after_the_banner() {
    for render in [false, true] {
        let (output, messages) = shut_down_attached_client(AttachStreamOptIns {
            render,
            server_exited: true,
        })
        .await;

        assert_attach_stop_restores_cursor_before_banner(&output, SERVER_EXITED_BANNER);
        assert!(output.ends_with(SERVER_EXITED_BANNER));
        assert_eq!(
            messages,
            vec![(output.len(), AttachMessage::ServerExited)],
            "render={render}: exactly one notice, after every output byte"
        );
    }
}

#[tokio::test]
async fn opted_in_detach_keeps_its_banner_and_never_reports_a_server_exit() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let live_input = LiveAttachInputContext::unregistered_for_test(handler, std::process::id());

    let attach_task = tokio::spawn(forward_attach_with_opt_ins(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        live_input,
        AttachStreamOptIns {
            render: false,
            server_exited: true,
        },
    ));
    let _ = read_attach_data_until(&mut peer, b"BASE-0").await;

    control_tx
        .send(AttachControl::Detach)
        .expect("send detach control");
    let (output, messages) = read_attach_stream_to_eof(&mut peer).await;
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "detach must end the attach cleanly: {result:?}"
    );

    assert!(
        output.ends_with(b"[detached (from session alpha)]\r\n"),
        "detach keeps its own banner: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        !output
            .windows(SERVER_EXITED_BANNER.len())
            .any(|window| window == SERVER_EXITED_BANNER),
        "a detach is not a server exit"
    );
    assert!(
        messages.is_empty(),
        "a detach sends no typed notice: {messages:?}"
    );
}
