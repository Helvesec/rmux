//! V1 frame ledger and current-envelope binary fixture regression tests.
//!
//! These tests assert the stable frame ledger and fixture contracts:
//!
//! 1. The `FrameKind(u16)` ledger in `rmux_proto::frame_kind` is the
//!    authoritative compatibility ledger. Each fixture's ledger entry pins its
//!    bincode tag without relying on Rust source order: encoded payload bytes
//!    are inspected and compared against `FrameKind::bincode_tag` for both
//!    `Request` and `Response` fixtures. A representative cross-section of
//!    other variants exercises the same invariant.
//! 2. The checked-in ledger fixtures under
//!    `tests/wire-fixtures/ledger-v1-current-wire/`
//!    decode through both `decode_frame` and `FrameDecoder` using the current
//!    wire envelope (currently `RMUX_WIRE_VERSION = 3`), asserting stable
//!    semantic fields rather than enum source order. Encoding canonical inputs
//!    reproduces each fixture byte-for-byte to guard against silent codec drift.

use std::path::{Path, PathBuf};

use rmux_proto::{
    decode_frame, encode_frame, frame_kind_for_request, frame_kind_for_response, ledger_entry_for,
    AttachSessionExt2Request, AttachSessionExt3Request, BindKeyRequest, CancelSdkWaitRequest,
    CancelSdkWaitResponse, CapturePaneRequest, CapturePaneTargetActionRequest,
    ClientTerminalContext, ClockModeRequest, ControlMode, ControlModeRequest, ControlModeResponse,
    DetachClientRequest, DisplayMessageExtRequest, ErrorResponse, FrameDecoder, FrameDirection,
    FrameFeature, FrameKind, FrameStatus, HandshakeRequest, HandshakeResponse, HasSessionRequest,
    HasSessionResponse, HookLifecycle, HookName, KillServerResponse, KillSessionRequest,
    ListBuffersRequest, NewSessionResponse, OptionName, PaneId, PaneInputRequest, PaneKillRequest,
    PaneOutputCursor, PaneOutputCursorRequest, PaneOutputCursorResponse, PaneOutputEvent,
    PaneOutputLagNotice, PaneOutputLagResponse, PaneOutputSubscriptionId,
    PaneOutputSubscriptionStart, PaneRecentOutput, PaneResizeRequest, PaneRespawnRequest,
    PaneSelectRequest, PaneSnapshotCursor, PaneSnapshotRefRequest, PaneSnapshotResponse,
    PaneTarget, PaneTargetRef, Request, ResizePaneAdjustment, ResizePaneTargetActionRequest,
    ResolveTargetRequest, ResolveTargetType, Response, RmuxError, ScopeSelector,
    SdkWaitForOutputRefRequest, SdkWaitForOutputRequest, SdkWaitForOutputResponse, SdkWaitId,
    SdkWaitOutcome, SdkWaitOwnerId, SendKeysExt2Request, SendKeysRequest, SendKeysResponse,
    SessionName, SetHookRequest, SetOptionMode, SetOptionRequest, SplitDirection,
    SplitWindowTargetActionRequest, SubscribePaneOutputRefRequest, SubscribePaneOutputRequest,
    SubscribePaneOutputResponse, TerminalSize, UnsubscribePaneOutputRequest,
    UnsubscribePaneOutputResponse, WebShareConfigRequest, WebShareListener, WebShareRequest,
    WebShareResponse, WindowTarget, RMUX_FRAME_MAGIC, RMUX_WIRE_VERSION, V1_FRAME_LEDGER,
};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("wire-fixtures")
        .join("ledger-v1-current-wire")
}

fn read_fixture(name: &str) -> Vec<u8> {
    let path = fixture_root().join(format!("{name}.bin"));
    std::fs::read(&path)
        .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()))
}

fn alpha() -> SessionName {
    SessionName::new("alpha").expect("valid session name")
}

fn pane_alpha_2() -> PaneTarget {
    PaneTarget::new(alpha(), 2)
}

#[derive(Debug)]
struct FullFrameFixture {
    name: &'static str,
    kind: FrameKind,
    direction: FrameDirection,
    feature: FrameFeature,
    encode: Frame,
}

#[derive(Debug)]
enum Frame {
    Request(Request),
    Response(Response),
}

fn fixtures() -> Vec<FullFrameFixture> {
    let has_session = Request::HasSession(HasSessionRequest { target: alpha() });
    let kill_session = Request::KillSession(KillSessionRequest {
        target: alpha(),
        kill_all_except_target: false,
        clear_alerts: false,
    });
    let send_keys = Request::SendKeys(SendKeysRequest {
        target: pane_alpha_2(),
        keys: vec!["echo".to_owned(), "Enter".to_owned()],
    });
    let capture_pane = Request::CapturePane(Box::new(CapturePaneRequest {
        target: pane_alpha_2(),
        start: Some(-5),
        end: Some(-1),
        print: true,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        join_wrapped: false,
        use_mode_screen: false,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    }));
    let control_mode = Request::ControlMode(ControlModeRequest {
        mode: ControlMode::ControlControl,
        client_terminal: ClientTerminalContext::default(),
    });
    let split_target_action =
        Request::SplitWindowTargetAction(Box::new(SplitWindowTargetActionRequest {
            target: Some("alpha:0.0".to_owned()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
            command: Some(vec!["printf".to_owned(), "ready".to_owned()]),
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        }));
    let resize_target_action = Request::ResizePaneTargetAction(ResizePaneTargetActionRequest {
        target: Some("alpha:0.0".to_owned()),
        adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 100 },
    });
    let capture_target_action =
        Request::CapturePaneTargetAction(Box::new(CapturePaneTargetActionRequest {
            target: Some("alpha:0.0".to_owned()),
            start: Some(-10),
            end: Some(-1),
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        }));
    let new_session_response = Response::NewSession(NewSessionResponse {
        session_name: alpha(),
        detached: true,
        output: None,
    });
    let error_response = Response::Error(ErrorResponse {
        error: RmuxError::SessionNotFound("gone".to_owned()),
    });

    vec![
        FullFrameFixture {
            name: "has_session_request",
            kind: frame_kind_for_request(&has_session),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Sessions,
            encode: Frame::Request(has_session),
        },
        FullFrameFixture {
            name: "kill_session_request",
            kind: frame_kind_for_request(&kill_session),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Sessions,
            encode: Frame::Request(kill_session),
        },
        FullFrameFixture {
            name: "send_keys_request",
            kind: frame_kind_for_request(&send_keys),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Panes,
            encode: Frame::Request(send_keys),
        },
        FullFrameFixture {
            name: "capture_pane_request",
            kind: frame_kind_for_request(&capture_pane),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Panes,
            encode: Frame::Request(capture_pane),
        },
        FullFrameFixture {
            name: "control_mode_request",
            kind: frame_kind_for_request(&control_mode),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Control,
            encode: Frame::Request(control_mode),
        },
        FullFrameFixture {
            name: "split_window_target_action_request",
            kind: frame_kind_for_request(&split_target_action),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Panes,
            encode: Frame::Request(split_target_action),
        },
        FullFrameFixture {
            name: "resize_pane_target_action_request",
            kind: frame_kind_for_request(&resize_target_action),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Panes,
            encode: Frame::Request(resize_target_action),
        },
        FullFrameFixture {
            name: "capture_pane_target_action_request",
            kind: frame_kind_for_request(&capture_target_action),
            direction: FrameDirection::ClientToServer,
            feature: FrameFeature::Panes,
            encode: Frame::Request(capture_target_action),
        },
        FullFrameFixture {
            name: "new_session_response",
            kind: frame_kind_for_response(&new_session_response),
            direction: FrameDirection::ServerToClient,
            feature: FrameFeature::Sessions,
            encode: Frame::Response(new_session_response),
        },
        FullFrameFixture {
            name: "error_response",
            kind: frame_kind_for_response(&error_response),
            direction: FrameDirection::ServerToClient,
            feature: FrameFeature::Errors,
            encode: Frame::Response(error_response),
        },
    ]
}

fn encode_fixture(fixture: &FullFrameFixture) -> Vec<u8> {
    match &fixture.encode {
        Frame::Request(request) => encode_frame(request).expect("request encodes"),
        Frame::Response(response) => encode_frame(response).expect("response encodes"),
    }
}

#[test]
fn fixture_count_matches_manifest() {
    assert_eq!(
        fixtures().len(),
        10,
        "Milestone 3 mandates ten ledger fixtures"
    );
}

#[test]
fn fixture_set_covers_both_directions_and_multiple_features() {
    let entries = fixtures();
    let directions: std::collections::HashSet<_> =
        entries.iter().map(|fixture| fixture.direction).collect();
    assert!(
        directions.len() >= 2,
        "fixtures must cover both client→server and server→client"
    );
    let features: std::collections::HashSet<_> =
        entries.iter().map(|fixture| fixture.feature).collect();
    assert!(
        features.len() >= 4,
        "fixtures should sample at least four owning features, got {features:?}"
    );
}

#[test]
fn ledger_pins_each_fixture_with_active_metadata() {
    for fixture in fixtures() {
        let entry = ledger_entry_for(fixture.kind)
            .unwrap_or_else(|| panic!("ledger entry missing for {}", fixture.name));
        assert_eq!(entry.fixture, Some(fixture.name), "fixture name mismatch");
        assert_eq!(entry.direction, fixture.direction, "direction mismatch");
        assert_eq!(entry.feature, fixture.feature, "feature mismatch");
        assert!(matches!(entry.status, FrameStatus::Active));
    }
}

#[test]
fn fixture_bytes_match_canonical_encoding() {
    for fixture in fixtures() {
        let encoded = encode_fixture(&fixture);
        let stored = read_fixture(fixture.name);
        assert_eq!(
            stored, encoded,
            "fixture {} drifted from canonical encoding; \
             regenerate via `cargo test -p rmux-proto --test wire_ledger_v1 \
             -- --ignored regenerate_v1_fixtures`",
            fixture.name
        );
    }
}

#[test]
fn fixture_envelope_uses_current_wire_version() {
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        assert_eq!(
            bytes.first().copied(),
            Some(RMUX_FRAME_MAGIC),
            "fixture {} missing magic byte",
            fixture.name
        );
        // Pin the canonical single-byte varint encoding for the current wire
        // version: a multi-byte varint with the same logical value would drift
        // the envelope layout silently. Every fixture must use exactly one byte
        // for the wire-version varint while this build supports a single
        // version.
        assert_eq!(
            bytes.get(1).copied(),
            Some(RMUX_WIRE_VERSION as u8),
            "fixture {} uses wrong wire version byte",
            fixture.name
        );
        assert_eq!(
            bytes.get(1).copied().map(|b| b & 0x80),
            Some(0),
            "fixture {} wire-version varint must terminate in one byte",
            fixture.name
        );
        let length_bytes: [u8; 4] = bytes[2..6].try_into().expect("4 length bytes");
        let announced = u32::from_le_bytes(length_bytes) as usize;
        assert_eq!(
            announced + 6,
            bytes.len(),
            "fixture {} envelope length mismatch",
            fixture.name
        );
        assert!(announced > 0, "fixture {} has empty payload", fixture.name);
    }
}

#[test]
fn fixture_payload_first_four_bytes_match_ledger_bincode_tag() {
    // Edge case: the high direction bit must never leak into the payload tag.
    // We assert the payload's leading u32 (the bincode tag of the
    // top-level Request/Response variant) equals the ledger entry's
    // `bincode_tag()` exactly. This is a narrower contract than the
    // cross-section invariant: it pins fixture bytes to ledger metadata
    // without re-running bincode at all.
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let payload = &bytes[6..];
        assert!(
            payload.len() >= 4,
            "fixture {} payload shorter than 4 bytes",
            fixture.name
        );
        let tag_bytes: [u8; 4] = payload[..4].try_into().expect("4 tag bytes");
        let tag = u32::from_le_bytes(tag_bytes);
        assert_eq!(
            tag,
            fixture.kind.bincode_tag(),
            "fixture {} payload tag drifted from ledger bincode_tag",
            fixture.name
        );
        assert!(
            tag < 0x8000,
            "fixture {} payload tag must fit in 15 bits",
            fixture.name
        );
    }
}

#[test]
fn fixture_full_frame_decodes_through_decode_frame() {
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        match &fixture.encode {
            Frame::Request(expected) => {
                let decoded: Request = decode_frame(&bytes).expect("decode_frame request");
                assert_request_semantic_equal(&decoded, expected, fixture.name);
                assert_eq!(
                    frame_kind_for_request(&decoded),
                    fixture.kind,
                    "fixture {} kind",
                    fixture.name
                );
            }
            Frame::Response(expected) => {
                let decoded: Response = decode_frame(&bytes).expect("decode_frame response");
                assert_response_semantic_equal(&decoded, expected, fixture.name);
                assert_eq!(
                    frame_kind_for_response(&decoded),
                    fixture.kind,
                    "fixture {} kind",
                    fixture.name
                );
            }
        }
    }
}

#[test]
fn fixture_full_frame_decodes_through_frame_decoder_in_one_shot() {
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let mut decoder = FrameDecoder::new();
        decoder.push_bytes(&bytes);
        match &fixture.encode {
            Frame::Request(expected) => {
                let decoded: Option<Request> = decoder
                    .next_frame::<Request>()
                    .expect("decoder yields no error");
                let decoded = decoded.expect("decoder produced a frame");
                assert_request_semantic_equal(&decoded, expected, fixture.name);
                assert_eq!(
                    decoder.next_frame::<Request>().expect("no extra frames"),
                    None
                );
            }
            Frame::Response(expected) => {
                let decoded: Option<Response> = decoder
                    .next_frame::<Response>()
                    .expect("decoder yields no error");
                let decoded = decoded.expect("decoder produced a frame");
                assert_response_semantic_equal(&decoded, expected, fixture.name);
                assert_eq!(
                    decoder.next_frame::<Response>().expect("no extra frames"),
                    None
                );
            }
        }
        assert!(
            decoder.remaining_bytes().is_empty(),
            "fixture {} left bytes in decoder",
            fixture.name
        );
    }
}

#[test]
fn fixture_full_frame_decodes_through_frame_decoder_in_byte_pieces() {
    // Edge case: every fixture must decode when bytes arrive one at a time.
    // This guards against length/varint state regressions in `FrameDecoder`.
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let mut decoder = FrameDecoder::new();
        for window in bytes.windows(1) {
            decoder.push_bytes(window);
        }
        match &fixture.encode {
            Frame::Request(expected) => {
                let decoded: Option<Request> = decoder
                    .next_frame::<Request>()
                    .expect("byte-piece request decodes");
                let decoded = decoded.expect("byte-piece request produced a frame");
                assert_request_semantic_equal(&decoded, expected, fixture.name);
            }
            Frame::Response(expected) => {
                let decoded: Option<Response> = decoder
                    .next_frame::<Response>()
                    .expect("byte-piece response decodes");
                let decoded = decoded.expect("byte-piece response produced a frame");
                assert_response_semantic_equal(&decoded, expected, fixture.name);
            }
        }
    }
}

#[test]
fn payload_only_bincode_roundtrip_supplements_full_frame_coverage() {
    // Supplemental: the contract permits payload-only `bincode::deserialize`
    // checks on top of the full-frame coverage. We exercise it on at least
    // one fixture per direction.
    let bytes = read_fixture("has_session_request");
    let payload = &bytes[6..];
    let request: Request = bincode::deserialize(payload).expect("payload deserializes");
    match request {
        Request::HasSession(value) => assert_eq!(value.target.as_str(), "alpha"),
        other => panic!("unexpected request variant: {other:?}"),
    }

    let bytes = read_fixture("new_session_response");
    let payload = &bytes[6..];
    let response: Response = bincode::deserialize(payload).expect("payload deserializes");
    match response {
        Response::NewSession(value) => {
            assert_eq!(value.session_name.as_str(), "alpha");
            assert!(value.detached);
            assert!(value.output.is_none());
        }
        other => panic!("unexpected response variant: {other:?}"),
    }
}

/// Representative cross-section of variants used to assert that the ledger
/// `FrameKind` low bits match the bincode tag observed at runtime. This is
/// not a comprehensive variant table — its purpose is to detect drift across
/// every covered feature without coupling the test to brittle struct shapes.
fn cross_section_requests() -> Vec<Request> {
    let alpha = alpha();
    let pane = pane_alpha_2();
    let pane_ref = PaneTargetRef::by_id(alpha.clone(), PaneId::new(9));
    vec![
        Request::HasSession(HasSessionRequest {
            target: alpha.clone(),
        }),
        Request::KillSession(KillSessionRequest {
            target: alpha.clone(),
            kill_all_except_target: false,
            clear_alerts: false,
        }),
        Request::DetachClient(DetachClientRequest),
        Request::SendKeys(SendKeysRequest {
            target: pane.clone(),
            keys: vec!["echo".to_owned()],
        }),
        Request::ListBuffers(ListBuffersRequest::default()),
        Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultTerminal,
            value: "tmux".to_owned(),
            mode: SetOptionMode::Replace,
        }),
        Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Global,
            hook: HookName::ClientAttached,
            command: "echo".to_owned(),
            lifecycle: HookLifecycle::OneShot,
        }),
        Request::ControlMode(ControlModeRequest {
            mode: ControlMode::Plain,
            client_terminal: ClientTerminalContext::default(),
        }),
        Request::ClockMode(ClockModeRequest {
            target: Some(pane.clone()),
        }),
        Request::CapturePane(Box::new(CapturePaneRequest {
            target: pane.clone(),
            start: None,
            end: None,
            print: true,
            buffer_name: None,
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        })),
        Request::BindKey(Box::new(BindKeyRequest {
            table_name: "root".to_owned(),
            key: "C-a".to_owned(),
            note: None,
            repeat: false,
            command: Some(vec!["send-prefix".to_owned()]),
        })),
        Request::ResolveTarget(ResolveTargetRequest {
            target: Some("alpha:0.0".to_owned()),
            target_type: ResolveTargetType::Pane,
            window_index: false,
            prefer_unattached: false,
        }),
        Request::Handshake(HandshakeRequest::current()),
        Request::SubscribePaneOutput(SubscribePaneOutputRequest {
            target: pane.clone(),
            start: PaneOutputSubscriptionStart::Now,
        }),
        Request::SubscribePaneOutputRef(SubscribePaneOutputRefRequest {
            target: PaneTargetRef::slot(pane.clone()),
            start: PaneOutputSubscriptionStart::Now,
        }),
        Request::UnsubscribePaneOutput(UnsubscribePaneOutputRequest {
            subscription_id: PaneOutputSubscriptionId::new(7),
        }),
        Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id: PaneOutputSubscriptionId::new(7),
            max_events: Some(4),
        }),
        Request::SdkWaitForOutput(SdkWaitForOutputRequest {
            owner_id: SdkWaitOwnerId::new(3),
            wait_id: SdkWaitId::new(4),
            target: pane.clone(),
            bytes: b"ready".to_vec(),
            start: PaneOutputSubscriptionStart::Now,
        }),
        Request::SdkWaitForOutputRef(SdkWaitForOutputRefRequest {
            owner_id: SdkWaitOwnerId::new(3),
            wait_id: SdkWaitId::new(5),
            target: PaneTargetRef::slot(pane.clone()),
            bytes: b"ready".to_vec(),
            start: PaneOutputSubscriptionStart::Now,
        }),
        Request::CancelSdkWait(CancelSdkWaitRequest {
            owner_id: SdkWaitOwnerId::new(3),
            wait_id: SdkWaitId::new(4),
        }),
        Request::PaneInput(PaneInputRequest {
            target: pane_ref.clone(),
            keys: vec!["ready".to_owned()],
            literal: true,
        }),
        Request::PaneResize(PaneResizeRequest {
            target: pane_ref.clone(),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 80 },
        }),
        Request::PaneKill(PaneKillRequest {
            target: pane_ref.clone(),
            kill_all_except: false,
        }),
        Request::PaneRespawn(Box::new(PaneRespawnRequest {
            target: pane_ref.clone(),
            kill: true,
            start_directory: Some(std::path::PathBuf::from("/tmp")),
            environment: Some(vec!["RMUX_TEST=1".to_owned()]),
            command: Some(vec!["printf".to_owned(), "ready".to_owned()]),
            process_command: None,
            keep_alive_on_exit: Some(true),
        })),
        Request::PaneSnapshotRef(PaneSnapshotRefRequest {
            target: pane_ref.clone(),
        }),
        Request::PaneSelect(PaneSelectRequest {
            target: pane_ref,
            title: Some("agent".to_owned()),
        }),
        Request::WebShare(Box::new(WebShareRequest::Config(WebShareConfigRequest))),
        Request::DisplayMessageExt(Box::new(DisplayMessageExtRequest {
            target: Some(rmux_proto::Target::Session(alpha.clone())),
            print: true,
            message: Some("#{client_name}".to_owned()),
            target_client: Some("=".to_owned()),
            empty_target_context: false,
        })),
        Request::SendKeysExt2(Box::new(SendKeysExt2Request {
            target: Some(pane.clone()),
            keys: vec!["A".to_owned()],
            expand_formats: false,
            hex: false,
            literal: true,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
            target_client: Some("=".to_owned()),
        })),
        Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
            AttachSessionExt2Request {
                target: Some(alpha.clone()),
                target_spec: Some("alpha:0.0".to_owned()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
                working_directory: None,
                client_terminal: ClientTerminalContext::default(),
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            },
            vec![rmux_proto::CAPABILITY_ATTACH_RENDER.to_owned()],
        ))),
    ]
}

fn cross_section_responses() -> Vec<Response> {
    let alpha = alpha();
    let cursor = PaneOutputCursor {
        next_sequence: 2,
        missed_events: 0,
    };
    vec![
        Response::HasSession(HasSessionResponse { exists: true }),
        Response::NewSession(NewSessionResponse {
            session_name: alpha.clone(),
            detached: true,
            output: None,
        }),
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("gone".to_owned()),
        }),
        Response::SendKeys(SendKeysResponse { key_count: 1 }),
        Response::KillServer(KillServerResponse),
        Response::ControlMode(ControlModeResponse {
            mode: ControlMode::ControlControl,
        }),
        Response::Handshake(HandshakeResponse::current()),
        Response::SubscribePaneOutput(SubscribePaneOutputResponse {
            subscription_id: PaneOutputSubscriptionId::new(7),
            target: pane_alpha_2(),
            pane_id: rmux_proto::PaneId::new(2),
            cursor,
        }),
        Response::UnsubscribePaneOutput(UnsubscribePaneOutputResponse {
            subscription_id: PaneOutputSubscriptionId::new(7),
            removed: true,
        }),
        Response::PaneOutputCursor(PaneOutputCursorResponse {
            subscription_id: PaneOutputSubscriptionId::new(7),
            cursor,
            events: vec![PaneOutputEvent {
                sequence: 1,
                bytes: b"x".to_vec(),
            }],
            limited: false,
        }),
        Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
            subscription_id: PaneOutputSubscriptionId::new(7),
            cursor: PaneOutputCursor {
                next_sequence: 10,
                missed_events: 8,
            },
            lag: PaneOutputLagNotice {
                expected_sequence: 2,
                resume_sequence: 10,
                missed_events: 8,
                newest_sequence: 12,
                recent: PaneRecentOutput {
                    bytes: b"recent".to_vec(),
                    oldest_sequence: Some(10),
                    newest_sequence: Some(12),
                },
            },
        })),
        Response::SdkWaitForOutput(SdkWaitForOutputResponse {
            wait_id: SdkWaitId::new(4),
            outcome: SdkWaitOutcome::Matched,
        }),
        Response::CancelSdkWait(CancelSdkWaitResponse {
            wait_id: SdkWaitId::new(4),
            removed: true,
        }),
        Response::WebShare(Box::new(WebShareResponse::Config(
            rmux_proto::WebShareConfigResponse {
                listener: WebShareListener {
                    host: "127.0.0.1".to_owned(),
                    port: 9777,
                    frontend_origin: "https://share.rmux.io".to_owned(),
                },
                output: rmux_proto::CommandOutput::from_stdout(
                    "127.0.0.1:9777 https://share.rmux.io\n",
                ),
            },
        ))),
    ]
}

#[test]
fn pane_snapshot_output_and_lag_response_kinds_stay_lane_scoped() {
    let snapshot = Response::PaneSnapshot(PaneSnapshotResponse {
        cols: 0,
        rows: 0,
        cells: Vec::new(),
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: false,
            style: 0,
        },
        revision: 55,
    });
    let output_cursor = Response::PaneOutputCursor(PaneOutputCursorResponse {
        subscription_id: PaneOutputSubscriptionId::new(7),
        cursor: PaneOutputCursor {
            next_sequence: 6,
            missed_events: 0,
        },
        events: vec![PaneOutputEvent {
            sequence: 5,
            bytes: b"live".to_vec(),
        }],
        limited: false,
    });
    let output_lag = Response::PaneOutputLag(Box::new(PaneOutputLagResponse {
        subscription_id: PaneOutputSubscriptionId::new(7),
        cursor: PaneOutputCursor {
            next_sequence: 10,
            missed_events: 4,
        },
        lag: PaneOutputLagNotice {
            expected_sequence: 6,
            resume_sequence: 10,
            missed_events: 4,
            newest_sequence: 12,
            recent: PaneRecentOutput {
                bytes: b"bounded-hint".to_vec(),
                oldest_sequence: None,
                newest_sequence: Some(12),
            },
        },
    }));

    assert_eq!(frame_kind_for_response(&snapshot).bincode_tag(), 80);
    assert_eq!(frame_kind_for_response(&output_cursor).bincode_tag(), 83);
    assert_eq!(frame_kind_for_response(&output_lag).bincode_tag(), 84);
    assert_ne!(
        frame_kind_for_response(&snapshot),
        frame_kind_for_response(&output_cursor)
    );
    assert_ne!(
        frame_kind_for_response(&output_cursor),
        frame_kind_for_response(&output_lag)
    );

    let decoded_frame: Response =
        decode_frame(&encode_frame(&output_lag).expect("lag encodes")).expect("lag frame decodes");
    assert_eq!(decoded_frame, output_lag);
    let decoded_bincode: Response =
        bincode::deserialize(&bincode::serialize(&output_lag).expect("lag serializes as bincode"))
            .expect("lag deserializes from bincode");
    assert_eq!(decoded_bincode, output_lag);

    let Response::PaneOutputLag(decoded_lag) = decoded_bincode else {
        panic!("lag response must not decode as an output cursor or snapshot lane");
    };
    assert_eq!(decoded_lag.lag.recent.bytes, b"bounded-hint");
    assert_eq!(decoded_lag.lag.expected_sequence, 6);
    assert_eq!(decoded_lag.lag.resume_sequence, 10);
    assert_eq!(decoded_lag.cursor.next_sequence, 10);
}

#[test]
fn ledger_kind_low_bits_match_bincode_tag_for_request_cross_section() {
    for request in cross_section_requests() {
        let kind = frame_kind_for_request(&request);
        let encoded = bincode::serialize(&request).expect("encodes");
        assert!(encoded.len() >= 4, "{:?}", request);
        let tag_bytes: [u8; 4] = encoded[..4].try_into().expect("4 tag bytes");
        let tag = u32::from_le_bytes(tag_bytes);
        assert_eq!(
            kind.bincode_tag(),
            tag,
            "ledger drift for request {:?}",
            request
        );
        let entry = ledger_entry_for(kind).expect("ledger entry exists");
        assert!(matches!(entry.status, FrameStatus::Active));
    }
}

#[test]
fn ledger_kind_low_bits_match_bincode_tag_for_response_cross_section() {
    for response in cross_section_responses() {
        let kind = frame_kind_for_response(&response);
        let encoded = bincode::serialize(&response).expect("encodes");
        assert!(encoded.len() >= 4, "{:?}", response);
        let tag_bytes: [u8; 4] = encoded[..4].try_into().expect("4 tag bytes");
        let tag = u32::from_le_bytes(tag_bytes);
        assert_eq!(
            kind.bincode_tag(),
            tag,
            "ledger drift for response {:?}",
            response
        );
        let entry = ledger_entry_for(kind).expect("ledger entry exists");
        assert!(matches!(entry.status, FrameStatus::Active));
    }
}

#[test]
fn ledger_active_size_matches_request_and_response_variant_count() {
    let active_count = V1_FRAME_LEDGER
        .iter()
        .filter(|entry| matches!(entry.status, FrameStatus::Active))
        .count();
    // Active entries = 121 Request variants + 94 Response variants.
    assert_eq!(active_count, 121 + 94, "active ledger size mismatch");
}

#[test]
fn ledger_reserved_entries_cover_both_directions() {
    let reserved: Vec<&_> = V1_FRAME_LEDGER
        .iter()
        .filter(|entry| matches!(entry.status, FrameStatus::Reserved))
        .collect();
    assert!(
        reserved
            .iter()
            .any(|entry| entry.direction == FrameDirection::ClientToServer),
        "reserved c2s sentinel missing"
    );
    assert!(
        reserved
            .iter()
            .any(|entry| entry.direction == FrameDirection::ServerToClient),
        "reserved s2c sentinel missing"
    );
}

#[test]
fn frame_decoder_handles_back_to_back_request_fixtures() {
    let mut combined: Vec<u8> = Vec::new();
    let mut expected_kinds: Vec<FrameKind> = Vec::new();
    for fixture in fixtures() {
        if let Frame::Request(_) = &fixture.encode {
            combined.extend_from_slice(&read_fixture(fixture.name));
            expected_kinds.push(fixture.kind);
        }
    }

    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&combined);
    let mut decoded_kinds: Vec<FrameKind> = Vec::new();
    while let Some(request) = decoder.next_frame::<Request>().expect("no error") {
        decoded_kinds.push(frame_kind_for_request(&request));
    }
    assert_eq!(decoded_kinds, expected_kinds);
    assert!(decoder.remaining_bytes().is_empty());
}

#[test]
fn frame_decoder_rejects_corrupt_fixture_magic() {
    let mut bytes = read_fixture("has_session_request");
    bytes[0] = 0x00;
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&bytes);
    let err = decoder
        .next_frame::<Request>()
        .expect_err("bad magic must fail");
    assert!(matches!(err, RmuxError::BadFrameMagic(0x00)));
}

#[test]
fn frame_decoder_rejects_unsupported_wire_version() {
    let bytes = read_fixture("has_session_request");
    let payload = &bytes[6..];
    let mut synthetic = Vec::with_capacity(bytes.len());
    synthetic.push(RMUX_FRAME_MAGIC);
    synthetic.push(0x07); // claim wire version 7
    let length = u32::try_from(payload.len()).expect("fits");
    synthetic.extend_from_slice(&length.to_le_bytes());
    synthetic.extend_from_slice(payload);

    let err = decode_frame::<Request>(&synthetic).expect_err("unsupported version must fail");
    assert!(matches!(
        err,
        RmuxError::UnsupportedWireVersion {
            got: 7,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        }
    ));
}

#[test]
fn fixture_truncated_by_one_byte_is_rejected() {
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let truncated = &bytes[..bytes.len() - 1];
        match &fixture.encode {
            Frame::Request(_) => {
                let err =
                    decode_frame::<Request>(truncated).expect_err("truncated request must fail");
                assert!(matches!(err, RmuxError::IncompleteFrame { .. }));
            }
            Frame::Response(_) => {
                let err =
                    decode_frame::<Response>(truncated).expect_err("truncated response must fail");
                assert!(matches!(err, RmuxError::IncompleteFrame { .. }));
            }
        }
    }
}

#[test]
fn fixture_with_trailing_byte_is_rejected_by_decode_frame() {
    let mut bytes = read_fixture("has_session_request");
    bytes.push(0xFF);
    let err = decode_frame::<Request>(&bytes).expect_err("trailing byte must fail");
    assert!(matches!(err, RmuxError::Decode(_)));
}

#[test]
fn ledger_request_and_response_partitions_are_disjoint() {
    let request_kinds: std::collections::HashSet<FrameKind> = cross_section_requests()
        .iter()
        .map(frame_kind_for_request)
        .collect();
    let response_kinds: std::collections::HashSet<FrameKind> = cross_section_responses()
        .iter()
        .map(frame_kind_for_response)
        .collect();
    assert!(
        request_kinds.is_disjoint(&response_kinds),
        "request and response FrameKind ranges must not overlap"
    );
    for kind in &request_kinds {
        assert_eq!((*kind).direction(), FrameDirection::ClientToServer);
    }
    for kind in &response_kinds {
        assert_eq!((*kind).direction(), FrameDirection::ServerToClient);
    }
}

fn assert_request_semantic_equal(actual: &Request, expected: &Request, label: &str) {
    match (actual, expected) {
        (Request::HasSession(actual), Request::HasSession(expected)) => {
            assert_eq!(actual.target.as_str(), expected.target.as_str(), "{label}");
        }
        (Request::KillSession(actual), Request::KillSession(expected)) => {
            assert_eq!(actual.target.as_str(), expected.target.as_str(), "{label}");
            assert_eq!(
                actual.kill_all_except_target, expected.kill_all_except_target,
                "{label}"
            );
            assert_eq!(actual.clear_alerts, expected.clear_alerts, "{label}");
        }
        (Request::SendKeys(actual), Request::SendKeys(expected)) => {
            assert_eq!(
                actual.target.session_name().as_str(),
                expected.target.session_name().as_str(),
                "{label}"
            );
            assert_eq!(
                actual.target.window_index(),
                expected.target.window_index(),
                "{label}"
            );
            assert_eq!(
                actual.target.pane_index(),
                expected.target.pane_index(),
                "{label}"
            );
            assert_eq!(actual.keys, expected.keys, "{label}");
        }
        (Request::CapturePane(actual), Request::CapturePane(expected)) => {
            assert_eq!(
                actual.target.session_name().as_str(),
                expected.target.session_name().as_str(),
                "{label}"
            );
            assert_eq!(actual.start, expected.start, "{label}");
            assert_eq!(actual.end, expected.end, "{label}");
            assert_eq!(actual.print, expected.print, "{label}");
        }
        (Request::ControlMode(actual), Request::ControlMode(expected)) => {
            assert_eq!(actual.mode, expected.mode, "{label}");
        }
        (Request::SplitWindowTargetAction(actual), Request::SplitWindowTargetAction(expected)) => {
            assert_eq!(actual.target, expected.target, "{label}");
            assert_eq!(actual.direction, expected.direction, "{label}");
            assert_eq!(actual.command, expected.command, "{label}");
        }
        (Request::ResizePaneTargetAction(actual), Request::ResizePaneTargetAction(expected)) => {
            assert_eq!(actual.target, expected.target, "{label}");
            assert_eq!(actual.adjustment, expected.adjustment, "{label}");
        }
        (Request::CapturePaneTargetAction(actual), Request::CapturePaneTargetAction(expected)) => {
            assert_eq!(actual.target, expected.target, "{label}");
            assert_eq!(actual.start, expected.start, "{label}");
            assert_eq!(actual.end, expected.end, "{label}");
            assert_eq!(actual.print, expected.print, "{label}");
        }
        (Request::NewSession(actual), Request::NewSession(expected)) => {
            assert_eq!(
                actual.session_name.as_str(),
                expected.session_name.as_str(),
                "{label}"
            );
            assert_eq!(actual.detached, expected.detached, "{label}");
        }
        (actual, expected) => {
            panic!("{label}: variant mismatch — got {actual:?}, expected {expected:?}")
        }
    }
}

fn assert_response_semantic_equal(actual: &Response, expected: &Response, label: &str) {
    match (actual, expected) {
        (Response::NewSession(actual), Response::NewSession(expected)) => {
            assert_eq!(
                actual.session_name.as_str(),
                expected.session_name.as_str(),
                "{label}"
            );
            assert_eq!(actual.detached, expected.detached, "{label}");
            assert_eq!(
                actual.output.is_some(),
                expected.output.is_some(),
                "{label}"
            );
        }
        (Response::Error(actual), Response::Error(expected)) => {
            assert_eq!(
                actual.error.to_string(),
                expected.error.to_string(),
                "{label}"
            );
        }
        (actual, expected) => {
            panic!("{label}: variant mismatch — got {actual:?}, expected {expected:?}")
        }
    }
}

#[test]
fn fixture_set_lookup_through_ledger_entry_for_round_trip() {
    // Edge case: the canonical lookup direction (FrameKind → ledger entry)
    // must agree with the ledger's `fixture` field. Adding a new fixture
    // without updating the ledger entry's `fixture` field would silently
    // make this round-trip fail.
    for fixture in fixtures() {
        let entry = ledger_entry_for(fixture.kind)
            .unwrap_or_else(|| panic!("ledger entry missing for {}", fixture.name));
        assert_eq!(entry.fixture, Some(fixture.name));
        assert_eq!(entry.direction, fixture.kind.direction());
        match (&fixture.encode, entry.direction) {
            (Frame::Request(_), FrameDirection::ClientToServer) => {}
            (Frame::Response(_), FrameDirection::ServerToClient) => {}
            (frame, direction) => {
                panic!(
                    "{} ledger direction {direction:?} disagrees with frame {frame:?}",
                    fixture.name
                )
            }
        }
    }
}

#[test]
fn ledger_entry_for_unknown_kind_returns_none() {
    // Edge case: a kind that is neither active nor reserved must not match
    // any ledger entry. We pick a value far from any current allocation but
    // inside the c2s band.
    let unknown = FrameKind(0x4321);
    assert!(
        ledger_entry_for(unknown).is_none(),
        "unexpected ledger hit for unknown kind {:#06x}",
        unknown.0
    );

    // Edge case: same on the s2c side. `0x8000 | 0x4321 = 0xC321` is also
    // far from any current allocation.
    let unknown_s2c = FrameKind(0xC321);
    assert!(
        ledger_entry_for(unknown_s2c).is_none(),
        "unexpected ledger hit for unknown kind {:#06x}",
        unknown_s2c.0
    );
}

#[test]
fn ledger_bincode_tag_fits_in_fifteen_bits_for_every_entry() {
    // Edge case: the high bit is reserved as direction. Any active or
    // reserved entry whose `bincode_tag()` overflows into bit 15 would mean
    // either a corrupted entry or a direction bit accidentally folded into
    // the payload tag.
    for entry in V1_FRAME_LEDGER {
        assert!(
            entry.kind.bincode_tag() < 0x8000,
            "ledger entry {} kind {:#06x} bincode_tag exceeds 15 bits",
            entry.dto_type,
            entry.kind.0,
        );
    }
}

#[test]
fn full_ledger_request_and_response_partitions_are_disjoint() {
    // The cross-section overlap test in
    // `ledger_request_and_response_partitions_are_disjoint` only exercises
    // a sample. Walk every active entry once and assert that c2s and s2c
    // bands never alias.
    let mut c2s = std::collections::HashSet::new();
    let mut s2c = std::collections::HashSet::new();
    for entry in V1_FRAME_LEDGER
        .iter()
        .filter(|entry| matches!(entry.status, FrameStatus::Active))
    {
        match entry.direction {
            FrameDirection::ClientToServer => {
                assert!(c2s.insert(entry.kind), "duplicate c2s {:?}", entry.dto_type);
            }
            FrameDirection::ServerToClient => {
                assert!(s2c.insert(entry.kind), "duplicate s2c {:?}", entry.dto_type);
            }
        }
    }
    assert!(c2s.is_disjoint(&s2c), "c2s and s2c bands overlap");
}

#[test]
fn fixture_round_trips_decode_then_re_encode_byte_for_byte() {
    // Edge case: any fixture that decodes through `decode_frame` must encode
    // back to the exact same bytes. Catches cases where decode succeeds but
    // payload normalisation drops or rewrites optional fields.
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let re_encoded = match &fixture.encode {
            Frame::Request(_) => {
                let decoded: Request = decode_frame(&bytes).expect("request decodes");
                encode_frame(&decoded).expect("request re-encodes")
            }
            Frame::Response(_) => {
                let decoded: Response = decode_frame(&bytes).expect("response decodes");
                encode_frame(&decoded).expect("response re-encodes")
            }
        };
        assert_eq!(
            bytes, re_encoded,
            "fixture {} did not round-trip decode→encode",
            fixture.name
        );
    }
}

#[test]
fn frame_decoder_recovers_after_payload_decode_error() {
    // Edge case: the decoder's error policy is to clear the buffer on a
    // payload-level `Decode(_)` and continue accepting subsequent frames.
    // Build a synthetic frame with a valid envelope but a nonsense bincode
    // tag (`u32::MAX`) so the payload-level decode fails. Then push a real
    // fixture and confirm the decoder still parses it.
    let mut decoder = FrameDecoder::new();

    let bogus_payload = u32::MAX.to_le_bytes();
    let mut bogus_frame = Vec::with_capacity(2 + 4 + bogus_payload.len());
    bogus_frame.push(RMUX_FRAME_MAGIC);
    bogus_frame.push(RMUX_WIRE_VERSION as u8);
    bogus_frame.extend_from_slice(&(bogus_payload.len() as u32).to_le_bytes());
    bogus_frame.extend_from_slice(&bogus_payload);
    decoder.push_bytes(&bogus_frame);
    let err = decoder
        .next_frame::<Request>()
        .expect_err("bogus tag must surface decode error");
    assert!(matches!(err, RmuxError::Decode(_)));
    assert!(
        decoder.remaining_bytes().is_empty(),
        "decoder must clear buffer after payload decode error"
    );

    let recovery = read_fixture("has_session_request");
    decoder.push_bytes(&recovery);
    let decoded: Request = decoder
        .next_frame::<Request>()
        .expect("recovery decode")
        .expect("recovery yielded a frame");
    match decoded {
        Request::HasSession(value) => assert_eq!(value.target.as_str(), "alpha"),
        other => panic!("unexpected variant after recovery: {other:?}"),
    }
}

#[test]
fn frame_decoder_clears_buffer_after_bad_magic_and_keeps_working() {
    // Edge case: bad magic clears the entire buffer (intentional — a single
    // garbled byte at the head is undistinguishable from arbitrary trailing
    // bytes from a previous protocol). After the error, pushing a fresh
    // fixture must produce a valid frame.
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&[0x00, 0x01, 0x02, 0x03]);
    let err = decoder
        .next_frame::<Request>()
        .expect_err("bad magic must error");
    assert!(matches!(err, RmuxError::BadFrameMagic(0x00)));
    assert!(
        decoder.remaining_bytes().is_empty(),
        "decoder must clear buffer after bad-magic error"
    );

    let bytes = read_fixture("has_session_request");
    decoder.push_bytes(&bytes);
    let decoded: Request = decoder
        .next_frame::<Request>()
        .expect("recovery decode")
        .expect("recovery yielded a frame");
    match decoded {
        Request::HasSession(value) => assert_eq!(value.target.as_str(), "alpha"),
        other => panic!("unexpected variant after recovery: {other:?}"),
    }
}

#[test]
fn frame_decoder_with_zero_byte_chunks_still_decodes() {
    // Edge case: pushing zero bytes is a permitted no-op and must not
    // corrupt internal state. Interleave empty pushes with byte-by-byte
    // pushes and decode a fixture.
    let bytes = read_fixture("control_mode_request");
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&[]);
    for byte in &bytes {
        decoder.push_bytes(&[]);
        decoder.push_bytes(std::slice::from_ref(byte));
    }
    decoder.push_bytes(&[]);
    let decoded: Request = decoder
        .next_frame::<Request>()
        .expect("zero-chunk decode")
        .expect("frame produced");
    match decoded {
        Request::ControlMode(value) => assert_eq!(value.mode, ControlMode::ControlControl),
        other => panic!("unexpected variant: {other:?}"),
    }
    assert!(decoder.remaining_bytes().is_empty());
}

#[test]
fn fixtures_have_no_duplicate_names_within_test_set() {
    // Edge case: protect against a future contributor pasting two fixtures
    // with the same name. The ledger has a parallel guard
    // (`fixture_names_are_unique`); this is the test-side counterpart.
    let mut names = std::collections::HashSet::new();
    for fixture in fixtures() {
        assert!(
            names.insert(fixture.name),
            "duplicate fixture name {}",
            fixture.name
        );
    }
}

#[test]
fn every_ledger_some_fixture_has_corresponding_file_on_disk() {
    // Edge case: a ledger entry may declare `fixture: Some("foo")` but the
    // corresponding `foo.bin` file may have been deleted or renamed. Walk the
    // ledger and verify every claimed fixture file actually exists and is
    // non-empty on disk.
    let root = fixture_root();
    for entry in V1_FRAME_LEDGER {
        if let Some(name) = entry.fixture {
            let path = root.join(format!("{name}.bin"));
            let metadata = std::fs::metadata(&path).unwrap_or_else(|err| {
                panic!(
                    "ledger entry {} (kind {:#06x}) references missing fixture {}: {err}",
                    entry.dto_type,
                    entry.kind.0,
                    path.display()
                )
            });
            assert!(
                metadata.is_file(),
                "fixture path {} is not a file",
                path.display()
            );
            assert!(metadata.len() > 0, "fixture {} is empty", path.display());
        }
    }
}

#[test]
fn every_v1_fixture_file_on_disk_has_a_ledger_entry() {
    // Edge case: the inverse of the previous test. If a contributor adds a
    // new `.bin` file under `tests/wire-fixtures/ledger-v1-current-wire/`
    // without registering a ledger entry, fail loudly so the wire ledger
    // remains the single source of truth.
    let root = fixture_root();
    let entries = std::fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("read fixture root {}: {err}", root.display()));
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".bin") else {
            continue;
        };
        let registered = V1_FRAME_LEDGER
            .iter()
            .any(|ledger_entry| ledger_entry.fixture == Some(stem));
        assert!(
            registered,
            "fixture file {file_name} has no ledger entry (add to V1_FRAME_LEDGER \
             or remove the file)"
        );
    }
}

#[test]
fn ledger_kinds_are_strictly_increasing_within_each_direction() {
    // Edge case: insertion order matters. If a future contributor adds an
    // entry mid-table out of order, the ledger remains technically correct
    // (kinds are still unique) but human review becomes hard. Pin the
    // canonical sort order so that mistakes are caught at test time.
    let mut last_c2s: Option<FrameKind> = None;
    let mut last_s2c: Option<FrameKind> = None;
    for entry in V1_FRAME_LEDGER {
        let last = match entry.direction {
            FrameDirection::ClientToServer => &mut last_c2s,
            FrameDirection::ServerToClient => &mut last_s2c,
        };
        if let Some(prev) = *last {
            assert!(
                entry.kind.0 > prev.0,
                "ledger {} entries are not strictly increasing: {:#06x} after {:#06x}",
                entry.direction.abbreviation(),
                entry.kind.0,
                prev.0
            );
        }
        *last = Some(entry.kind);
    }
}

#[test]
fn frame_kind_helpers_match_ledger_metadata_for_every_entry() {
    // Edge case: `FrameKind::direction()` and `FrameKind::bincode_tag()` are
    // helpers that callers will use to implement routing. Walk every entry
    // (including reserved sentinels) and assert that those helpers agree with
    // the recorded direction and that the bincode tag never aliases the
    // direction bit.
    for entry in V1_FRAME_LEDGER {
        assert_eq!(
            entry.kind.direction(),
            entry.direction,
            "kind {:#06x} ({}): direction helper disagrees with ledger",
            entry.kind.0,
            entry.dto_type,
        );
        assert!(
            entry.kind.bincode_tag() < 0x8000,
            "kind {:#06x} ({}): bincode_tag {} would collide with direction bit",
            entry.kind.0,
            entry.dto_type,
            entry.kind.bincode_tag(),
        );
        match entry.direction {
            FrameDirection::ClientToServer => assert!(
                entry.kind.0 <= 0x7FFF,
                "kind {:#06x} ({}) marked c2s but exceeds c2s ceiling",
                entry.kind.0,
                entry.dto_type,
            ),
            FrameDirection::ServerToClient => assert!(
                entry.kind.0 >= 0x8000,
                "kind {:#06x} ({}) marked s2c but lacks direction bit",
                entry.kind.0,
                entry.dto_type,
            ),
        }
    }
}

#[test]
fn fixture_set_kinds_match_manifest_table() {
    // The ledger is authoritative; this test pins the exact fixture name and
    // kind pairs used by the wire compatibility fixtures.
    let expected: &[(&str, u16)] = &[
        ("has_session_request", 0x0001),
        ("kill_session_request", 0x0002),
        ("send_keys_request", 0x0019),
        ("capture_pane_request", 0x002B),
        ("control_mode_request", 0x003F),
        ("split_window_target_action_request", 0x0076),
        ("resize_pane_target_action_request", 0x0077),
        ("capture_pane_target_action_request", 0x0078),
        ("new_session_response", 0x8000),
        ("error_response", 0x801F),
    ];
    let mut by_name: std::collections::HashMap<&str, u16> = std::collections::HashMap::new();
    for entry in V1_FRAME_LEDGER {
        if let Some(name) = entry.fixture {
            by_name.insert(name, entry.kind.0);
        }
    }
    for (name, expected_kind) in expected {
        let actual = by_name
            .get(name)
            .unwrap_or_else(|| panic!("manifest fixture {name} missing from ledger"));
        assert_eq!(
            *actual, *expected_kind,
            "fixture {name}: ledger kind {:#06x} disagrees with manifest {:#06x}",
            *actual, *expected_kind
        );
    }
    assert_eq!(
        by_name.len(),
        expected.len(),
        "ledger has {} ledger-tagged fixtures but manifest pins {}",
        by_name.len(),
        expected.len()
    );
}

#[test]
fn fixture_envelope_payload_length_lies_within_default_max() {
    // Edge case: if a future fixture grows past the default cap, the
    // released decoder would silently start rejecting it. Pin the
    // current fixtures well within the default budget.
    use rmux_proto::DEFAULT_MAX_FRAME_LENGTH;
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let length_bytes: [u8; 4] = bytes[2..6].try_into().expect("4 length bytes");
        let announced = u32::from_le_bytes(length_bytes) as usize;
        assert!(
            announced < DEFAULT_MAX_FRAME_LENGTH,
            "fixture {} payload length {} ≥ DEFAULT_MAX_FRAME_LENGTH ({})",
            fixture.name,
            announced,
            DEFAULT_MAX_FRAME_LENGTH
        );
    }
}

#[test]
fn fixture_full_frame_decodes_through_frame_decoder_with_max_length_at_payload_size() {
    // Edge case: `FrameDecoder::with_max_frame_length` is documented as the
    // payload-byte cap. Decode every fixture with the cap set to the exact
    // payload size to confirm equality is permitted (a strict `>` comparison
    // would silently reject every fixture).
    for fixture in fixtures() {
        let bytes = read_fixture(fixture.name);
        let length_bytes: [u8; 4] = bytes[2..6].try_into().expect("4 length bytes");
        let announced = u32::from_le_bytes(length_bytes) as usize;

        let mut decoder = FrameDecoder::with_max_frame_length(announced);
        decoder.push_bytes(&bytes);
        match &fixture.encode {
            Frame::Request(_) => {
                let decoded = decoder
                    .next_frame::<Request>()
                    .expect("request decodes at boundary")
                    .expect("frame at boundary");
                assert_eq!(frame_kind_for_request(&decoded), fixture.kind);
            }
            Frame::Response(_) => {
                let decoded = decoder
                    .next_frame::<Response>()
                    .expect("response decodes at boundary")
                    .expect("frame at boundary");
                assert_eq!(frame_kind_for_response(&decoded), fixture.kind);
            }
        }
    }
}

#[test]
fn frame_decoder_rejects_payload_exactly_one_byte_over_max() {
    // Edge case: pin the cap-comparison direction. A fixture with payload
    // length `n` decoded by a decoder configured with `n - 1` must surface
    // `FrameTooLarge`, not silently truncate.
    let bytes = read_fixture("send_keys_request");
    let length_bytes: [u8; 4] = bytes[2..6].try_into().expect("4 length bytes");
    let announced = u32::from_le_bytes(length_bytes) as usize;
    let mut decoder = FrameDecoder::with_max_frame_length(announced - 1);
    decoder.push_bytes(&bytes);
    let err = decoder
        .next_frame::<Request>()
        .expect_err("oversize must surface error");
    match err {
        RmuxError::FrameTooLarge { length, maximum } => {
            assert_eq!(length, announced, "FrameTooLarge.length");
            assert_eq!(maximum, announced - 1, "FrameTooLarge.maximum");
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
}

/// Regenerates the checked-in ledger fixtures.
///
/// Run via `cargo test -p rmux-proto --test wire_ledger_v1 --
/// --ignored regenerate_v1_fixtures`. The result must be reviewed and
/// committed; downstream readers consume the bytes through `decode_frame`
/// and `FrameDecoder`.
#[test]
#[ignore = "regenerator: writes binary fixtures to disk"]
fn regenerate_v1_fixtures() {
    let root = fixture_root();
    std::fs::create_dir_all(&root).expect("create fixture root");
    for fixture in fixtures() {
        let bytes = encode_fixture(&fixture);
        let path = root.join(format!("{}.bin", fixture.name));
        std::fs::write(&path, &bytes).expect("write fixture");
        eprintln!("wrote {} ({} bytes)", path.display(), bytes.len());
    }
}

// Suppress unused imports when this file is compiled in isolation.
#[allow(dead_code)]
fn _types_used(_: WindowTarget, _: TerminalSize) {}
