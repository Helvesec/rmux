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

#[test]
fn pull_is_opt_in_and_uses_distinct_continuation_tags() {
    let offer = SourceFileInputV1Request::on_demand(vec!["source-file".into(), "x".into()], None);
    assert!(offer.requests_input_pull());
    assert_eq!(offer.stdin_payload, None);
    let old: Vec<_> = SUPPORTED_CAPABILITIES
        .iter()
        .copied()
        .filter(|id| *id != CAPABILITY_SOURCE_FILE_INPUT_PULL_V1)
        .collect();
    assert!(offer.handshake.validate_against(&old).is_err());
    assert!(!SourceFileInputV1Request::new(vec![], None, None).requests_input_pull());
    let read = Response::SourceFileReadStdinV1(SourceFileReadStdinV1Response);
    assert_eq!(frame_kind_for_response(&read).bincode_tag(), 113);
    assert_eq!(&encode_frame(&read).unwrap()[6..10], &113u32.to_le_bytes());
    for payload in [Ok(vec![]), Ok(vec![0, 255]), Err("read failed".into())] {
        let data = Request::SourceFileInputDataV1(SourceFileInputDataV1Request {
            data: payload.clone(),
        });
        assert_eq!(frame_kind_for_request(&data).bincode_tag(), 141);
        let frame = encode_frame(&data).unwrap();
        assert_eq!(&frame[6..10], &141u32.to_le_bytes());
        assert!(
            matches!(decode_frame(&frame), Ok(Request::SourceFileInputDataV1(decoded)) if decoded.data == payload)
        );
    }
}

#[test]
fn pull_reply_byte_limit_includes_the_frame_overhead() {
    let empty = Request::SourceFileInputDataV1(SourceFileInputDataV1Request {
        data: Ok(Vec::new()),
    });
    assert_eq!(
        bincode::serialized_size(&empty).unwrap() as usize
            + SourceFileInputDataV1Request::MAX_BYTES,
        DEFAULT_MAX_DETACHED_FRAME_LENGTH
    );
}

#[test]
fn source_stdin_relay_requires_opt_in_and_distinct_frames() {
    let mut offer = SourceFileInputV1Request::on_demand(vec![], None);
    assert!(!offer.requests_input_relay());
    offer
        .handshake
        .required_capabilities
        .push(CAPABILITY_SOURCE_FILE_INPUT_RELAY_V1.into());
    assert!(offer.requests_input_relay());
    let old = SUPPORTED_CAPABILITIES
        .iter()
        .copied()
        .filter(|c| *c != CAPABILITY_SOURCE_FILE_INPUT_RELAY_V1)
        .collect::<Vec<_>>();
    assert!(offer.handshake.validate_against(&old).is_err());
    let credentials = SourceFileRelayV1Response {
        ticket: [0xee; 16],
        proof: [0x11; 16],
    };
    let claim = Request::SourceFileRelayV1(SourceFileRelayV1Request {
        ticket: credentials.ticket,
    });
    let frame = encode_frame(&claim).unwrap();
    assert_eq!(&frame[6..10], &142u32.to_le_bytes());
    assert!(
        matches!(decode_frame(&frame), Ok(Request::SourceFileRelayV1(c)) if c.ticket == credentials.ticket)
    );
    let prompt = Response::SourceFileRelayReadyV1(SourceFileRelayReadyV1Response {
        proof: credentials.proof,
    });
    for (response, tag) in [
        (Response::SourceFileRelayV1(credentials), 114u32),
        (prompt, 115),
    ] {
        let frame = encode_frame(&response).unwrap();
        assert_eq!(&frame[6..10], &tag.to_le_bytes());
        assert_eq!(
            encode_frame(&decode_frame::<Response>(&frame).unwrap()).unwrap(),
            frame
        );
    }
}
