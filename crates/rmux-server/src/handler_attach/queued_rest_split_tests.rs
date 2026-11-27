//! D1 R10: what the split of a frame's rest does with a paste left open in a part that is not the
//! last. The input path never builds that composition (the decoder leaves paste bodies opaque, so a
//! paste left open always ends the rest; see queued_rest_paste_tests); built directly here, it shows
//! that the paste's conservation rests on that.

use rmux_proto::SessionId;

use super::{
    split_rest_of_frame, ActiveAttachIdentity, FramePart, PasteScan, QueuedFrame, QueuedUnit,
};

fn queued_bytes(frames: &[QueuedFrame]) -> Vec<Vec<u8>> {
    frames
        .iter()
        .map(|frame| match &frame.unit {
            QueuedUnit::Live { bytes, .. } => bytes.clone(),
            other => panic!("expected queued bytes, got {other:?}"),
        })
        .collect()
}

#[test]
fn an_open_paste_stays_with_the_client_only_when_it_ends_the_rest_of_a_frame() {
    let identity = super::queued_identity::QueuedInputIdentity::for_test(
        ActiveAttachIdentity::new(974_910, 1, SessionId::from(1)),
    );
    // As the input path builds it: the paste left open is the rest's last part.
    let rest = vec![FramePart::Bytes(b"\x1b[200~abcd".to_vec())];
    let (frames, (focus_reports, retained)) = split_rest_of_frame(
        identity.clone(),
        Vec::new(),
        rest,
        &PasteScan::default(),
        |_| 0,
    );
    assert!(frames.is_empty() && focus_reports.is_empty());
    assert_eq!(retained, b"\x1b[200~abcd");

    // Built directly: the same paste left open in a part another part follows. It is queued open
    // and nothing stays with the client, so the end of a binding would flush it.
    let rest = vec![
        FramePart::Bytes(b"\x1b[200~ab".to_vec()),
        FramePart::Bytes(b"cd".to_vec()),
    ];
    let (frames, (focus_reports, retained)) = split_rest_of_frame(
        identity.clone(),
        Vec::new(),
        rest,
        &PasteScan::default(),
        |_| 0,
    );
    assert!(focus_reports.is_empty() && retained.is_empty());
    assert_eq!(
        queued_bytes(&frames),
        [b"\x1b[200~ab".to_vec(), b"cd".to_vec()]
    );
}
