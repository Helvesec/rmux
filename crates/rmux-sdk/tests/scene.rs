use rmux_sdk::{PaneId, ScenePane, SceneRect, SceneSnapshot, SceneState, SessionId, WindowId};
fn snapshot() -> SceneSnapshot {
    SceneSnapshot {
        schema: 1,
        session_id: SessionId::new(1),
        window_id: WindowId::new(3),
        window_index: 0,
        revision: "baseline".into(),
        cols: 80,
        rows: 24,
        content_y_offset: 0,
        active_pane_id: None,
        panes: vec![],
        borders: vec![],
    }
}
fn pane(id: u32) -> ScenePane {
    let rect = SceneRect {
        x: -2,
        y: 1,
        cols: 8,
        rows: 3,
    };
    ScenePane {
        id: PaneId::new(id),
        index: 0,
        floating: true,
        layer: 1,
        focused: false,
        visible: true,
        content: rect.clone(),
        frame: rect.clone(),
        content_clip: rect.clone(),
        frame_clip: rect,
        hit_regions: vec![],
        spans: vec![],
    }
}
#[test]
fn observations_cover_create_change_remove_selection_and_explicit_gap() {
    let mut state = SceneState::default();
    let mut scene = snapshot();
    assert!(state.apply(scene.clone()).reset);
    assert!(!state.apply(scene.clone()).reset);
    scene.panes.push(pane(10));
    assert_eq!(state.apply(scene.clone()).created, vec![PaneId::new(10)]);
    scene.panes[0].content.x = 3;
    scene.active_pane_id = Some(PaneId::new(10));
    let change = state.apply(scene.clone());
    assert_eq!(change.changed, vec![PaneId::new(10)]);
    assert!(change.selection_changed);
    scene.panes.clear();
    scene.panes.push(pane(11)); // same slot, distinct stable object
    let change = state.apply(scene.clone());
    assert_eq!(change.removed, vec![PaneId::new(10)]);
    assert_eq!(change.created, vec![PaneId::new(11)]);
    let recovery = state.resynchronize(scene.clone());
    assert!(recovery.reset);
    assert!(recovery.removed.is_empty());
    scene.window_id = WindowId::new(4);
    assert!(state.apply(scene).reset);
}
