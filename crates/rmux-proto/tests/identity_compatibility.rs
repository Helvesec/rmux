//! Old public paths retain the single shared value type and wire diagnostics.
use rmux_proto::{identity, types, PaneId, RmuxError, SessionId, SessionName, WindowId};
use std::error::Error;

#[test]
fn old_paths_are_assignable_without_conversion_or_revalidation() {
    let name: rmux_types::SessionName = SessionName::new("a\\n:b.c").unwrap();
    let old_module: identity::SessionName = name;
    let old_types: types::SessionName = old_module;
    let root: SessionName = old_types;
    assert_eq!(root.as_str(), "a\\\\n_b_c");
    let session: rmux_types::SessionId = identity::SessionId::new(u32::MAX);
    let window: rmux_types::WindowId = identity::WindowId::new(0);
    let pane: rmux_types::PaneId = identity::PaneId::new(u32::MAX);
    let _: (SessionId, WindowId, PaneId) = (session, window, pane);
    let error: identity::SessionNameError = SessionName::new("").unwrap_err();
    let _: rmux_types::SessionNameError = error;
}

#[test]
fn validation_maps_to_the_original_protocol_leaf_and_tag_zero() {
    let error = rmux_proto::Target::parse("").unwrap_err();
    assert_eq!(error, RmuxError::EmptySessionName);
    assert_eq!(
        error.to_string(),
        "invalid session name: session names must be non-empty"
    );
    assert_eq!(format!("{error:?}"), "EmptySessionName");
    assert!(error.source().is_none());
    assert_eq!(bincode::serialize(&error).unwrap(), [0; 4]);
    assert_eq!(
        serde_json::to_string(&error).unwrap(),
        "\"EmptySessionName\""
    );
    assert_eq!(bincode::deserialize::<RmuxError>(&[0; 4]).unwrap(), error);
    for raw in [":0", ":0.1"] {
        assert_eq!(rmux_proto::Target::parse(raw), Err(error.clone()));
    }
}
