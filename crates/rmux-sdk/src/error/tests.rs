use std::error::Error as StdError;

use super::RmuxError;

#[test]
fn protocol_wire_version_maps_to_sdk_guidance() {
    let error = RmuxError::from_proto(rmux_proto::RmuxError::UnsupportedWireVersion {
        got: 7,
        minimum: 1,
        maximum: 1,
    });

    assert_eq!(
        error.to_string(),
        "unsupported feature `protocol.wire_version`\n\
         hint: upgrade the rmux daemon or use an SDK that supports wire version 7 \
         (supported range 1..=1)"
    );
    assert_eq!(error.feature(), Some("protocol.wire_version"));
    assert_eq!(
        error.hint(),
        Some(
            "upgrade the rmux daemon or use an SDK that supports wire version 7 \
             (supported range 1..=1)"
        )
    );
    assert!(
        !error.to_string().contains("unsupported RMUX wire version"),
        "SDK display should not expose raw lower-crate wire-version text"
    );
    assert!(StdError::source(&error).is_none());
}

#[test]
fn unknown_command_maps_to_unsupported_capability_guidance() {
    let error = RmuxError::from_proto(rmux_proto::RmuxError::UnknownCommand(
        "capture-pane".to_owned(),
    ));

    assert_eq!(
        error.to_string(),
        "unsupported feature `command.capture-pane`\n\
         hint: upgrade the rmux daemon or use a command advertised by the negotiated \
         command inventory before sending `capture-pane`"
    );
    assert_eq!(error.feature(), Some("command.capture-pane"));
    assert_eq!(
        error.hint(),
        Some(
            "upgrade the rmux daemon or use a command advertised by the negotiated \
             command inventory before sending `capture-pane`"
        )
    );
    assert!(StdError::source(&error).is_none());
}

#[test]
fn typed_proto_runtime_errors_map_without_text_parsing() {
    let pane_not_found = RmuxError::from_proto(rmux_proto::RmuxError::PaneNotFound {
        session_name: rmux_proto::SessionName::new("alpha").expect("valid session"),
        pane_id: rmux_proto::PaneId::new(42),
    });
    assert!(matches!(
        pane_not_found,
        RmuxError::PaneNotFound {
            pane_id,
            ..
        } if pane_id == rmux_proto::PaneId::new(42)
    ));

    let still_running = RmuxError::from_proto(rmux_proto::RmuxError::ProcessStillRunning);
    assert!(matches!(
        still_running,
        RmuxError::ProcessStillRunning { .. }
    ));

    let spawn_failed = RmuxError::from_proto(rmux_proto::RmuxError::SpawnFailed {
        message: "failed to spawn pane shell: denied".to_owned(),
    });
    assert!(matches!(
        spawn_failed,
        RmuxError::SpawnFailed { ref message, .. } if message.contains("denied")
    ));

    let lease_lost = RmuxError::from_proto(rmux_proto::RmuxError::OwnedSessionLeaseLost {
        session_name: rmux_proto::SessionName::new("owned").expect("valid session"),
    });
    assert!(matches!(
        lease_lost,
        RmuxError::OwnedSessionLeaseLost { ref message, .. } if message.contains("owned")
    ));
}
