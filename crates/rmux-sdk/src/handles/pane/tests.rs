use super::info::{
    derive_exit_state, derive_process_state, pane_size_from_details, parse_details_line,
    revision_from_details, LiveDetails,
};
use super::snapshot::{cell_from_wire, snapshot_from_response};
use super::target::{is_already_closed_error, is_already_closed_pane_id_error, TargetSelector};
use crate::transport::{OperationDeadline, TransportClient};
use crate::{
    PaneAttributes, PaneColor, PaneId, PaneProcessState, PaneRef, RmuxEndpoint, RmuxError,
    TerminalSizeSpec,
};
use rmux_proto::{
    encode_frame, FrameDecoder, HandshakeResponse, HasSessionRequest, HasSessionResponse,
    PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotResponse, PaneTarget, Request, Response,
    SessionName, SplitWindowIdentityResponse, CAPABILITY_SDK_PANE_SPLIT_IDENTITY,
    RMUX_WIRE_VERSION, SUPPORTED_CAPABILITIES,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(start_paused = true)]
async fn returned_pane_clears_the_deadline_of_its_creation_operation() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream).with_operation_deadline(
        OperationDeadline::from_timeout(Some(Duration::from_millis(50))),
    );
    let session_name = SessionName::new("alpha").expect("valid session");
    let pane = super::Pane::new(
        PaneRef::new(session_name.clone(), 0, 0),
        RmuxEndpoint::Default,
        Some(Duration::from_millis(50)),
        transport,
    );
    tokio::time::advance(Duration::from_millis(100)).await;

    let mut request = tokio::spawn(async move {
        pane.transport()
            .begin_operation()
            .request(Request::HasSession(HasSessionRequest {
                target: session_name,
            }))
            .await
    });
    tokio::select! {
        result = &mut request => panic!("returned pane retained an expired deadline: {result:?}"),
        wire_request = read_transport_request(&mut server_stream) => {
            assert!(matches!(wire_request, Request::HasSession(_)));
        }
    }
    write_transport_response(
        &mut server_stream,
        &Response::HasSession(HasSessionResponse { exists: true }),
    )
    .await;
    assert_eq!(
        request
            .await
            .expect("pane request task must not panic")
            .expect("returned pane starts a fresh operation"),
        Response::HasSession(HasSessionResponse { exists: true })
    );
}

#[tokio::test]
async fn split_uses_one_atomic_mutation_rpc_without_display_message_follow_up() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    let session_name = SessionName::new("alpha").expect("valid session");
    let pane = super::Pane::new(
        PaneRef::new(session_name.clone(), 0, 4),
        RmuxEndpoint::Default,
        None,
        transport,
    );

    let split = tokio::spawn(async move { pane.split(crate::SplitDirection::Right).await });
    let handshake = read_transport_request(&mut server_stream).await;
    assert!(matches!(handshake, Request::Handshake(_)));
    write_transport_response(
        &mut server_stream,
        &Response::Handshake(HandshakeResponse::current()),
    )
    .await;

    let mutation = read_transport_request(&mut server_stream).await;
    let Request::SplitWindowIdentity(request) = mutation else {
        panic!("expected atomic split request, got {mutation:?}");
    };
    assert_eq!(request.action.target.as_deref(), Some("alpha:0.4"));
    write_transport_response(
        &mut server_stream,
        &Response::SplitWindowIdentity(SplitWindowIdentityResponse {
            pane: PaneTarget::with_window(session_name.clone(), 0, 9),
            pane_id: PaneId::new(42),
        }),
    )
    .await;

    let returned = split
        .await
        .expect("split task must not panic")
        .expect("atomic split succeeds");
    assert_eq!(returned.target(), &PaneRef::new(session_name, 0, 9));
    assert_eq!(returned.stable_id, Some(PaneId::new(42)));
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            read_transport_request(&mut server_stream)
        )
        .await
        .is_err(),
        "split must not issue a post-mutation display-message RPC"
    );
}

#[tokio::test]
async fn split_rejects_missing_identity_capability_before_mutation() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    let keepalive = transport.clone();
    let session_name = SessionName::new("alpha").expect("valid session");
    let pane = super::Pane::new(
        PaneRef::new(session_name, 0, 0),
        RmuxEndpoint::Default,
        None,
        transport,
    );

    let split = tokio::spawn(async move { pane.split(crate::SplitDirection::Right).await });
    assert!(matches!(
        read_transport_request(&mut server_stream).await,
        Request::Handshake(_)
    ));
    let capabilities = SUPPORTED_CAPABILITIES
        .iter()
        .copied()
        .filter(|capability| *capability != CAPABILITY_SDK_PANE_SPLIT_IDENTITY)
        .map(str::to_owned)
        .collect();
    write_transport_response(
        &mut server_stream,
        &Response::Handshake(HandshakeResponse {
            wire_version: RMUX_WIRE_VERSION,
            capabilities,
        }),
    )
    .await;

    let error = split
        .await
        .expect("split task must not panic")
        .expect_err("missing identity capability must fail closed");
    assert!(error
        .to_string()
        .contains(CAPABILITY_SDK_PANE_SPLIT_IDENTITY));
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            read_transport_request(&mut server_stream)
        )
        .await
        .is_err(),
        "capability rejection must happen before the split mutation"
    );
    drop(keepalive);
}

async fn read_transport_request(stream: &mut tokio::io::DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 256];
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }
        let read = stream.read(&mut buffer).await.expect("read request");
        assert_ne!(read, 0, "client closed before request");
        decoder.push_bytes(&buffer[..read]);
    }
}

async fn write_transport_response(stream: &mut tokio::io::DuplexStream, response: &Response) {
    let frame = encode_frame(response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

fn details_with(history_bytes: u64) -> LiveDetails {
    LiveDetails {
        cols: 80,
        rows: 24,
        history_bytes,
        ..LiveDetails::default()
    }
}

#[test]
fn revision_from_details_changes_with_history_bytes() {
    let r1 = revision_from_details(&details_with(10));
    let r2 = revision_from_details(&details_with(11));
    assert_ne!(r1, r2);
}

#[test]
fn revision_from_details_is_never_zero() {
    assert_ne!(revision_from_details(&LiveDetails::default()), 0);
}

#[test]
fn parse_details_line_handles_empty_optional_fields() {
    let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\t\t0\t0\t0\t/tmp";
    let details = parse_details_line(line).expect("parses");
    assert_eq!(details.pane_id.unwrap().to_string(), "%2");
    assert_eq!(details.pid, Some(1234));
    assert!(!details.dead);
    assert_eq!(details.dead_status, None);
    assert_eq!(details.dead_signal, None);
    assert_eq!(details.cols, 80);
    assert_eq!(details.rows, 24);
    assert_eq!(details.cursor_x, 10);
    assert_eq!(details.cursor_y, 5);
    assert!(details.cursor_visible);
    assert_eq!(details.history_bytes, 128);
    assert_eq!(details.history_size, 4);
    assert_eq!(details.current_path.as_deref(), Some("/tmp"));
}

#[test]
fn parse_details_line_returns_default_for_blank_or_short_input() {
    assert_eq!(
        parse_details_line("").expect("blank"),
        LiveDetails::default()
    );
    assert_eq!(
        parse_details_line("only\tone\ttwo").expect("short"),
        LiveDetails::default()
    );
}

#[test]
fn parse_details_line_preserves_tabs_inside_current_path() {
    let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\t\t0\t0\t0\t/tmp/odd\tdir\twith\ttabs";
    let details = parse_details_line(line).expect("parses");
    assert_eq!(
        details.current_path.as_deref(),
        Some("/tmp/odd\tdir\twith\ttabs")
    );
}

#[test]
fn parse_details_line_decodes_sticky_lifecycle_fields_without_env() {
    let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\tprintf\x1falpha%09beta%25\
             \t3\t5\t7\t/tmp/start";
    let details = parse_details_line(line).expect("parses");
    assert_eq!(
        details.start_command.as_deref(),
        Some(["printf".to_owned(), "alpha\tbeta%".to_owned()].as_slice())
    );
    assert_eq!(details.generation, 3);
    assert_eq!(details.lifecycle_revision, 5);
    assert_eq!(details.output_sequence, 7);
    assert_eq!(details.current_path.as_deref(), Some("/tmp/start"));
}

#[test]
fn parse_details_line_decodes_tmux_quoted_shell_command() {
    let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\t\"sleep 60\"\
             \t3\t5\t7\t/tmp/start";
    let details = parse_details_line(line).expect("parses");
    assert_eq!(
        details.start_command.as_deref(),
        Some(["sleep 60".to_owned()].as_slice())
    );
}

#[test]
fn parse_details_line_rejects_malformed_encoded_command() {
    let line = "%2\t1234\t0\t\t\t80\t24\t10\t5\t1\t0\t128\t4\tbad%XX\t1\t1\t1\t/tmp";
    assert!(parse_details_line(line).is_err());
}

#[test]
fn revision_from_details_changes_when_pane_id_changes() {
    let mut alpha = LiveDetails {
        cols: 80,
        rows: 24,
        ..LiveDetails::default()
    };
    alpha.pane_id = Some(PaneId::new(1));
    let mut beta = alpha.clone();
    beta.pane_id = Some(PaneId::new(2));
    assert_ne!(revision_from_details(&alpha), revision_from_details(&beta));
}

#[test]
fn pane_ref_target_selector_recognizes_session_invalidation() {
    let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 3, 1);
    assert!(target.matches_invalid_target("alpha:3.1", "pane index does not exist in session"));
    assert!(target.matches_invalid_target("alpha:3", "window index does not exist in session"));
    assert!(!target.matches_invalid_target("alpha:3.1", "pane index does not exist in window"));
    assert!(!target.matches_invalid_target("alpha:9", "window index does not exist in session"));
}

#[test]
fn is_already_closed_error_matches_session_not_found_for_target_session() {
    let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 0, 0);
    let error = RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound("alpha".to_owned()));
    assert!(is_already_closed_error(&error, &target));
}

#[test]
fn is_already_closed_error_does_not_match_session_not_found_for_other_session() {
    let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 0, 0);
    let error = RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound("beta".to_owned()));
    assert!(!is_already_closed_error(&error, &target));
}

#[test]
fn is_already_closed_error_matches_invalid_window_or_pane_target() {
    let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 5, 2);
    let pane_invalid = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
        value: "alpha:5.2".to_owned(),
        reason: "pane index does not exist in session".to_owned(),
    });
    let window_invalid = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
        value: "alpha:5".to_owned(),
        reason: "window index does not exist in session".to_owned(),
    });
    assert!(is_already_closed_error(&pane_invalid, &target));
    assert!(is_already_closed_error(&window_invalid, &target));
}

#[test]
fn is_already_closed_error_ignores_unrelated_protocol_errors() {
    let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 0, 0);
    let error = RmuxError::protocol(rmux_proto::RmuxError::Server(
        "daemon malfunction".to_owned(),
    ));
    assert!(!is_already_closed_error(&error, &target));
}

#[test]
fn is_already_closed_error_ignores_invalid_target_for_other_slot() {
    let target = PaneRef::new(rmux_proto::SessionName::new("alpha").unwrap(), 5, 2);
    let foreign = RmuxError::protocol(rmux_proto::RmuxError::InvalidTarget {
        value: "beta:0.0".to_owned(),
        reason: "pane index does not exist in session".to_owned(),
    });
    assert!(!is_already_closed_error(&foreign, &target));
}

#[test]
fn pane_id_closed_error_matches_only_the_resolved_identity() {
    let session_name = rmux_proto::SessionName::new("alpha").unwrap();
    let pane_id = PaneId::new(7);
    let exact = RmuxError::pane_not_found(session_name.clone(), pane_id);
    assert!(is_already_closed_pane_id_error(
        &exact,
        &session_name,
        pane_id
    ));

    let other_pane = RmuxError::pane_not_found(session_name.clone(), PaneId::new(8));
    assert!(!is_already_closed_pane_id_error(
        &other_pane,
        &session_name,
        pane_id
    ));
    let other_session = rmux_proto::SessionName::new("beta").unwrap();
    let foreign = RmuxError::pane_not_found(other_session, pane_id);
    assert!(!is_already_closed_pane_id_error(
        &foreign,
        &session_name,
        pane_id
    ));
}

#[test]
fn derive_exit_state_treats_signal_zero_as_absent() {
    let details = LiveDetails {
        dead: true,
        dead_status: Some(7),
        dead_signal: Some(0),
        ..LiveDetails::default()
    };
    let exit = derive_exit_state(&details).expect("dead pane has exit state");
    assert_eq!(exit.code, Some(7));
    assert!(exit.signal.is_none());
}

#[test]
fn derive_exit_state_returns_none_for_live_pane() {
    let details = LiveDetails {
        dead: false,
        dead_status: Some(7),
        dead_signal: Some(15),
        ..LiveDetails::default()
    };
    assert!(derive_exit_state(&details).is_none());
}

#[test]
fn derive_process_state_running_carries_pid_when_present() {
    let details = LiveDetails {
        pid: Some(42),
        ..LiveDetails::default()
    };
    match derive_process_state(&details) {
        PaneProcessState::Running { pid: Some(42) } => {}
        other => panic!("expected Running with pid 42, got {other:?}"),
    }
}

#[test]
fn derive_process_state_unknown_when_pid_missing_and_alive() {
    assert!(matches!(
        derive_process_state(&LiveDetails::default()),
        PaneProcessState::Unknown
    ));
}

#[test]
fn pane_size_falls_back_to_window_when_details_are_zero() {
    let details = LiveDetails::default();
    let fallback = TerminalSizeSpec::new(80, 24);
    assert_eq!(pane_size_from_details(&details, &fallback), fallback);
}

#[test]
fn pane_size_uses_details_when_present() {
    let details = LiveDetails {
        cols: 132,
        rows: 50,
        ..LiveDetails::default()
    };
    let fallback = TerminalSizeSpec::new(80, 24);
    assert_eq!(
        pane_size_from_details(&details, &fallback),
        TerminalSizeSpec::new(132, 50)
    );
}

#[test]
fn parse_details_line_rejects_malformed_pane_id_prefix() {
    let line = "no-prefix\t1\t0\t\t\t1\t1\t0\t0\t1\t0\t0\t0\t\t0\t0\t0\t/tmp";
    assert!(parse_details_line(line).is_err());
}

#[test]
fn parse_details_line_treats_unset_cursor_visibility_as_visible() {
    let line = "%1\t1\t0\t\t\t1\t1\t0\t0\t\t0\t0\t0\t\t0\t0\t0\t/tmp";
    let details = parse_details_line(line).expect("parses");
    assert!(details.cursor_visible);
}

fn wire_glyph_cell(text: &str, width: u8) -> PaneSnapshotCell {
    PaneSnapshotCell {
        text: text.to_owned(),
        width,
        padding: false,
        attributes: 0,
        fg: PaneColor::DEFAULT_ENCODING,
        bg: PaneColor::DEFAULT_ENCODING,
        us: PaneColor::DEFAULT_ENCODING,
        link: 0,
    }
}

fn wire_padding_cell() -> PaneSnapshotCell {
    PaneSnapshotCell {
        text: " ".to_owned(),
        width: 0,
        padding: true,
        attributes: 0,
        fg: PaneColor::DEFAULT_ENCODING,
        bg: PaneColor::DEFAULT_ENCODING,
        us: PaneColor::DEFAULT_ENCODING,
        link: 0,
    }
}

#[test]
fn cell_from_wire_preserves_padding_metadata() {
    let cell = cell_from_wire(wire_padding_cell());
    assert!(cell.is_padding());
    assert_eq!(cell.glyph.width, 0);
    // Padding markers travel with the rmux-core sentinel space text
    // verbatim — the SDK never substitutes a different glyph payload.
    assert_eq!(cell.glyph.text, " ");
}

#[test]
fn cell_from_wire_decodes_attributes_and_colors() {
    let wire = PaneSnapshotCell {
        text: "x".to_owned(),
        width: 1,
        padding: false,
        attributes: PaneAttributes::BOLD.bits() | PaneAttributes::UNDERLINE.bits(),
        fg: PaneColor::ansi(3).encoded(),
        bg: PaneColor::indexed(200).encoded(),
        us: PaneColor::rgb(10, 20, 30).encoded(),
        link: 7,
    };
    let cell = cell_from_wire(wire);
    assert!(!cell.is_padding());
    assert_eq!(cell.text(), "x");
    assert!(cell.attributes.contains(PaneAttributes::BOLD));
    assert!(cell.attributes.contains(PaneAttributes::UNDERLINE));
    assert_eq!(cell.foreground, PaneColor::ansi(3));
    assert_eq!(cell.background, PaneColor::indexed(200));
    assert_eq!(cell.underline, PaneColor::rgb(10, 20, 30));
}

#[test]
fn cell_from_wire_keeps_wide_glyph_width() {
    let cell = cell_from_wire(wire_glyph_cell("漢", 2));
    assert!(!cell.is_padding());
    assert_eq!(cell.glyph.width, 2);
    assert_eq!(cell.text(), "漢");
}

#[test]
fn snapshot_from_response_carries_cells_cursor_and_revision() {
    let response = PaneSnapshotResponse {
        cols: 2,
        rows: 1,
        cells: vec![wire_glyph_cell("a", 1), wire_glyph_cell("b", 1)],
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 1,
            visible: true,
            style: 4,
        },
        revision: 0xCAFE_BEEF,
    };
    let snapshot = snapshot_from_response(response).expect("valid wire shape");
    assert_eq!(snapshot.cols, 2);
    assert_eq!(snapshot.rows, 1);
    assert!(snapshot.is_row_major_shape());
    assert_eq!(snapshot.cells[0].text(), "a");
    assert_eq!(snapshot.cells[1].text(), "b");
    assert_eq!(snapshot.cursor.col, 1);
    assert_eq!(snapshot.cursor.style, 4);
    assert!(snapshot.cursor.visible);
    assert_eq!(snapshot.revision, 0xCAFE_BEEF);
}

#[test]
fn snapshot_from_response_handles_zero_dimensions() {
    let response = PaneSnapshotResponse {
        cols: 0,
        rows: 0,
        cells: Vec::new(),
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision: 0,
    };
    let snapshot = snapshot_from_response(response).expect("valid zero-size wire shape");
    assert!(snapshot.is_row_major_shape());
    assert_eq!(snapshot.revision, 0);
}

#[test]
fn snapshot_from_response_rejects_malformed_wire_shape() {
    let response = PaneSnapshotResponse {
        cols: 2,
        rows: 2,
        cells: vec![wire_glyph_cell("a", 1)],
        cursor: PaneSnapshotCursor {
            row: 0,
            col: 0,
            visible: true,
            style: 0,
        },
        revision: 1,
    };

    let error = snapshot_from_response(response).expect_err("shape mismatch is protocol error");
    assert!(
        error
            .to_string()
            .contains("pane-snapshot response had malformed row-major cell shape"),
        "unexpected error: {error}"
    );
}
