use super::*;
use rmux_proto::{
    RmuxWindowSnapshot, RmuxWindowSnapshotCell, SessionId, SessionName, WindowTarget,
};

fn rect(x: i32, y: i32, cols: i32, rows: i32) -> FloatingPaneRect {
    FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect")
}

fn target() -> WindowTarget {
    WindowTarget::with_window(SessionName::new("alpha").expect("valid session name"), 0)
}

fn stack_ids(window: &Window) -> Vec<PaneId> {
    window
        .floating_stack()
        .placements()
        .iter()
        .map(|placement| placement.pane_id())
        .collect()
}

fn last_pane_id(window: &Window) -> Option<PaneId> {
    window
        .last_pane_index()
        .and_then(|pane_index| window.pane_id(pane_index))
}

fn mixed_window_with_selected_rear_float() -> (Window, RmuxWindowSnapshot) {
    let mut window = Window::new(TerminalSize::new(120, 40));
    window.split_after_position_with_id(0, PaneId::new(10));
    window.split_after_position_with_id(1, PaneId::new(20));
    window.set_layout(LayoutName::EvenVertical);
    window.float_pane(1, rect(-3, 4, 30, 10)).unwrap();
    window.float_pane(2, rect(45, -2, 22, 7)).unwrap();
    window.select_floating_pane(1).unwrap();
    window.raise_floating_pane(2).unwrap();
    window.set_floating_pane_visible(1, false).unwrap();

    let snapshot = window
        .export_rmux_window_snapshot(SessionId::new(1), target())
        .unwrap();
    (window, snapshot)
}

fn exported_snapshot(window: &Window) -> RmuxWindowSnapshot {
    window
        .export_rmux_window_snapshot(SessionId::new(1), target())
        .unwrap()
}

fn assert_rejects_without_effect(snapshot: RmuxWindowSnapshot, expected_error: &str) {
    let (source, _) = mixed_window_with_selected_rear_float();
    let mut target = source.clone();
    let before = target.clone();

    let error = target
        .restore_rmux_window_snapshot(&snapshot)
        .expect_err("snapshot restore must reject");

    assert_eq!(target, before);
    assert!(
        error.to_string().contains(expected_error),
        "unexpected error: {error}"
    );
}

#[test]
fn snapshot_round_trip_restores_float_selection_without_raising_front() {
    let (source, snapshot) = mixed_window_with_selected_rear_float();
    let mut target = source.clone();

    target.select_floating_pane(2).unwrap();
    target.raise_floating_pane(1).unwrap();
    target.set_floating_pane_visible(1, true).unwrap();
    target.move_floating_pane(1, 8, 6).unwrap();
    target.move_floating_pane(2, 50, 3).unwrap();
    target.set_layout(LayoutName::EvenHorizontal);

    let resized_panes = target.restore_rmux_window_snapshot(&snapshot).unwrap();

    assert!(
        resized_panes.is_empty(),
        "position, visibility, order, and selection restores must not resize PTYs"
    );
    assert_eq!(
        super::snapshot::snapshot_pane_ids(snapshot.tiled.as_ref().unwrap()),
        vec![PaneId::new(0)]
    );
    assert_eq!(exported_snapshot(&target), snapshot);
    assert_eq!(stack_ids(&target), vec![PaneId::new(10), PaneId::new(20)]);
    assert_eq!(target.active_pane().unwrap().id(), PaneId::new(10));
    assert_eq!(last_pane_id(&target), snapshot.last_pane);
    assert_eq!(
        target
            .floating_stack()
            .placement(PaneId::new(10))
            .unwrap()
            .rect(),
        rect(-3, 4, 30, 10)
    );
    assert!(!target
        .floating_stack()
        .placement(PaneId::new(10))
        .unwrap()
        .visible());
    assert_eq!(
        target
            .floating_stack()
            .placement(PaneId::new(20))
            .unwrap()
            .rect(),
        rect(45, -2, 22, 7)
    );
}

#[test]
fn snapshot_restore_reports_only_real_size_deltas() {
    let (_, snapshot) = mixed_window_with_selected_rear_float();
    let mut target = Window::new(TerminalSize::new(120, 40));
    target.split_after_position_with_id(0, PaneId::new(10));
    target.split_after_position_with_id(1, PaneId::new(20));
    target.float_pane(1, rect(-3, 4, 35, 10)).unwrap();
    target.float_pane(2, rect(45, -2, 22, 9)).unwrap();
    target.select_floating_pane(1).unwrap();
    target.raise_floating_pane(2).unwrap();

    let resized_panes = target.restore_rmux_window_snapshot(&snapshot).unwrap();

    assert_eq!(resized_panes, vec![PaneId::new(10), PaneId::new(20)]);
    assert_eq!(exported_snapshot(&target), snapshot);
}

#[test]
fn snapshot_restores_next_and_previous_cycle_without_overwriting_old_layout() {
    for previous in [false, true] {
        let mut window = Window::new(TerminalSize::new(120, 40));
        window.split_after_position(0);
        window.split_after_position(1);
        window.set_layout(LayoutName::EvenHorizontal);
        let snapshot = exported_snapshot(&window);
        let mut reference = window.clone();
        let expected = if previous {
            reference.previous_layout()
        } else {
            reference.next_layout()
        };
        window.set_layout(LayoutName::MainVertical);
        window.old_layout = Some(window.layout_dump());
        let old_layout = window.old_layout.clone();
        window.restore_rmux_window_snapshot(&snapshot).unwrap();
        assert_eq!(window.old_layout, old_layout);
        let actual = if previous {
            window.previous_layout()
        } else {
            window.next_layout()
        };
        assert_eq!(actual, expected, "cycle direction previous={previous}");
        assert_eq!(window.layout_dump(), reference.layout_dump());
    }
}

#[test]
fn snapshot_restore_rejects_malformed_or_foreign_payloads_without_effect() {
    let (_, snapshot) = mixed_window_with_selected_rear_float();

    let mut duplicate = snapshot.clone();
    duplicate.floating[0].pane_id = PaneId::new(0);
    assert_rejects_without_effect(duplicate, "appears more than once");

    let mut extra_missing = snapshot.clone();
    extra_missing.floating[0].pane_id = PaneId::new(99);
    assert_rejects_without_effect(extra_missing, "pane set does not match");

    let mut wrong_size = snapshot.clone();
    wrong_size.size.cols += 1;
    assert_rejects_without_effect(wrong_size, "size does not match");

    let mut wrong_window = snapshot.clone();
    wrong_window.window_id = WindowId::new(99);
    assert_rejects_without_effect(wrong_window, "window id does not match");

    let mut snapshot_zoomed = snapshot.clone();
    snapshot_zoomed.zoomed = true;
    assert_rejects_without_effect(snapshot_zoomed, "zoomed windows cannot be restored");

    let mut bad_float = snapshot.clone();
    bad_float.floating[0].cols = 0;
    assert_rejects_without_effect(bad_float, "floating pane dimensions must be positive");

    let mut bad_tiled_offset = snapshot.clone();
    match &mut bad_tiled_offset.tiled {
        Some(RmuxWindowSnapshotCell::Pane { x, .. }) => *x = 1,
        Some(RmuxWindowSnapshotCell::Split { children, .. }) => {
            let RmuxWindowSnapshotCell::Pane { x, .. } = &mut children[0] else {
                panic!("first child is a pane");
            };
            *x = 1;
        }
        None => panic!("fixture has a tiled tree"),
    }
    assert_rejects_without_effect(bad_tiled_offset, "tiled");
}

#[test]
fn zoomed_source_or_target_is_not_snapshotted_or_restored() {
    let (source, snapshot) = mixed_window_with_selected_rear_float();

    let mut zoomed_source = source.clone();
    assert!(zoomed_source.toggle_zoom(1));
    assert!(zoomed_source
        .export_rmux_window_snapshot(SessionId::new(1), target())
        .is_err());

    let mut zoomed_target = source.clone();
    assert!(zoomed_target.toggle_zoom(1));
    let before = zoomed_target.clone();
    let error = zoomed_target
        .restore_rmux_window_snapshot(&snapshot)
        .expect_err("zoomed target must reject");

    assert_eq!(zoomed_target, before);
    assert!(error
        .to_string()
        .contains("zoomed windows cannot be restored"));
}

#[test]
fn snapshot_v2_retains_overhang_and_attachment_next_operation() {
    let (mut w, _) = mixed_window_with_selected_rear_float();
    w.raise_floating_pane(1).unwrap();
    assert!(w
        .export_rmux_window_snapshot(SessionId::new(1), target())
        .unwrap_err()
        .to_string()
        .contains("v2"));
    w.resize_canonical_absolute(1, None, Some(8)).unwrap();
    let snapshot = w
        .export_rmux_window_snapshot_v2(SessionId::new(1), target())
        .unwrap();
    let mut expected = w.clone();
    expected
        .resize_canonical_absolute(1, None, Some(6))
        .unwrap();
    w.move_floating_pane(1, 55, -10).unwrap();
    w.resize_floating_pane(2, 10, 11).unwrap();
    w.restore_rmux_window_snapshot_v2(&snapshot).unwrap();
    assert_eq!(
        w.export_rmux_window_snapshot_v2(SessionId::new(1), target())
            .unwrap(),
        snapshot
    );
    w.resize_canonical_absolute(1, None, Some(6)).unwrap();
    assert_eq!(w, expected);

    let mut single = Window::new(TerminalSize::new(80, 12));
    single.split_after_position_with_id(0, PaneId::new(10));
    single.float_pane(1, rect(-3, 1, 12, 4)).unwrap();
    single
        .resize_canonical_absolute(1, Some(10), Some(3))
        .unwrap();
    assert_eq!(single.size(), TerminalSize::new(80, 12));
    assert_eq!(single.pane(0).unwrap().geometry().rows(), 13);
    assert!(single
        .export_rmux_window_snapshot(SessionId::new(1), target())
        .is_err());
    let overhang = single
        .export_rmux_window_snapshot_v2(SessionId::new(1), target())
        .unwrap();
    single.resize_canonical_absolute(1, None, Some(1)).unwrap();
    assert_eq!(
        single.restore_rmux_window_snapshot_v2(&overhang).unwrap(),
        vec![PaneId::new(0), PaneId::new(10)]
    );
    assert!(single
        .restore_rmux_window_snapshot_v2(&overhang)
        .unwrap()
        .is_empty());
}

#[test]
fn snapshot_v2_rejects_malformed_topology_and_geometry_before_effects() {
    let (mut w, _) = mixed_window_with_selected_rear_float();
    let good = w
        .export_rmux_window_snapshot_v2(SessionId::new(1), target())
        .unwrap();
    for n in 0..10 {
        let mut bad = good.clone();
        match n {
            0 => bad.version = 1,
            1 => bad
                .resize_topology
                .floating_attachment_order
                .push(PaneId::new(10)),
            2 => bad.resize_topology.floating_attachment_order[0] = PaneId::new(999),
            3 => bad.resize_topology.root_direction = None,
            4 => bad.floating[0].cols = 0,
            5 => bad.floating[0].cols = 65536,
            6 => bad.window_id = WindowId::new(999),
            7 => bad.size.rows += 1,
            8 => bad.zoomed = true,
            _ => bad.floating[0].x = i32::MAX,
        }
        let before = w.clone();
        assert!(w.restore_rmux_window_snapshot_v2(&bad).is_err(), "case {n}");
        assert_eq!(w, before, "case {n}");
    }
}
