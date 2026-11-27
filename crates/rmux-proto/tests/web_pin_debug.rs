use rmux_proto::{
    CommandOutput, CreateWebShareRequest, WebShareCreatedResponse, WebShareRequest,
    WebShareResponse, WebShareScope,
};

fn request() -> CreateWebShareRequest {
    serde_json::from_value(serde_json::json!({
        "scope": {"session": "demo"},
        "operator_pin": "193847",
        "spectator_pin": "826591",
    }))
    .expect("creation request")
}

fn assert_redacted(value: &impl std::fmt::Debug) {
    for diagnostic in [format!("{value:?}"), format!("{value:#?}")] {
        for pin in ["193847", "826591"] {
            assert!(!diagnostic.contains(pin), "Debug must redact creation PINs");
            assert!(
                !diagnostic.contains(&format!("{:?}", pin.as_bytes())),
                "Debug must redact PIN bytes in creation stdout"
            );
        }
    }
}

#[test]
fn creation_request_debug_redacts_pins_without_changing_wire_fields() {
    let request = request();
    let bytes = bincode::serialize(&request).expect("request wire");
    let decoded: CreateWebShareRequest = bincode::deserialize(&bytes).expect("request roundtrip");
    assert_eq!(decoded, request);
    assert_eq!(decoded.operator_pin.as_deref(), Some("193847"));
    assert_eq!(decoded.spectator_pin.as_deref(), Some("826591"));
    assert_redacted(&request);
    assert_redacted(&WebShareRequest::Create(request));
}

#[test]
fn creation_response_debug_redacts_pins_and_stdout_without_changing_cli_output() {
    let stdout = b"operator pin 193847\nspectator pin 826591\n".to_vec();
    let response = WebShareCreatedResponse {
        share_id: "debug-pin-canary".to_owned(),
        scope: WebShareScope::Session("demo".parse().expect("session name")),
        spectator_url: None,
        operator_url: None,
        tunnel_provider: None,
        tunnel_public_url: None,
        expires_at_unix: None,
        operator_pairing_code: Some("193847".to_owned()),
        spectator_pairing_code: Some("826591".to_owned()),
        max_spectators: None,
        max_operators: None,
        operator: true,
        spectator: true,
        controls: true,
        kill_session_on_expire: false,
        output: CommandOutput::from_stdout(stdout.clone()),
    };
    let bytes = bincode::serialize(&response).expect("response wire");
    let decoded: WebShareCreatedResponse =
        bincode::deserialize(&bytes).expect("response roundtrip");
    assert_eq!(decoded, response);
    assert_eq!(decoded.output.stdout(), stdout);
    assert_eq!(decoded.operator_pairing_code.as_deref(), Some("193847"));
    assert_eq!(decoded.spectator_pairing_code.as_deref(), Some("826591"));
    assert_redacted(&response);
    assert_redacted(&WebShareResponse::Created(response));
}

#[test]
fn creation_response_debug_redacts_stdout_pin_bytes_without_separate_pin_fields() {
    let response: WebShareCreatedResponse = serde_json::from_value(serde_json::json!({
        "share_id": "debug-pin-canary",
        "scope": {"session": "demo"},
        "operator": true,
        "spectator": true,
        "controls": true,
        "kill_session_on_expire": false,
        "output": {"stdout": b"826591"},
    }))
    .expect("creation response with CLI payload");
    assert_eq!(response.output.stdout(), b"826591");
    assert_redacted(&response);
    assert_redacted(&WebShareResponse::Created(response));
}
