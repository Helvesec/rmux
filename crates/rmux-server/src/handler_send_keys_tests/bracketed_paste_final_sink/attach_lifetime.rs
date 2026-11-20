use super::*;

async fn assert_refresh_preserves_paste(bracket_aware: bool, fragmented: bool) {
    let handler = RequestHandler::new();
    let session = session_name("final-sink-split");
    let body = b"refresh-between-reads";
    let paste = wrapped(body);
    let expected = if bracket_aware {
        paste.as_slice()
    } else {
        body
    };
    let slot = FinalSinkSlot::new("refresh-lifetime", expected, bracket_aware);
    started_final_sink_pane(&handler, &session, &slot, bracket_aware).await;
    let client = attach_to(&handler, &session).await;
    let mut pending_input = Vec::new();
    let chunks = if fragmented {
        vec![
            &paste[..2],
            &paste[2..4],
            &paste[4..paste.len() - 1],
            &paste[paste.len() - 1..],
        ]
    } else {
        vec![paste.as_slice()]
    };
    for chunk in chunks {
        // Pane output may refresh clients between any two input reads. Invoke
        // that production boundary directly so the proof needs no lucky timer.
        handler
            .refresh_attached_session_for_pane_output(&session)
            .await;
        client
            .send_read(&handler, &mut pending_input, chunk)
            .await
            .expect("final-sink attach must survive output refresh");
    }
    assert!(pending_input.is_empty());
    slot.assert_application_bytes("output refresh must preserve the attached paste destination");
}

#[tokio::test]
async fn complete_aware_paste_survives_output_refresh() {
    assert_refresh_preserves_paste(true, false).await;
}

#[tokio::test]
async fn fragmented_aware_paste_survives_output_refresh() {
    assert_refresh_preserves_paste(true, true).await;
}

#[tokio::test]
async fn complete_unaware_paste_survives_output_refresh() {
    assert_refresh_preserves_paste(false, false).await;
}

#[tokio::test]
async fn fragmented_unaware_paste_survives_output_refresh() {
    assert_refresh_preserves_paste(false, true).await;
}
