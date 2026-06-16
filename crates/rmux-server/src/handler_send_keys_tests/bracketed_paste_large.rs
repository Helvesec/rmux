use super::*;

const LARGE_PASTE_TARGET_BYTES: usize = 64 * 1024;
const CHUNK_PATTERN: &[usize] = &[1, 2, 4, 8, 3, 13, 89, 233, 1024, 7, 4096];

#[tokio::test]
async fn live_attach_large_bracketed_paste_survives_irregular_chunks() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let requester_pid = std::process::id();

    create_send_keys_test_session(&handler, &alpha).await;

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
