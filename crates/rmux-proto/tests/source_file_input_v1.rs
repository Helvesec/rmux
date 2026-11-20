use rmux_proto::*;

#[test]
fn source_input_is_additive_binary_and_independently_negotiated() {
    let offer = SourceFileInputV1Request::new(
        vec!["source-file".into(), "input.rmux".into()],
        None,
        Some(vec![0, 255, 27, 10, 13]),
    );
    assert_eq!(
        offer.handshake.required_capabilities,
        [
            CAPABILITY_SOURCE_FILE_INPUT_V1,
            CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
            CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1
        ]
    );
    for required in &offer.handshake.required_capabilities {
        assert!(SUPPORTED_CAPABILITIES.contains(&required.as_str()));
    }
    let request = Request::SourceFileInputV1(Box::new(offer));
    let frame = encode_frame(&request).unwrap();
    assert_eq!(&frame[6..10], &140u32.to_le_bytes());
    assert_eq!(frame_kind_for_request(&request).bincode_tag(), 140);
    let decoded: Request = decode_frame(&frame).unwrap();
    assert_eq!(
        bincode::serialize(&decoded).unwrap(),
        bincode::serialize(&request).unwrap()
    );
    let Request::SourceFileInputV1(decoded) = decoded else {
        panic!("source input tag")
    };
    assert_eq!(decoded.stdin_payload, Some(vec![0, 255, 27, 10, 13]));
    let old = include_bytes!(
        "../../../tests/wire-fixtures/ledger-v1-current-wire/new_pane_input_v1_request.bin"
    );
    let original: Request = decode_frame(old).unwrap();
    assert!(matches!(original, Request::NewPaneInputV1(_)));
    assert_eq!(encode_frame(&original).unwrap(), old);
}

#[test]
fn source_input_eof_and_absence_are_distinct_and_the_whole_frame_is_bounded() {
    let eof = SourceFileInputV1Request::new(vec!["source-file".into()], None, Some(Vec::new()));
    let mut absent = eof.clone();
    absent.stdin_payload = None;
    assert_ne!(
        bincode::serialize(&eof).unwrap(),
        bincode::serialize(&absent).unwrap()
    );
    let mut oversized = eof;
    oversized.stdin_payload = Some(vec![0; DEFAULT_MAX_DETACHED_FRAME_LENGTH]);
    assert!(matches!(
        encode_frame(&Request::SourceFileInputV1(Box::new(oversized))),
        Err(RmuxError::FrameTooLarge { .. })
    ));
}

#[test]
fn queue_status_is_opt_in_and_has_an_additive_response_tag() {
    let original = SourceFileInputV1Request::new(
        vec!["source-file".into(), "missing".into()],
        None,
        Some(vec![]),
    );
    let offer = original.clone().requiring_queue_status();
    assert!(!original
        .handshake
        .required_capabilities
        .iter()
        .any(|c| c == CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1));
    assert!(offer
        .handshake
        .required_capabilities
        .iter()
        .any(|c| c == CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1));
    assert!(offer
        .handshake
        .validate_against(SUPPORTED_CAPABILITIES)
        .is_ok());
    let old_capabilities: Vec<_> = SUPPORTED_CAPABILITIES
        .iter()
        .copied()
        .filter(|c| *c != CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1)
        .collect();
    assert!(offer.handshake.validate_against(&old_capabilities).is_err());
    assert!(original
        .handshake
        .validate_against(&old_capabilities)
        .is_ok());
    for stop_queue in [false, true] {
        let response = Response::SourceFileQueueV1(Box::new(SourceFileQueueV1Response {
            stop_queue,
            source: SourceFileResponse::from_output(CommandOutput::from_stdout(vec![0, 255, 10]))
                .with_stderr(vec![255, 0, 10])
                .with_exit_status(Some(1)),
        }));
        let encoded = encode_frame(&response).unwrap();
        assert_eq!(&encoded[6..10], &112_u32.to_le_bytes());
        assert_eq!(frame_kind_for_response(&response).bincode_tag(), 112);
        let decoded: Response = decode_frame(&encoded).unwrap();
        assert_eq!(encode_frame(&decoded).unwrap(), encoded);
        let Response::SourceFileQueueV1(decoded) = decoded else {
            panic!("queue status response")
        };
        assert_eq!(decoded.stop_queue, stop_queue);
        assert_eq!(decoded.source.stderr(), &[255, 0, 10]);
    }
}
