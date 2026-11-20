use rmux_proto::{scene::*, HandshakeRequest, HandshakeResponse, SessionId, WindowId};

#[test]
fn scene_negotiation_is_additive_and_legacy_server_rejects_before_query() {
    let request = HandshakeRequest::requiring([CAPABILITY_SCENE_V1]);
    assert!(request.validate_against(&[]).is_err());
    assert!(HandshakeResponse::current()
        .capabilities
        .contains(&CAPABILITY_SCENE_V1.to_owned()));
    let query = SceneQuery {
        window_index: 3,
        expected_identity: Some((SessionId::new(2), WindowId::new(9))),
    };
    let encoded = query.encode().unwrap();
    assert_eq!(
        SceneQuery::decode(encoded.strip_prefix(SCENE_QUERY_PREFIX).unwrap()).unwrap(),
        query
    );
    assert!(encoded.starts_with('\0'));
    let suffix = r#"{"window_index":3,"expected_identity":null,"future_field":true}"#;
    assert_eq!(SceneQuery::decode(suffix).unwrap().window_index, 3);
    assert!(SceneQuery::decode("{}").is_err());
}

#[test]
fn scene_revision_tracks_content_and_excludes_its_previous_value() {
    let mut scene = SceneSnapshot {
        schema: 1,
        session_id: SessionId::new(2),
        window_id: WindowId::new(3),
        window_index: 0,
        revision: String::new(),
        cols: 80,
        rows: 24,
        content_y_offset: 0,
        active_pane_id: None,
        panes: vec![],
        borders: vec![],
    };
    scene.seal().unwrap();
    let first = scene.revision.clone();
    assert_eq!(first.len(), 64);
    scene.seal().unwrap();
    assert_eq!(scene.revision, first);
    scene.cols = 79;
    scene.seal().unwrap();
    assert_ne!(scene.revision, first);
    let mut value = serde_json::to_value(&scene).unwrap();
    value["future_field"] = true.into();
    assert_eq!(
        serde_json::from_value::<SceneSnapshot>(value).unwrap(),
        scene
    );
}
