use rmux_proto::{
    decode_frame, encode_frame, DisplayMessageExtRequest, Request, Response, RmuxError,
    SessionName, Target, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION,
};
use serde::Serialize;

const HAS_SESSION_REQUEST_V1: &str =
    include_str!("../../../tests/reference/wire/v1/has_session_request.hex");
const NEW_SESSION_RESPONSE_V1: &str =
    include_str!("../../../tests/reference/wire/v1/new_session_response.hex");
const HAS_SESSION_REQUEST_V2: &str =
    include_str!("../../../tests/reference/wire/v2/has_session_request.hex");
const NEW_SESSION_RESPONSE_V2: &str =
    include_str!("../../../tests/reference/wire/v2/new_session_response.hex");
const CAPTURE_PANE_REQUEST_V4: &[u8] = include_bytes!(
    "../../../tests/reference/wire/v4/ledger-v1-current-wire/capture_pane_request.bin"
);
const SPLIT_WINDOW_IDENTITY_REQUEST_V5: &[u8] = include_bytes!(
    "../../../tests/reference/wire/v5/ledger-v1-current-wire/split_window_identity_request.bin"
);
const SUBSCRIBE_PANE_STREAM_REQUEST_V6: &[u8] = include_bytes!(
    "../../../tests/reference/wire/v6/ledger-v1-current-wire/subscribe_pane_stream_request.bin"
);
const PANE_STREAM_CURSOR_RESPONSE_V7: &[u8] = include_bytes!(
    "../../../tests/reference/wire/v7/ledger-v1-current-wire/pane_stream_cursor_response.bin"
);

#[test]
fn v1_has_session_request_fixture_is_rejected_by_current_wire() {
    let bytes = decode_hex(HAS_SESSION_REQUEST_V1);
    assert_wire_envelope(&bytes, 1);

    assert_wire_is_unsupported(decode_frame::<Request>(&bytes), 1);
}

#[test]
fn v1_new_session_response_fixture_is_rejected_by_current_wire() {
    let bytes = decode_hex(NEW_SESSION_RESPONSE_V1);
    assert_wire_envelope(&bytes, 1);

    assert_wire_is_unsupported(decode_frame::<Response>(&bytes), 1);
}

#[test]
fn v2_has_session_request_fixture_is_rejected_by_current_wire() {
    let bytes = decode_hex(HAS_SESSION_REQUEST_V2);
    assert_wire_envelope(&bytes, 2);

    assert_wire_is_unsupported(decode_frame::<Request>(&bytes), 2);
}

#[test]
fn v2_new_session_response_fixture_is_rejected_by_current_wire() {
    let bytes = decode_hex(NEW_SESSION_RESPONSE_V2);
    assert_wire_envelope(&bytes, 2);

    assert_wire_is_unsupported(decode_frame::<Response>(&bytes), 2);
}

#[test]
fn distributed_v4_capture_pane_fixture_is_rejected_by_current_wire() {
    assert_wire_envelope(CAPTURE_PANE_REQUEST_V4, 4);
    assert_wire_is_unsupported(decode_frame::<Request>(CAPTURE_PANE_REQUEST_V4), 4);
}

#[test]
fn last_distributed_v5_split_identity_fixture_is_rejected_by_current_wire() {
    assert_wire_envelope(SPLIT_WINDOW_IDENTITY_REQUEST_V5, 5);
    assert_wire_is_unsupported(decode_frame::<Request>(SPLIT_WINDOW_IDENTITY_REQUEST_V5), 5);
}

#[test]
fn original_v6_pane_stream_fixture_is_rejected_by_v8() {
    assert_wire_envelope(SUBSCRIBE_PANE_STREAM_REQUEST_V6, 6);
    assert_wire_is_unsupported(decode_frame::<Request>(SUBSCRIBE_PANE_STREAM_REQUEST_V6), 6);
}

#[test]
fn original_v7_surface_fixture_is_rejected_by_v8() {
    assert_wire_envelope(PANE_STREAM_CURSOR_RESPONSE_V7, 7);
    assert_wire_is_unsupported(decode_frame::<Response>(PANE_STREAM_CURSOR_RESPONSE_V7), 7);
}

#[derive(Serialize)]
struct UnreleasedV6DisplayMessageExtRequest {
    target: Option<Target>,
    print: bool,
    message: Option<String>,
    target_client: Option<String>,
    empty_target_context: bool,
}

#[test]
fn unreleased_v6_display_message_layout_is_rejected_after_v8_cut() {
    assert_eq!(RMUX_WIRE_VERSION, 8);
    let target = Some(Target::Session(
        SessionName::new("alpha").expect("valid session name"),
    ));
    let current = encode_frame(&Request::DisplayMessageExt(Box::new(
        DisplayMessageExtRequest {
            target: target.clone(),
            print: false,
            message: Some("#{client_name}".to_owned()),
            target_client: Some("client".to_owned()),
            empty_target_context: false,
            duration_ms: None,
            ignore_input: false,
        },
    )))
    .expect("current display-message request encodes");
    let request_tag = current
        .get(6..10)
        .expect("current request carries a four-byte bincode tag");
    let mut payload = request_tag.to_vec();
    payload.extend(
        bincode::serialize(&UnreleasedV6DisplayMessageExtRequest {
            target,
            print: false,
            message: Some("#{client_name}".to_owned()),
            target_client: Some("client".to_owned()),
            empty_target_context: false,
        })
        .expect("unreleased v6 request serializes"),
    );
    let mut frame = vec![RMUX_FRAME_MAGIC, 8];
    frame.extend_from_slice(
        &u32::try_from(payload.len())
            .expect("test payload fits u32")
            .to_le_bytes(),
    );
    frame.extend_from_slice(&payload);

    assert!(
        matches!(decode_frame::<Request>(&frame), Err(RmuxError::Decode(_))),
        "the pre-controls payload must not be accepted under the current envelope"
    );
    frame[1] = 6;
    assert_wire_is_unsupported(decode_frame::<Request>(&frame), 6);
}

#[test]
fn complete_last_distributed_v4_fixture_ledger_is_preserved() {
    assert_complete_fixture_ledger(4, 23);
}

#[test]
fn complete_last_distributed_v5_fixture_ledger_is_preserved() {
    assert_complete_fixture_ledger(5, 25);
}

#[test]
fn complete_original_v6_fixture_ledger_is_preserved() {
    assert_complete_fixture_ledger(6, 31);
}

#[test]
fn complete_original_v7_fixture_ledger_is_preserved() {
    assert_complete_fixture_ledger(7, 31);
}

fn assert_complete_fixture_ledger(version: u32, expected_count: usize) {
    let fixture_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("reference")
        .join("wire")
        .join(format!("v{version}"))
        .join("ledger-v1-current-wire");
    let mut fixture_count = 0;
    for entry in std::fs::read_dir(&fixture_root).expect("read preserved fixture ledger") {
        let path = entry.expect("read fixture entry").path();
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("bin") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read preserved fixture");
        assert_wire_envelope(&bytes, version);
        fixture_count += 1;
    }
    assert_eq!(
        fixture_count, expected_count,
        "complete v{version} fixture ledger must remain frozen"
    );
}

fn assert_wire_envelope(bytes: &[u8], version: u32) {
    assert_eq!(bytes.first().copied(), Some(RMUX_FRAME_MAGIC));
    assert_eq!(bytes.get(1).copied(), Some(version as u8));
}

fn assert_wire_is_unsupported<T>(result: Result<T, RmuxError>, version: u32) {
    assert!(matches!(
        result,
        Err(RmuxError::UnsupportedWireVersion {
            got,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        }) if got == version
    ));
}

fn decode_hex(text: &str) -> Vec<u8> {
    let text = text.trim();
    assert_eq!(text.len() % 2, 0, "hex fixture length must be even");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("valid hex byte"))
        .collect()
}
