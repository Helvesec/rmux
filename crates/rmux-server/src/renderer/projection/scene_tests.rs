use super::*;
use rmux_core::FloatingPaneRect;
use rmux_proto::{SessionName, TerminalSize, WindowTarget};

#[test]
fn scene_mapping_keeps_signed_identity_focus_and_server_occlusion() {
    let mut session = crate::session_fixture::SessionFixture::new(
        SessionName::new("scene-mapping").unwrap(),
        TerminalSize { cols: 20, rows: 8 },
    );
    let options = OptionStore::new();
    let query = rmux_proto::scene::SceneQuery {
        window_index: 0,
        expected_identity: None,
    };
    let initial = window_scene(session.view(), &options, &query).unwrap();
    assert_eq!(initial.panes.len(), 1);
    assert!(!initial.panes[0].floating);
    session.split_active_pane().unwrap();
    let target = WindowTarget::with_window(session.name().clone(), 0);
    session
        .store
        .window_layout_context_mut(&target)
        .unwrap()
        .float_pane(1, FloatingPaneRect::new(-3, 1, 9, 3).unwrap())
        .unwrap();
    let scene = window_scene(session.view(), &options, &query).unwrap();
    let front = scene.panes.iter().find(|p| p.floating).unwrap();
    assert_eq!(front.content.x, -3);
    assert_eq!(front.content_clip.x, 0);
    assert_eq!(front.spans[0].source_col, 3);
    assert_eq!(front.id, session.window().pane(1).unwrap().id());
    assert!(front.focused);
    assert_ne!(scene.revision, initial.revision);
    let rear = scene.panes.iter().find(|p| !p.floating).unwrap();
    for s in &rear.spans {
        for f in &front.spans {
            assert!(s.y != f.y || s.x + s.cols <= f.x || f.x + f.cols <= s.x);
        }
        for b in &scene.borders {
            assert!(s.y != b.y || b.x < s.x || b.x >= s.x + s.cols);
        }
    }
    assert_eq!(
        window_scene(session.view(), &options, &query).unwrap(),
        scene
    );
    let wrong = rmux_proto::scene::SceneQuery {
        expected_identity: Some((scene.session_id, rmux_proto::WindowId::new(u32::MAX))),
        ..query
    };
    assert!(window_scene(session.view(), &options, &wrong).is_err());
}

#[test]
fn scene_projection_of_linked_slot_preserves_owner_and_does_not_select_canonical_session() {
    let mut session = crate::session_fixture::SessionFixture::new(
        SessionName::new("scene-linked").unwrap(),
        TerminalSize { cols: 20, rows: 8 },
    );
    let window_id = session.window().id();
    session.link_window_id(7, window_id, false, false).unwrap();
    let query = rmux_proto::scene::SceneQuery {
        window_index: 7,
        expected_identity: Some((session.id(), window_id)),
    };
    let scene = window_scene(session.view(), &OptionStore::new(), &query).unwrap();
    assert_eq!(scene.window_id, window_id);
    assert_eq!(scene.window_index, 7);
    assert_eq!(session.active_window_index(), 0);
    assert_eq!(scene.panes[0].id, session.window().panes()[0].id());
}
