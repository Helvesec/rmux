use super::{PaneSize, ServerMessage};
use crate::web::WebShareConnectionCounts;

#[test]
fn debug_redacts_ready_spectator_pin_without_changing_wire_payload() {
    let message = ServerMessage::Ready {
        protocol_version: 1,
        capabilities: &[],
        pane_size: PaneSize { cols: 80, rows: 24 },
        scope: "session",
        share_id: "debug-pin-canary",
        session_name: Some("demo"),
        role: "operator",
        operator: true,
        operator_access: true,
        spectator_access: true,
        controls: true,
        show_viewers: true,
        spectator_pairing_code: Some("826591"),
        ttl_remaining_seconds: None,
        connection_counts: WebShareConnectionCounts::new(0, None, 1, None),
        terminal_palette: None,
    };
    assert_eq!(
        serde_json::to_value(&message).unwrap()["spectator_pairing_code"],
        "826591"
    );
    for diagnostic in [format!("{message:?}"), format!("{message:#?}")] {
        assert!(
            !diagnostic.contains("826591"),
            "Debug must redact ready PIN"
        );
    }
}
