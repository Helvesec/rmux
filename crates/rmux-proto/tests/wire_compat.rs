use rmux_proto::{
    decode_frame, HasSessionRequest, NewSessionResponse, Request, Response, SessionName,
    RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION,
};

const HAS_SESSION_REQUEST_V1: &str =
    include_str!("../../../tests/reference/wire/v1/has_session_request.hex");
const NEW_SESSION_RESPONSE_V1: &str =
    include_str!("../../../tests/reference/wire/v1/new_session_response.hex");

#[test]
fn v1_has_session_request_fixture_decodes() {
    let bytes = decode_hex(HAS_SESSION_REQUEST_V1);
    assert_v1_envelope(&bytes);

    let decoded: Request = decode_frame(&bytes).expect("v1 request decodes");
    assert_eq!(
        decoded,
        Request::HasSession(HasSessionRequest {
            target: SessionName::new("alpha").expect("valid session"),
        })
    );
}

#[test]
fn v1_new_session_response_fixture_decodes() {
    let bytes = decode_hex(NEW_SESSION_RESPONSE_V1);
    assert_v1_envelope(&bytes);

    let decoded: Response = decode_frame(&bytes).expect("v1 response decodes");
    assert_eq!(
        decoded,
        Response::NewSession(NewSessionResponse {
            session_name: SessionName::new("alpha").expect("valid session"),
            detached: true,
            output: None,
        })
    );
}

fn assert_v1_envelope(bytes: &[u8]) {
    assert_eq!(bytes.first().copied(), Some(RMUX_FRAME_MAGIC));
    assert_eq!(bytes.get(1).copied(), Some(RMUX_WIRE_VERSION as u8));
}

fn decode_hex(text: &str) -> Vec<u8> {
    let text = text.trim();
    assert_eq!(text.len() % 2, 0, "hex fixture length must be even");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("valid hex byte"))
        .collect()
}
