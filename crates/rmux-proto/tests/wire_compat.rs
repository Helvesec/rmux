use rmux_proto::{decode_frame, Request, Response, RmuxError, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION};

const HAS_SESSION_REQUEST_V1: &str =
    include_str!("../../../tests/reference/wire/v1/has_session_request.hex");
const NEW_SESSION_RESPONSE_V1: &str =
    include_str!("../../../tests/reference/wire/v1/new_session_response.hex");

#[test]
fn v1_has_session_request_fixture_is_rejected_after_wire_v2_bump() {
    let bytes = decode_hex(HAS_SESSION_REQUEST_V1);
    assert_v1_envelope(&bytes);

    assert_v1_is_unsupported(decode_frame::<Request>(&bytes));
}

#[test]
fn v1_new_session_response_fixture_is_rejected_after_wire_v2_bump() {
    let bytes = decode_hex(NEW_SESSION_RESPONSE_V1);
    assert_v1_envelope(&bytes);

    assert_v1_is_unsupported(decode_frame::<Response>(&bytes));
}

fn assert_v1_envelope(bytes: &[u8]) {
    assert_eq!(bytes.first().copied(), Some(RMUX_FRAME_MAGIC));
    assert_eq!(bytes.get(1).copied(), Some(1));
}

fn assert_v1_is_unsupported<T>(result: Result<T, RmuxError>) {
    assert!(matches!(
        result,
        Err(RmuxError::UnsupportedWireVersion {
            got: 1,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        })
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
