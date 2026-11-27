use super::*;
use crate::handler::pane_support::take_delimiter_search_bytes_for_test;

const LARGE_PASTE_TARGET_BYTES: usize = 64 * 1024;
const ONE_MEBIBYTE: usize = 1024 * 1024;
const CHUNK_PATTERN: &[usize] = &[1, 2, 4, 8, 3, 13, 89, 233, 1024, 7, 4096];
// The live input path hands a large frame to its steps one reroute chunk (1 KiB) at a time.
const LIVE_INPUT_REROUTE_CHUNK_BYTES: usize = 1024;
// The paste's end is searched for through its whole body, once: each step searches the bytes it
// was handed plus the few retained bytes a delimiter split across two steps may start in, never
// again the paste the client already retains.
const SPLIT_DELIMITER_BYTES: usize = b"\x1b[201~".len();
/// A held input path never returns, whether it waits for a wake-up that never comes or spins
/// without yielding. This only turns that hang into a failure of the test that holds it: the
/// work is bounded by counting it, never by how long a loaded host takes to do it.
const HELD_INPUT_PATH_GUARD: Duration = Duration::from_secs(120);

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

#[test]
fn live_attach_one_mebibyte_bracketed_paste_has_bounded_work() {
    fail_if_held(one_mebibyte_bracketed_paste_has_bounded_work);
}

/// Runs `test` as `#[tokio::test]` would, on a thread of its own, and fails when it has neither
/// finished nor failed within [`HELD_INPUT_PATH_GUARD`]. The held thread is left behind, so a
/// hang fails this test by name instead of freezing the rest of the pass, and a blocking spin is
/// caught as surely as an await that never wakes.
fn fail_if_held<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let (finished_tx, finished_rx) = std::sync::mpsc::channel::<()>();
    let name = std::thread::current()
        .name()
        .unwrap_or("held input path guard")
        .to_owned();
    let worker = std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the test's runtime builds");
            runtime.block_on(test());
            drop(runtime);
            let _ = finished_tx.send(());
        })
        .expect("the test's thread starts");
    match finished_rx.recv_timeout(HELD_INPUT_PATH_GUARD) {
        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            if let Err(failure) = worker.join() {
                std::panic::resume_unwind(failure);
            }
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!(
            "the client's input path is held: the test neither finished nor failed within \
             {HELD_INPUT_PATH_GUARD:?}"
        ),
    }
}

async fn one_mebibyte_bracketed_paste_has_bounded_work() {
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

    // The work is counted, not timed: a loaded host may take seconds for what costs
    // milliseconds, while a rescan of the retained paste costs half a gibibyte at any load.
    let mut pending_input = Vec::new();
    handler.live_input_work.take();
    take_delimiter_search_bytes_for_test();
    handler
        .handle_attached_live_input(requester_pid, &mut pending_input, &input)
        .await
        .expect("one MiB bracketed paste succeeds");
    let steps = handler.live_input_work.take().steps;
    let searched = take_delimiter_search_bytes_for_test();
    assert!(
        (1..=input.len().div_ceil(LIVE_INPUT_REROUTE_CHUNK_BYTES)).contains(&steps),
        "one MiB bracketed paste must take a live input step per reroute chunk: {steps} steps"
    );
    assert!(
        (body.len()..=input.len() + steps * SPLIT_DELIMITER_BYTES).contains(&searched),
        "one MiB bracketed paste must search each byte for its end once: {searched} bytes \
         searched for {} typed in {steps} steps",
        input.len()
    );
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
