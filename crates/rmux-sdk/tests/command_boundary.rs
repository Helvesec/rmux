use rmux_sdk::{RmuxCommand, RmuxCommandKind};
use serde_json::Value;

#[test]
fn typed_commands_keep_exactbase_json_and_bincode() {
    // Captured with 424c154ccc9cd40751b237fdd23f60c8117f5dd8, bincode 1.3.3.
    // Four sparse/default and four fully populated values; never regenerate
    // these expectations from the candidate implementation.
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/command-boundary/values.json")).unwrap();
    for (index, case) in cases.iter().enumerate() {
        let hex = case["bincode_hex"].as_str().unwrap();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let command: RmuxCommand = serde_json::from_value(case["value"].clone()).unwrap();
        assert_eq!(serde_json::to_value(&command).unwrap(), case["value"]);
        assert_eq!(bincode::serialize(&command).unwrap(), bytes);
        assert_eq!(
            bincode::deserialize::<RmuxCommand>(&bytes).unwrap(),
            command
        );

        let tag = (index % 4 + 1) as u32;
        let kind_bytes = bincode::serialize(&command.command).unwrap();
        assert_eq!(&kind_bytes[..4], &tag.to_le_bytes());
        assert_eq!(
            bincode::deserialize::<RmuxCommandKind>(&kind_bytes).unwrap(),
            command.command
        );
        assert_eq!(
            serde_json::to_value(&command.command).unwrap(),
            case["value"]["command"]
        );
        assert_eq!(
            command.command_name(),
            [
                "new-session",
                "attach-session",
                "split-window",
                "refresh-client"
            ][index % 4]
        );
    }
}

#[test]
fn retired_raw_input_is_rejected_before_reading_its_payload() {
    // A valid old raw request, an empty payload and a typed-looking payload
    // must all fail. Rejection is based on the outer SDK tag, never its contents.
    for raw in [
        r#"{"Request":{"ListSessions":{"format":null}}}"#,
        r#"{"Request":null}"#,
        r#"{"Request":{"NewSession":{}}}"#,
    ] {
        assert!(serde_json::from_str::<RmuxCommandKind>(raw).is_err());
        let wrapped = format!(r#"{{"command":{raw}}}"#);
        assert!(serde_json::from_str::<RmuxCommand>(&wrapped).is_err());
    }
    for tail in [
        vec![],
        bincode::serialize(&rmux_proto::Request::ListSessions(
            rmux_proto::ListSessionsRequest {
                format: None,
                filter: None,
                sort_order: None,
                reversed: false,
            },
        ))
        .unwrap(),
        vec![0; 64],
    ] {
        let mut bytes = 0_u32.to_le_bytes().to_vec();
        bytes.extend(tail);
        let error = bincode::deserialize::<RmuxCommandKind>(&bytes).unwrap_err();
        assert!(error
            .to_string()
            .contains("raw Request SDK commands were removed"));
        let wrapped = [0_u32.to_le_bytes().as_slice(), &bytes].concat();
        let error = bincode::deserialize::<RmuxCommand>(&wrapped).unwrap_err();
        assert!(error
            .to_string()
            .contains("raw Request SDK commands were removed"));
    }
    assert!(bincode::deserialize::<RmuxCommandKind>(&5_u32.to_le_bytes()).is_err());
    assert!(serde_json::from_str::<RmuxCommandKind>(r#"{"Unknown":{}}"#).is_err());
}
