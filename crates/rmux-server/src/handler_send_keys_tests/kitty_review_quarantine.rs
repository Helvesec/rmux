use super::kitty_review::bind;
use super::kitty_runtime::{captured, input, output, setup};

#[tokio::test]
async fn kitty_review_r3_invalid_frames_never_turn_their_suffix_into_a_binding() {
    let (handler, _, pid, target) = setup("kitty-review-quarantine").await;
    output(&handler, &target, b"\x1b[=10u").await;
    bind(&handler, "u").await;
    let frames = [
        b"\x1b[97;xuZ".to_vec(),
        format!("\x1b[97;{}uZ", "0".repeat(4096)).into_bytes(),
        format!("\x1b[97;x{}uZ", "0".repeat(4096)).into_bytes(),
    ];
    for frame in frames {
        let cuts = if frame.len() < 20 {
            (0..=frame.len()).collect::<Vec<_>>()
        } else {
            vec![
                0,
                2,
                3,
                6,
                7,
                4095,
                4096,
                4097,
                frame.len() - 1,
                frame.len(),
            ]
        };
        for cut in cuts {
            handler
                .state
                .lock()
                .await
                .start_pane_input_capture_for_test(&target);
            let mut pending = Vec::new();
            handler
                .handle_attached_live_input(pid, &mut pending, &frame[..cut])
                .await
                .unwrap();
            handler
                .handle_attached_live_input(pid, &mut pending, &frame[cut..])
                .await
                .unwrap();
            assert!(pending.is_empty(), "cut {cut}");
            captured(&handler, &target, b"Z").await;
        }
    }
}

#[tokio::test]
async fn kitty_review_r3_timeout_interruption_and_paste_keep_their_boundaries() {
    let (handler, _, pid, target) = setup("kitty-review-quarantine-neighbors").await;
    output(&handler, &target, b"\x1b[=10u").await;
    bind(&handler, "u").await;
    let mut pending = Vec::new();
    handler
        .handle_attached_live_input(pid, &mut pending, b"\x1b[97;x")
        .await
        .unwrap();
    handler
        .flush_attached_pending_escape_input(pid, &mut pending)
        .await
        .unwrap();
    assert!(pending.is_empty());
    captured(&handler, &target, b"").await;
    input(&handler, pid, b"u").await;
    handler
        .handle_attached_live_input(pid, &mut pending, b"\x1b[97;x")
        .await
        .unwrap();
    handler
        .handle_attached_live_input(pid, &mut pending, b"\x1b[98u")
        .await
        .unwrap();
    for chunk in [b"\x1b[200~\x1b[97;x".as_slice(), b"u\x1b[201~"] {
        handler
            .handle_attached_live_input(pid, &mut pending, chunk)
            .await
            .unwrap();
    }
    output(&handler, &target, b"\x1b[?2004h").await;
    for chunk in [b"\x1b[200~\x1b[97;x".as_slice(), b"u\x1b[201~"] {
        handler
            .handle_attached_live_input(pid, &mut pending, chunk)
            .await
            .unwrap();
    }
    captured(
        &handler,
        &target,
        b"BOUND\x1b[98u\x1b[97;xu\x1b[200~\x1b[97;xu\x1b[201~",
    )
    .await;
}
