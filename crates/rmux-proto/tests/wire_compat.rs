use rmux_proto::{decode_frame, Request, Response, RmuxError, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION};

const HAS_SESSION_REQUEST_V1: &str =
    include_str!("../../../tests/reference/wire/v1/has_session_request.hex");
const NEW_SESSION_RESPONSE_V1: &str =
    include_str!("../../../tests/reference/wire/v1/new_session_response.hex");
const HAS_SESSION_REQUEST_V2: &str =
    include_str!("../../../tests/reference/wire/v2/has_session_request.hex");
const NEW_SESSION_RESPONSE_V2: &str =
    include_str!("../../../tests/reference/wire/v2/new_session_response.hex");

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
