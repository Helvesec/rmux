use rmux_proto::{
    decode_frame, encode_frame, frame_kind_for_request, Request, RmuxError,
    RuntimeCommandExecutionV2Request, RuntimeCommandIntent, RMUX_WIRE_VERSION,
};

#[test]
fn v2_wire_carries_each_intent_and_preserves_literal_arguments() {
    for (tag, intent) in [
        RuntimeCommandIntent::GenericCli,
        RuntimeCommandIntent::NewFloatingPane,
        RuntimeCommandIntent::MoveFloatingPane,
        RuntimeCommandIntent::SetPanePlacement,
    ]
    .into_iter()
    .enumerate()
    {
        let request =
            Request::RuntimeCommandExecutionV2(Box::new(RuntimeCommandExecutionV2Request::new(
                vec![
                    "probe".to_owned(),
                    String::new(),
                    "space ; $HOME\n\t\\".to_owned(),
                ],
                Some("/client/cwd".into()),
                intent,
            )));
        let bytes = encode_frame(&request).unwrap();
        assert_eq!(RMUX_WIRE_VERSION, 9);
        assert_eq!(frame_kind_for_request(&request).bincode_tag(), 135);
        assert_eq!(&bytes[6..10], &135_u32.to_le_bytes());
        assert_eq!(
            &bytes[bytes.len() - 4..],
            &u32::try_from(tag).unwrap().to_le_bytes()
        );
        assert_eq!(decode_frame::<Request>(&bytes).unwrap(), request);
    }
}

#[test]
fn v2_missing_or_unknown_intent_is_rejected_by_the_decoder() {
    let fixture = include_bytes!("../../../tests/wire-fixtures/ledger-v1-current-wire/runtime_command_execution_v2_request.bin");
    for (remove, intent) in [(false, 4_u32), (true, 0_u32)] {
        let mut bytes = fixture.to_vec();
        bytes.truncate(bytes.len() - 4);
        if !remove {
            bytes.extend_from_slice(&intent.to_le_bytes());
        }
        let length = u32::try_from(bytes.len() - 6).unwrap();
        bytes[2..6].copy_from_slice(&length.to_le_bytes());
        assert!(matches!(
            decode_frame::<Request>(&bytes),
            Err(RmuxError::Decode(_))
        ));
    }
}

#[test]
fn v2_reference_entry_matches_the_additive_current_fixture() {
    assert_eq!(
        include_bytes!("../../../tests/reference/wire/v9/runtime_command_execution_v2_request.bin"),
        include_bytes!("../../../tests/wire-fixtures/ledger-v1-current-wire/runtime_command_execution_v2_request.bin"),
    );
}
