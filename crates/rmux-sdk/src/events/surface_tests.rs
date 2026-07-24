use super::*;
use crate::{PaneCursor, PaneSnapshot};

fn frame(epoch: u64, revision: u64) -> PaneSurfaceFrame {
    frame_at(epoch, revision, revision)
}

fn frame_at(epoch: u64, revision: u64, next_output_sequence: u64) -> PaneSurfaceFrame {
    PaneSurfaceFrame {
        epoch,
        revision,
        next_output_sequence,
        snapshot: PaneSurfaceSnapshot {
            grid: PaneSnapshot::new(0, 0, Vec::new(), PaneCursor::default())
                .expect("zero-sized grid")
                .with_revision(revision),
            cell_hyperlink_ids: Vec::new(),
            title: String::new(),
            path: String::new(),
            hyperlinks: BTreeMap::new(),
            dynamic_colors: PaneSurfaceDynamicColors::default(),
            metadata_complete: true,
            mode_bits: 0,
            alternate: false,
            scroll_top: 0,
            scroll_bottom: 0,
            history_size: 0,
            history_bytes: 0,
        },
    }
}

fn linked_proto_snapshot(metadata_complete: bool) -> ProtoSnapshot {
    ProtoSnapshot {
        cols: 1,
        rows: 1,
        cells: vec![rmux_proto::PaneSnapshotCell {
            text: "X".to_owned(),
            width: 1,
            padding: false,
            attributes: 0,
            fg: 8,
            bg: 8,
            us: 8,
            link: 7,
        }],
        hyperlinks: metadata_complete
            .then(|| rmux_proto::PaneSurfaceHyperlink {
                id: 7,
                uri: "https://example.test/docs".to_owned(),
            })
            .into_iter()
            .collect(),
        cursor: rmux_proto::PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        title: "docs".to_owned(),
        path: "/work".to_owned(),
        dynamic_colors: rmux_proto::PaneSurfaceDynamicColors {
            foreground: Some("#abcdef".to_owned()),
            background: None,
            cursor: Some("rgb:1111/2222/3333".to_owned()),
        },
        metadata_complete,
        mode_bits: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 0,
        history_size: 0,
        history_bytes: 0,
        revision: 1,
    }
}

#[test]
fn mapper_preserves_surface_hyperlinks_and_dynamic_colors() {
    let snapshot =
        snapshot_from_proto(linked_proto_snapshot(true)).expect("valid surface snapshot");

    assert_eq!(snapshot.hyperlink_id_at(0, 0), Some(7));
    assert_eq!(
        snapshot.hyperlink_uri_at(0, 0),
        Some("https://example.test/docs")
    );
    assert_eq!(
        snapshot.hyperlinks.get(&7).map(String::as_str),
        Some("https://example.test/docs")
    );
    assert_eq!(
        snapshot.dynamic_colors.foreground.as_deref(),
        Some("#abcdef")
    );
    assert_eq!(
        snapshot.dynamic_colors.cursor.as_deref(),
        Some("rgb:1111/2222/3333")
    );
}

#[test]
fn mapper_rejects_a_complete_surface_with_missing_hyperlink_metadata() {
    let mut snapshot = linked_proto_snapshot(false);
    snapshot.metadata_complete = true;
    assert!(snapshot_from_proto(snapshot).is_err());

    let incomplete =
        snapshot_from_proto(linked_proto_snapshot(false)).expect("omission is declared");
    assert_eq!(incomplete.hyperlink_id_at(0, 0), Some(7));
    assert!(incomplete.hyperlinks.is_empty());
    assert!(!incomplete.metadata_complete);
}

#[test]
fn reducer_rejects_hyperlink_ids_that_do_not_match_the_grid() {
    let mut malformed = frame(1, 1);
    malformed.snapshot.cell_hyperlink_ids.push(Some(7));
    assert_eq!(
        PaneSurfaceState::default().apply(&PaneSurfaceEvent::Reset(malformed)),
        Err(PaneSurfaceApplyError::InvalidHyperlinkShape {
            expected: 0,
            actual: 1,
        })
    );
}

#[test]
fn reducer_requires_reset_then_monotone_same_epoch_patches() {
    let mut state = PaneSurfaceState::default();
    assert_eq!(
        state.apply(&PaneSurfaceEvent::Patch(frame(1, 1))),
        Err(PaneSurfaceApplyError::PatchBeforeReset)
    );
    assert_eq!(state.apply(&PaneSurfaceEvent::Reset(frame(1, 1))), Ok(true));
    assert_eq!(
        state.apply(&PaneSurfaceEvent::Patch(frame(1, 1))),
        Err(PaneSurfaceApplyError::StaleRevision {
            current_revision: 1,
            received_revision: 1,
        })
    );
    assert_eq!(
        state.apply(&PaneSurfaceEvent::Patch(frame(2, 2))),
        Err(PaneSurfaceApplyError::EpochMismatch {
            current_epoch: 1,
            received_epoch: 2,
        })
    );
    assert_eq!(state.apply(&PaneSurfaceEvent::Patch(frame(1, 2))), Ok(true));
    assert_eq!(state.frame(), Some(&frame(1, 2)));
}

#[test]
fn reducer_accepts_new_epoch_reset_and_rejects_events_after_end() {
    let mut state = PaneSurfaceState::default();
    state
        .apply(&PaneSurfaceEvent::Reset(frame(1, 1)))
        .expect("initial reset");
    state
        .apply(&PaneSurfaceEvent::Reset(frame(2, 2)))
        .expect("new epoch reset");
    state
        .apply(&PaneSurfaceEvent::End(PaneStreamEndReason::PaneRemoved))
        .expect("end");
    assert_eq!(state.ended(), Some(PaneStreamEndReason::PaneRemoved));
    assert_eq!(
        state.apply(&PaneSurfaceEvent::Lifecycle(
            PaneStreamLifecycleEvent::ProcessExited {
                output_sequence: None,
            }
        )),
        Err(PaneSurfaceApplyError::AlreadyEnded(
            PaneStreamEndReason::PaneRemoved
        ))
    );
}

#[test]
fn reducer_rejects_output_boundary_regressions_across_patches_and_resets() {
    let mut patch_state = PaneSurfaceState::default();
    patch_state
        .apply(&PaneSurfaceEvent::Reset(frame_at(1, 1, 8)))
        .expect("initial reset");
    assert_eq!(
        patch_state.apply(&PaneSurfaceEvent::Patch(frame_at(1, 2, 7))),
        Err(PaneSurfaceApplyError::OutputSequenceRegressed {
            current_sequence: 8,
            received_sequence: 7,
        })
    );
    patch_state
        .apply(&PaneSurfaceEvent::Patch(frame_at(1, 2, 8)))
        .expect("a frame can advance without new raw output");

    let mut reset_state = PaneSurfaceState::default();
    reset_state
        .apply(&PaneSurfaceEvent::Reset(frame_at(1, 1, 8)))
        .expect("initial reset");
    assert_eq!(
        reset_state.apply(&PaneSurfaceEvent::Reset(frame_at(2, 2, 7))),
        Err(PaneSurfaceApplyError::OutputSequenceRegressed {
            current_sequence: 8,
            received_sequence: 7,
        })
    );
}
