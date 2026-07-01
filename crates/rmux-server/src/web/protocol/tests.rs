use super::{
    encode_session_key, parse_pane_resize_body, parse_resize_body, session_logout_allowed,
    valid_window_name, AuthWireMessage, ClientMessage, WEB_SHARE_PROTOCOL_VERSION,
};
use rmux_proto::ResizePaneAdjustment;

#[test]
fn session_logout_requires_operator_controls() {
    assert!(!session_logout_allowed(false, false));
    assert!(!session_logout_allowed(false, true));
    assert!(!session_logout_allowed(true, false));
    assert!(session_logout_allowed(true, true));
}

#[test]
fn auth_wire_rejects_unknown_fields() {
    let message = format!(
        r#"{{"type":"auth","protocol_version":{},"capabilities":["e2ee-token-auth"],"extra":"nope"}}"#,
        WEB_SHARE_PROTOCOL_VERSION
    );

    assert!(serde_json::from_str::<AuthWireMessage>(&message).is_err());
}

#[test]
fn auth_wire_requires_versioned_e2ee_capability_payload() {
    let message = format!(
        r#"{{"type":"auth","protocol_version":{},"capabilities":["e2ee-token-auth"]}}"#,
        WEB_SHARE_PROTOCOL_VERSION
    );

    let decoded =
        serde_json::from_str::<AuthWireMessage>(&message).expect("current auth payload decodes");

    assert_eq!(decoded.kind, "auth");
    assert_eq!(decoded.protocol_version, WEB_SHARE_PROTOCOL_VERSION);
    assert_eq!(decoded.capabilities, ["e2ee-token-auth"]);
}

#[test]
fn session_key_tokens_encode_to_terminal_bytes() {
    assert_eq!(encode_session_key("C-c").as_deref(), Some(&[0x03][..]));
    assert_eq!(encode_session_key("Enter").as_deref(), Some(&b"\r"[..]));
    assert_eq!(encode_session_key("S-Enter").as_deref(), Some(&b"\n"[..]));
    assert_eq!(encode_session_key("not-a-key"), None);
}

#[cfg(windows)]
#[test]
fn session_key_tokens_can_carry_windows_console_metadata() {
    let (bytes, key) =
        super::encode_session_windows_console_key("C-a").expect("C-a maps to console key");

    assert_eq!(bytes, vec![0x01]);
    assert_eq!(key.virtual_key_code(), b'A' as u16);
    assert_eq!(key.unicode_char(), 0x01);
    assert_eq!(key.control_key_state(), 0x0008);
}

#[test]
fn pane_scroll_wire_decodes_targeted_request() {
    let message = r#"{"type":"pane_scroll","pane_id":7,"delta":-3}"#;

    match serde_json::from_str::<ClientMessage>(message).expect("pane scroll decodes") {
        ClientMessage::PaneScroll { pane_id, delta } => {
            assert_eq!(pane_id, 7);
            assert_eq!(delta, -3);
        }
        _ => panic!("decoded wrong client message"),
    }
}

#[test]
fn select_pane_wire_decodes_targeted_request() {
    let message = r#"{"type":"select_pane","pane_id":7}"#;

    match serde_json::from_str::<ClientMessage>(message).expect("select pane decodes") {
        ClientMessage::SelectPane { pane_id } => assert_eq!(pane_id, 7),
        _ => panic!("decoded wrong client message"),
    }
}

#[test]
fn session_action_wire_decodes_window_controls() {
    assert!(matches!(
        serde_json::from_str::<ClientMessage>(r#"{"type":"split_pane","direction":"horizontal"}"#)
            .expect("split action decodes"),
        ClientMessage::SplitPane { .. }
    ));
    match serde_json::from_str::<ClientMessage>(
        r#"{"type":"rename_window","window_index":2,"name":"logs"}"#,
    )
    .expect("rename action decodes")
    {
        ClientMessage::RenameWindow { window_index, name } => {
            assert_eq!(window_index, 2);
            assert_eq!(name, "logs");
        }
        _ => panic!("decoded wrong client message"),
    }
}

#[test]
fn web_window_names_reject_control_bytes() {
    assert!(valid_window_name("logs"));
    assert!(!valid_window_name(""));
    assert!(!valid_window_name("bad\nname"));
    assert!(!valid_window_name(&"x".repeat(129)));
}

#[test]
fn resize_request_wire_uses_big_endian_size() {
    assert_eq!(
        parse_resize_body(&[0x00, 0x64, 0x00, 0x28]),
        Some(rmux_proto::TerminalSize {
            cols: 100,
            rows: 40
        })
    );
    assert_eq!(parse_resize_body(&[0x00, 0x00, 0x00, 0x28]), None);
    assert_eq!(parse_resize_body(&[0x00, 0x64]), None);
    assert_eq!(parse_resize_body(&[0x10, 0x01, 0x00, 0x28]), None);
    assert_eq!(parse_resize_body(&[0x04, 0x00, 0x04, 0x00]), None);
}

#[test]
fn pane_resize_wire_decodes_targeted_request() {
    assert_eq!(
        parse_pane_resize_body(&[0, 0, 0, 7, 1, 0, 5]),
        Some((7, ResizePaneAdjustment::Right { cells: 5 }))
    );
    assert_eq!(
        parse_pane_resize_body(&[0, 0, 0, 7, 2, 0, 3]),
        Some((7, ResizePaneAdjustment::Up { cells: 3 }))
    );
    assert_eq!(parse_pane_resize_body(&[0, 0, 0, 7, 9, 0, 5]), None);
    assert_eq!(parse_pane_resize_body(&[0, 0, 0, 7, 1, 0, 0]), None);
    assert_eq!(parse_pane_resize_body(&[0, 0, 0, 7, 1, 0x27, 0x11]), None);
    assert_eq!(parse_pane_resize_body(&[0, 0, 0, 7, 1, 0]), None);
}

#[test]
fn client_message_rejects_unknown_fields() {
    let message = r#"{"type":"pane_scroll","pane_id":7,"delta":-3,"role":"operator"}"#;

    assert!(serde_json::from_str::<ClientMessage>(message).is_err());
}
