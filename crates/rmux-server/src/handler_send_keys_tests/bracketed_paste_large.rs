use super::*;

const LARGE_PASTE_TARGET_BYTES: usize = 64 * 1024;
const ONE_MEBIBYTE: usize = 1024 * 1024;
const CHUNK_PATTERN: &[usize] = &[1, 2, 4, 8, 3, 13, 89, 233, 1024, 7, 4096];

#[tokio::test]
async fn live_attach_large_bracketed_paste_survives_irregular_chunks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let input = large_bracketed_paste_bytes();
    let expected = bracketed_paste_body(&input);
    assert!(input.len() >= LARGE_PASTE_TARGET_BYTES);
    assert!(input.len() < DEFAULT_MAX_FRAME_LENGTH);

    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "live-attach-large-bracketed-paste",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    let mut offset = 0;
    for width in CHUNK_PATTERN.iter().copied().cycle() {
        if offset == input.len() {
            break;
        }

        let end = input.len().min(offset + width);
        handler
            .handle_attached_live_input(requester_pid, &mut pending_input, &input[offset..end])
            .await
            .expect("large bracketed paste chunk");
        offset = end;
    }
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, expected).await;
}

#[tokio::test]
async fn live_attach_one_mebibyte_bracketed_paste_has_bounded_work() {
    let handler = RequestHandler::new();
    let alpha = session_name("one-mebibyte-bracketed-paste");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let body = vec![b'p'; ONE_MEBIBYTE];
    let mut input = Vec::with_capacity(body.len() + 12);
    input.extend_from_slice(b"\x1b[200~");
    input.extend_from_slice(&body);
    input.extend_from_slice(b"\x1b[201~");
    let capture =
        RawPaneInputProbe::start(&handler, &alpha, "one-mebibyte-bracketed-paste", body.len())
            .await;

    let mut pending_input = Vec::new();
    tokio::time::timeout(
        if cfg!(windows) {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(2)
        },
        handler.handle_attached_live_input(requester_pid, &mut pending_input, &input),
    )
    .await
    .expect("one MiB bracketed paste must finish with bounded work")
    .expect("one MiB bracketed paste succeeds");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &body).await;
}

#[tokio::test]
async fn live_attach_over_limit_bracketed_mode_uses_bounded_envelopes_product_divergence() {
    let handler = RequestHandler::new();
    let alpha = session_name("over-limit-bracketed-mode");
    let requester_pid = std::process::id();

    create_quiet_input_session(&handler, &alpha).await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&alpha, 0, 0, b"\x1b[?2004h")
            .expect("pane enables bracketed paste mode");
    }
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

    let body = vec![b'p'; DEFAULT_MAX_FRAME_LENGTH + 64];
    let first_body_len = DEFAULT_MAX_FRAME_LENGTH - b"\x1b[200~".len() + 1;
    let mut expected = Vec::with_capacity(body.len() + 24);
    expected.extend_from_slice(b"\x1b[200~");
    expected.extend_from_slice(&body[..first_body_len]);
    expected.extend_from_slice(b"\x1b[201~");
    expected.extend_from_slice(b"\x1b[200~");
    expected.extend_from_slice(&body[first_body_len..]);
    expected.extend_from_slice(b"\x1b[201~");
    let capture = RawPaneInputProbe::start(
        &handler,
        &alpha,
        "over-limit-bracketed-mode",
        expected.len(),
    )
    .await;

    let mut pending_input = Vec::new();
    let mut first = b"\x1b[200~".to_vec();
    first.extend_from_slice(&body[..first_body_len]);
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &first)
        .await
        .expect("first bounded envelope forwards");
    assert_eq!(pending_input, b"\x1b[200~");

    let mut final_chunk = body[first_body_len..].to_vec();
    final_chunk.extend_from_slice(b"\x1b[201~");
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &final_chunk)
        .await
        .expect("final bounded envelope forwards");
    assert!(pending_input.is_empty());

    capture.finish(&handler, &alpha).await;
    capture.assert_contents(&handler, &expected).await;
}

fn bracketed_paste_body(bytes: &[u8]) -> &[u8] {
    &bytes[b"\x1b[200~".len()..bytes.len() - b"\x1b[201~".len()]
}

fn large_bracketed_paste_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(LARGE_PASTE_TARGET_BYTES + 1024);
    bytes.extend_from_slice(b"\x1b[200~");

    let mut line = 0;
    while bytes.len() < LARGE_PASTE_TARGET_BYTES {
        bytes.extend_from_slice(format!("line-{line:04}: ").as_bytes());
        bytes.extend_from_slice("ASCII | 東京 | 한글 | cafe\u{0301} | ".as_bytes());

        if line % 11 == 0 {
            bytes.extend_from_slice(b"\x02 prefix ");
        }
        if line % 17 == 0 {
            bytes.extend_from_slice(b"\x1b[<64;2;2M mouse-ish ");
        }
        if line % 23 == 0 {
            bytes.extend_from_slice(b"\x1b[9;2u key-ish ");
        }
        if line % 29 == 0 {
            bytes.extend_from_slice(b"\x1b[200~ nested-start-ish ");
        }

        bytes.extend_from_slice(b"\r\n");
        line += 1;
    }

    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}
