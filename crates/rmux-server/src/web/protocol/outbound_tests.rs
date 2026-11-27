use rmux_proto::TerminalSize;
use serde_json::json;

use super::{
    pane_snapshot_payload, session_keyframe_payloads, session_pane_frame_payload, PaneSize,
    ServerMessage, WebSessionPaneFrame, WebSessionSnapshot, SERVER_CAPABILITIES,
    WEB_OUTBOUND_BYTES_MAX, WEB_SHARE_PROTOCOL_VERSION, WS_PANE_RECOVERY_SNAPSHOT,
    WS_RESIZE_NOTIFY, WS_SESSION_PANE_FRAME, WS_SESSION_VIEW, WS_SNAPSHOT_FULL,
};
use crate::handler::{TestWebSessionView, WebPaneSnapshot, WebSessionPaneView};
use crate::pane_recovery::MAX_RECOVERY_KEYFRAME_BYTES;
use crate::web::protocol::{PANE_FRAME_CAPABILITY, PANE_RECOVERY_COVERAGE_CAPABILITY};
use crate::web::stream_sanitizer::WebTerminalSanitizer;
use crate::web::{WebShareConnectRole, WebShareConnectionCounts, WebShareRevokeReason};

#[test]
fn ready_message_wire_shape_is_v1_and_capability_gated() {
    let payload = ServerMessage::Ready {
        protocol_version: WEB_SHARE_PROTOCOL_VERSION,
        capabilities: SERVER_CAPABILITIES,
        pane_size: PaneSize { cols: 80, rows: 24 },
        scope: "session",
        share_id: "share-1",
        session_name: Some("dev"),
        role: "operator",
        operator: true,
        operator_access: true,
        spectator_access: false,
        controls: true,
        show_viewers: true,
        spectator_pairing_code: None,
        ttl_remaining_seconds: Some(30),
        connection_counts: WebShareConnectionCounts::new(2, Some(5), 1, Some(1)),
        terminal_palette: None,
    };

    let encoded = serde_json::to_value(payload).expect("ready payload serializes");

    assert_eq!(WEB_SHARE_PROTOCOL_VERSION, 1);
    assert_eq!(
        encoded,
        json!({
            "type": "ready",
            "protocol_version": 1,
            "capabilities": [
                "e2ee-token-auth",
                "terminal-palette-v1",
                PANE_FRAME_CAPABILITY,
                PANE_RECOVERY_COVERAGE_CAPABILITY
            ],
            "pane_size": { "cols": 80, "rows": 24 },
            "scope": "session",
            "share_id": "share-1",
            "session_name": "dev",
            "role": "operator",
            "operator": true,
            "operator_access": true,
            "spectator_access": false,
            "controls": true,
            "show_viewers": true,
            "ttl_remaining_seconds": 30,
            "spectators_active": 2,
            "spectators_max": 5,
            "operators_active": 1,
            "operators_max": 1,
            "viewers_connected": 3
        })
    );
}

#[test]
fn viewer_count_message_wire_shape_is_stable() {
    let payload = ServerMessage::ViewerCount {
        connection_counts: WebShareConnectionCounts::new(1, None, 2, Some(3)),
    };

    let encoded = serde_json::to_value(payload).expect("viewer count payload serializes");

    assert_eq!(
        encoded,
        json!({
            "type": "viewer_count",
            "spectators_active": 1,
            "operators_active": 2,
            "operators_max": 3,
            "viewers_connected": 3
        })
    );
}

#[test]
fn share_revoked_message_wire_shape_is_stable() {
    let payload = ServerMessage::ShareRevoked {
        reason: WebShareRevokeReason::TtlExpired.as_str(),
    };

    let encoded = serde_json::to_value(payload).expect("revoked payload serializes");

    assert_eq!(
        encoded,
        json!({
            "type": "share_revoked",
            "reason": "ttl_expired"
        })
    );
}

#[test]
fn session_keyframe_keeps_resize_snapshot_and_view_atomic_order() {
    let size = TerminalSize { cols: 80, rows: 24 };
    let snapshot =
        WebSessionSnapshot::new(size, b"paint".to_vec(), TestWebSessionView::new(size), 0, 0)
            .expect("snapshot fits");
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);

    let frames =
        session_keyframe_payloads(Some(size), &snapshot, &mut sanitizer).expect("view serializes");

    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0][0], WS_RESIZE_NOTIFY);
    assert_eq!(frames[1][0], WS_SNAPSHOT_FULL);
    assert_eq!(frames[2][0], WS_SESSION_VIEW);
}

#[test]
fn session_pane_frame_payload_uses_fixed_header_and_ansi_body() {
    let size = TerminalSize {
        cols: 120,
        rows: 40,
    };
    let frame = WebSessionPaneFrame::new(
        size,
        WebSessionPaneView {
            id: 7,
            x: 41,
            y: 2,
            cols: 39,
            rows: 20,
            active: true,
            history_size: 50_000,
            scroll_offset: 12,
            alternate_on: false,
            mouse_on: false,
        },
        b"\x1b[3;42Hrow".to_vec(),
    )
    .expect("pane frame fits");

    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);
    let payload = session_pane_frame_payload(&frame, &mut sanitizer).expect("pane frame fits");

    assert_eq!(payload[0], WS_SESSION_PANE_FRAME);
    assert_eq!(u32::from_be_bytes(payload[1..5].try_into().unwrap()), 7);
    assert_eq!(u16::from_be_bytes(payload[5..7].try_into().unwrap()), 120);
    assert_eq!(u16::from_be_bytes(payload[7..9].try_into().unwrap()), 40);
    assert_eq!(u16::from_be_bytes(payload[9..11].try_into().unwrap()), 41);
    assert_eq!(u16::from_be_bytes(payload[11..13].try_into().unwrap()), 2);
    assert_eq!(u16::from_be_bytes(payload[13..15].try_into().unwrap()), 39);
    assert_eq!(u16::from_be_bytes(payload[15..17].try_into().unwrap()), 20);
    assert_eq!(u32::from_be_bytes(payload[17..21].try_into().unwrap()), 12);
    assert_eq!(
        u32::from_be_bytes(payload[21..25].try_into().unwrap()),
        50_000
    );
    assert_eq!(&payload[25..], b"\x1b[3;42Hrow");
}

#[test]
fn session_keyframes_and_pane_frames_apply_the_web_terminal_policy() {
    let size = TerminalSize { cols: 80, rows: 24 };
    let unsafe_link = b"before\x1b\r]8;;javascript:alert(1)\x1b\\link\x1b]8;;\x1b\\after";
    let safe_link = b"before\r\x1b]8;;\x1b\\link\x1b]8;;\x1b\\after";
    let snapshot = WebSessionSnapshot::new(
        size,
        unsafe_link.to_vec(),
        TestWebSessionView::new(size),
        0,
        0,
    )
    .expect("snapshot fits");
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);
    let frames =
        session_keyframe_payloads(None, &snapshot, &mut sanitizer).expect("view serializes");
    assert!(
        !frames[0]
            .windows(b"javascript:".len())
            .any(|window| window == b"javascript:"),
        "session recovery frame must not retain an active-content URI"
    );
    assert!(frames[0].ends_with(safe_link));

    let pane_frame = WebSessionPaneFrame::new(
        size,
        WebSessionPaneView {
            id: 7,
            x: 0,
            y: 0,
            cols: 80,
            rows: 24,
            active: true,
            history_size: 0,
            scroll_offset: 0,
            alternate_on: false,
            mouse_on: false,
        },
        unsafe_link.to_vec(),
    )
    .expect("pane frame fits");
    let payload = session_pane_frame_payload(&pane_frame, &mut sanitizer).expect("pane frame fits");
    assert_eq!(&payload[25..], safe_link);

    let pane_snapshot = WebPaneSnapshot {
        cols: 80,
        rows: 24,
        output_sequence: 0,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 23,
        history_rows_total: 0,
        history_rows_included: 0,
        metadata_complete: true,
        recovery_keyframe: Some(unsafe_link.to_vec()),
    };
    let payload =
        pane_snapshot_payload(&pane_snapshot, &mut sanitizer, false).expect("snapshot fits");
    assert_eq!(&payload[1..], safe_link);
}

#[test]
fn pane_and_session_recovery_to_live_paths_close_rejected_hyperlinks() {
    let old_link = b"\x1b]8;;https://old.example\x1b\\OLD";
    let rejected_and_text = b"\x1b]8;;file:///etc/passwd\x1b\\NEXT\x1b]8;;\x1b\\END";
    let safe_live = b"\x1b]8;;\x1b\\NEXT\x1b]8;;\x1b\\END";
    let pane_snapshot = WebPaneSnapshot {
        cols: 80,
        rows: 24,
        output_sequence: 0,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 23,
        history_rows_total: 0,
        history_rows_included: 0,
        metadata_complete: true,
        recovery_keyframe: Some(old_link.to_vec()),
    };
    let mut pane_sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);
    let pane_recovery = pane_snapshot_payload(&pane_snapshot, &mut pane_sanitizer, false)
        .expect("pane snapshot fits");
    assert_eq!(&pane_recovery[1..], old_link);
    let mut pane_live = Vec::new();
    for chunk in rejected_and_text.chunks(1) {
        pane_sanitizer.push(chunk, &mut pane_live);
    }
    assert_eq!(pane_live, safe_live);

    let size = TerminalSize { cols: 80, rows: 24 };
    let session_snapshot =
        WebSessionSnapshot::new(size, old_link.to_vec(), TestWebSessionView::new(size), 0, 0)
            .expect("session snapshot fits");
    let mut session_sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Spectator);
    let frames = session_keyframe_payloads(None, &session_snapshot, &mut session_sanitizer)
        .expect("session snapshot fits");
    assert!(frames[0].ends_with(old_link));
    let mut session_live = Vec::new();
    for chunk in rejected_and_text.chunks(1) {
        session_sanitizer.push(chunk, &mut session_live);
    }
    assert_eq!(session_live, safe_live);
}

#[test]
fn pane_recovery_keyframe_and_live_tail_share_one_sanitizer_state() {
    let snapshot = WebPaneSnapshot {
        cols: 8,
        rows: 2,
        output_sequence: 4,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 1,
        history_rows_total: 7,
        history_rows_included: 3,
        metadata_complete: false,
        recovery_keyframe: Some(b"safe\x1b".to_vec()),
    };
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Spectator);
    let payload = pane_snapshot_payload(&snapshot, &mut sanitizer, false).expect("snapshot fits");
    assert_eq!(&payload[1..], b"safe");

    let mut c0 = Vec::new();
    sanitizer.push(b"\r", &mut c0);
    assert_eq!(c0, b"\r");

    let mut live = Vec::new();
    sanitizer.push(b"]52;c;Zm9vYmFy\x07after", &mut live);
    assert_eq!(
        live, b"after",
        "ESC and C0 frames must not expose a following OSC 52"
    );
}

#[test]
fn session_recovery_keyframe_and_live_tail_share_one_sanitizer_state() {
    let size = TerminalSize { cols: 80, rows: 24 };
    let snapshot = WebSessionSnapshot::new(
        size,
        b"safe\x1b".to_vec(),
        TestWebSessionView::new(size),
        0,
        0,
    )
    .expect("snapshot fits");
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Spectator);
    let frames =
        session_keyframe_payloads(None, &snapshot, &mut sanitizer).expect("view serializes");
    assert!(frames[0].ends_with(b"safe"));

    let mut c0 = Vec::new();
    sanitizer.push(b"\r", &mut c0);
    assert_eq!(c0, b"\r");

    let mut live = Vec::new();
    sanitizer.push(b"]52;c;Zm9vYmFy\x1b\\after", &mut live);
    assert_eq!(
        live, b"after",
        "session ESC and C0 frames must not expose a following OSC 52"
    );
}

#[test]
fn dcs_passthrough_state_survives_the_recovery_live_boundary() {
    let snapshot = WebPaneSnapshot {
        cols: 8,
        rows: 2,
        output_sequence: 4,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 1,
        history_rows_total: 7,
        history_rows_included: 3,
        metadata_complete: false,
        recovery_keyframe: Some(b"safe\x1bPq".to_vec()),
    };
    let mut sanitizer = WebTerminalSanitizer::default();
    let payload = pane_snapshot_payload(&snapshot, &mut sanitizer, false).expect("snapshot fits");
    assert_eq!(&payload[1..], b"safe");

    let mut live = Vec::new();
    sanitizer.push(b"\x18HIDDEN\x1b\\after", &mut live);
    assert_eq!(
        live, b"after",
        "DCS passthrough controls must remain payload across recovery/live frames"
    );
}

#[test]
fn a_bare_c1_byte_in_the_live_tail_does_not_stop_the_viewer_stream() {
    let snapshot = WebPaneSnapshot {
        cols: 8,
        rows: 2,
        output_sequence: 4,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 1,
        history_rows_total: 7,
        history_rows_included: 3,
        metadata_complete: false,
        recovery_keyframe: Some(b"$ cat logo.bin\r\n".to_vec()),
    };
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);
    let payload = pane_snapshot_payload(&snapshot, &mut sanitizer, false).expect("snapshot fits");
    assert_eq!(&payload[1..], b"$ cat logo.bin\r\n");

    // The binary file's first bytes, then the shell prompt again. Every
    // later frame used to be discarded by the sanitizer.
    let mut binary = Vec::new();
    sanitizer.push(b"\x9f\x8a\x00PNG\r\n$ ", &mut binary);
    assert_eq!(binary, "\u{fffd}\u{fffd}\0PNG\r\n$ ".as_bytes());

    let mut next = Vec::new();
    sanitizer.push(b"echo hello\r\nhello\r\n$ ", &mut next);
    assert_eq!(next, b"echo hello\r\nhello\r\n$ ");
}

#[test]
fn pane_recovery_coverage_is_negotiated_and_atomic_with_the_snapshot() {
    let snapshot = WebPaneSnapshot {
        cols: 8,
        rows: 2,
        output_sequence: 4,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 1,
        history_rows_total: 50,
        history_rows_included: 12,
        metadata_complete: false,
        recovery_keyframe: Some(b"screen".to_vec()),
    };
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);
    let legacy = pane_snapshot_payload(&snapshot, &mut sanitizer, false).expect("snapshot fits");
    assert_eq!(legacy, b"\x10screen");

    let covered = pane_snapshot_payload(&snapshot, &mut sanitizer, true).expect("snapshot fits");
    assert_eq!(covered[0], WS_PANE_RECOVERY_SNAPSHOT);
    assert_eq!(
        u64::from_be_bytes(covered[1..9].try_into().expect("total row bytes")),
        50
    );
    assert_eq!(
        u64::from_be_bytes(covered[9..17].try_into().expect("included row bytes")),
        12
    );
    assert_eq!(covered[17], 0);
    assert_eq!(&covered[18..], b"screen");
}

#[test]
fn web_payload_builders_admit_maximum_recovery_and_reject_oversized_frames() {
    let mut pane_snapshot = WebPaneSnapshot {
        cols: 8,
        rows: 2,
        output_sequence: 0,
        ansi_lines: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        mode_bits: 0,
        cursor_style: 0,
        alternate: false,
        scroll_top: 0,
        scroll_bottom: 1,
        history_rows_total: 0,
        history_rows_included: 0,
        metadata_complete: true,
        recovery_keyframe: Some(vec![b'x'; MAX_RECOVERY_KEYFRAME_BYTES]),
    };
    let mut sanitizer = WebTerminalSanitizer::for_role(WebShareConnectRole::Operator);
    assert!(
        pane_snapshot_payload(&pane_snapshot, &mut sanitizer, true).is_some(),
        "the largest accepted recovery keyframe must fit with Web coverage metadata"
    );

    pane_snapshot.recovery_keyframe = Some(vec![b'x'; WEB_OUTBOUND_BYTES_MAX]);
    assert!(pane_snapshot_payload(&pane_snapshot, &mut sanitizer, false).is_none());

    let size = TerminalSize { cols: 80, rows: 24 };
    let pane = WebSessionPaneView {
        id: 7,
        x: 0,
        y: 0,
        cols: 1,
        rows: 1,
        active: true,
        history_size: 0,
        scroll_offset: 0,
        alternate_on: false,
        mouse_on: false,
    };
    let view = TestWebSessionView {
        scene: None,
        size,
        panes: vec![pane; 8_000],
        windows: Vec::new(),
        metadata_complete: false,
    };
    let snapshot = WebSessionSnapshot::new(
        size,
        vec![b'x'; crate::web::WEB_RECOVERY_CONTENT_BYTES_MAX],
        view,
        0,
        0,
    )
    .expect("session content alone fits its stricter cap");
    assert!(session_keyframe_payloads(None, &snapshot, &mut sanitizer).is_none());
}
