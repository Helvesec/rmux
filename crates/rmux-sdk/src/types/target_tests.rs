use super::*;
use crate::transport::TransportClient;
use crate::{ProcessSpec, RmuxCommand, Session, SplitDirectionSpec, SplitSpec, SplitTargetSpec};
use rmux_proto::{
    encode_frame, FrameDecoder, KillWindowRequest, KillWindowResponse, NewWindowRequest,
    NewWindowResponse, PaneTarget, Request, Response, SelectWindowRequest, SelectWindowResponse,
    SplitWindowExtRequest, SplitWindowResponse, SplitWindowTarget, Target, WindowTarget,
};
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

// From f42c768's field copies, serde declaration order and protocol Display.
// These fixtures must not be inferred from candidate conversion output.
const INDICES: [(u32, u32); 7] = [
    (0, 0),
    (0, 37),
    (91, 0),
    (123, 45),
    (u32::MAX, 1),
    (1, u32::MAX),
    (u32::MAX, u32::MAX),
];
const NAMES: [&str; 3] = ["alpha", " leading $@%[];=/é雪 ", r"line\nslash\\nul\000"];

fn name(value: &str) -> SessionName {
    // Already validated bytes, including existing vis escapes. Reconstructing
    // with SessionName::new would escape the backslashes a second time.
    serde_json::from_value(serde_json::json!(value)).unwrap()
}

fn check_serde<T: Serialize + DeserializeOwned + PartialEq + Debug>(
    value: &T,
    json: &str,
    bytes: &[u8],
) {
    assert_eq!(serde_json::to_string(value).unwrap(), json);
    assert_eq!(&serde_json::from_str::<T>(json).unwrap(), value);
    assert_eq!(bincode::serialize(value).unwrap(), bytes);
    assert_eq!(&bincode::deserialize::<T>(bytes).unwrap(), value);
}

#[test]
fn selectors_preserve_fields_serde_order_variants_and_base_spelling() {
    for text in NAMES {
        let session = name(text);
        assert_eq!(session.as_str(), text);
        assert_eq!(WindowRef::first(session.clone()).window_index, 0);
        assert_eq!(
            PaneRef::in_first_window(session.clone(), u32::MAX).window_index,
            0
        );
        let quoted = serde_json::to_string(text).unwrap();
        for (w, p) in INDICES {
            let window = WindowRef::new(session.clone(), w);
            let pane = PaneRef::new(session.clone(), w, p);
            let window_json = format!("{{\"session_name\":{quoted},\"window_index\":{w}}}");
            let pane_json =
                format!("{{\"session_name\":{quoted},\"window_index\":{w},\"pane_index\":{p}}}");
            let mut session_bytes = (text.len() as u64).to_le_bytes().to_vec();
            session_bytes.extend_from_slice(text.as_bytes());
            let mut window_bytes = session_bytes.clone();
            window_bytes.extend_from_slice(&w.to_le_bytes());
            let mut pane_bytes = window_bytes.clone();
            pane_bytes.extend_from_slice(&p.to_le_bytes());
            check_serde(&window, &window_json, &window_bytes);
            check_serde(&pane, &pane_json, &pane_bytes);
            assert_eq!(window.to_string(), format!("{text}:{w}"));
            assert_eq!(pane.to_string(), format!("{text}:{w}.{p}"));
            for (index, label, target, proto, payload, bytes, spelling) in [
                (
                    0_u32,
                    "Session",
                    TargetRef::Session(session.clone()),
                    Target::Session(session.clone()),
                    quoted.clone(),
                    session_bytes,
                    text.to_owned(),
                ),
                (
                    1,
                    "Window",
                    TargetRef::Window(window),
                    Target::Window(WindowTarget::with_window(session.clone(), w)),
                    window_json,
                    window_bytes,
                    format!("{text}:{w}"),
                ),
                (
                    2,
                    "Pane",
                    TargetRef::Pane(pane),
                    Target::Pane(PaneTarget::with_window(session.clone(), w, p)),
                    pane_json,
                    pane_bytes,
                    format!("{text}:{w}.{p}"),
                ),
            ] {
                let mut expected = index.to_le_bytes().to_vec();
                expected.extend_from_slice(&bytes);
                check_serde(&target, &format!("{{\"{label}\":{payload}}}"), &expected);
                assert_eq!(bincode::serialize(&proto).unwrap(), expected);
                assert_eq!(proto.to_string(), spelling);
                assert_eq!(target.to_string(), spelling);
            }
        }
    }
}

#[test]
fn owned_and_borrowed_conversions_preserve_names_and_literal_indices() {
    for text in NAMES {
        for (w, p) in INDICES {
            let window = WindowRef::new(name(text), w);
            let ptr = window.session_name.as_str().as_ptr();
            let borrowed = window.to_proto();
            assert_eq!(borrowed, WindowTarget::with_window(name(text), w));
            assert_eq!(window.session_name.as_str().as_ptr(), ptr);
            assert_ne!(borrowed.session_name().as_str().as_ptr(), ptr);
            let owned = window.into_proto();
            assert_eq!(
                owned.session_name().as_str().as_ptr(),
                ptr,
                "owned name must move"
            );
            assert_eq!(owned, borrowed);
            assert_eq!(WindowRef::from_proto(owned), WindowRef::new(name(text), w));

            let pane = PaneRef::new(name(text), w, p);
            let ptr = pane.session_name.as_str().as_ptr();
            let borrowed = pane.to_proto();
            assert_eq!(borrowed, PaneTarget::with_window(name(text), w, p));
            assert_eq!(pane.session_name.as_str().as_ptr(), ptr);
            assert_ne!(borrowed.session_name().as_str().as_ptr(), ptr);
            let owned = pane.into_proto();
            assert_eq!(
                owned.session_name().as_str().as_ptr(),
                ptr,
                "owned name must move"
            );
            assert_eq!(owned, borrowed);
            assert_eq!(PaneRef::from_proto(owned), PaneRef::new(name(text), w, p));
        }
    }
}

async fn exchange(stream: &mut DuplexStream, expected: Request, response: Response) {
    let expected_bytes = encode_frame(&expected).unwrap();
    let mut bytes = vec![0; expected_bytes.len()];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(
        bytes, expected_bytes,
        "complete request frame and field order"
    );
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&bytes);
    assert_eq!(decoder.next_frame::<Request>().unwrap(), Some(expected));
    stream
        .write_all(&encode_frame(&response).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn window_creation_selection_and_close_preserve_response_targets_and_frames() {
    for text in NAMES {
        for (w, _) in INDICES {
            let (client, mut server) = tokio::io::duplex(8192);
            let operation = async {
                let session = Session::new(
                    name(text),
                    RmuxEndpoint::Default,
                    None,
                    TransportClient::spawn(client),
                    false,
                    None,
                );
                let window = session.new_window().await.unwrap();
                assert_eq!(window.target(), &WindowRef::new(name(NAMES[2]), w));
                window.select().await.unwrap();
                assert_eq!(
                    window.close().await.unwrap(),
                    crate::WindowCloseOutcome::Closed {
                        active: WindowRef::new(name(NAMES[1]), u32::MAX - w),
                    }
                );
            };
            let wire = async {
                exchange(
                    &mut server,
                    Request::NewWindow(Box::new(NewWindowRequest {
                        target: name(text),
                        name: None,
                        detached: false,
                        environment: None,
                        command: None,
                        process_command: None,
                        start_directory: None,
                        target_window_index: None,
                        insert_at_target: false,
                    })),
                    Response::NewWindow(NewWindowResponse {
                        target: WindowTarget::with_window(name(NAMES[2]), w),
                    }),
                )
                .await;
                exchange(
                    &mut server,
                    Request::SelectWindow(SelectWindowRequest {
                        target: WindowTarget::with_window(name(NAMES[2]), w),
                    }),
                    Response::SelectWindow(SelectWindowResponse {
                        target: WindowTarget::with_window(name(NAMES[2]), w),
                    }),
                )
                .await;
                exchange(
                    &mut server,
                    Request::KillWindow(KillWindowRequest {
                        target: WindowTarget::with_window(name(NAMES[2]), w),
                        kill_all_others: false,
                    }),
                    Response::KillWindow(KillWindowResponse {
                        target: WindowTarget::with_window(name(NAMES[1]), u32::MAX - w),
                    }),
                )
                .await;
                assert_eq!(server.read(&mut [0]).await.unwrap(), 0, "no trailing RPC");
            };
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                tokio::join!(operation, wire);
            })
            .await
            .unwrap();
        }
    }
}

#[tokio::test]
async fn split_command_values_and_explicit_raw_fixture_preserve_targets_and_fields() {
    for text in NAMES {
        for (w, p) in INDICES {
            for is_pane in [false, true] {
                let target = if is_pane {
                    SplitTargetSpec::Pane(PaneRef::new(name(text), w, p))
                } else {
                    SplitTargetSpec::Session(name(text))
                };
                let expected = if is_pane {
                    SplitWindowTarget::Pane(PaneTarget::with_window(name(text), w, p))
                } else {
                    SplitWindowTarget::Session(name(text))
                };
                assert_eq!(
                    bincode::serialize(&target).unwrap(),
                    bincode::serialize(&expected).unwrap()
                );
                assert_eq!(
                    serde_json::to_value(&target).unwrap(),
                    serde_json::to_value(&expected).unwrap()
                );
                let spec = SplitSpec {
                    target,
                    direction: SplitDirectionSpec::Horizontal,
                    before: true,
                    process: ProcessSpec {
                        command: Some(vec!["printf".into(), "x y".into()]),
                        process_command: None,
                        environment: Some(vec!["A=B=C".into(), "EMPTY=".into()]),
                    },
                };
                let expected = Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                    target: expected,
                    direction: rmux_proto::SplitDirection::Horizontal,
                    before: true,
                    command: Some(vec!["printf".into(), "x y".into()]),
                    process_command: None,
                    environment: Some(vec!["A=B=C".into(), "EMPTY=".into()]),
                    start_directory: None,
                    keep_alive_on_exit: None,
                    detached: false,
                    size: None,
                    preserve_zoom: false,
                    full_size: false,
                    stdin_payload: None,
                }));
                let (client, mut server) = tokio::io::duplex(8192);
                let response = Response::SplitWindow(SplitWindowResponse {
                    pane: PaneTarget::with_window(name(NAMES[1]), p, w),
                });
                let operation = async {
                    let transport = TransportClient::spawn(client);
                    let command = RmuxCommand::new(spec);
                    let bytes = bincode::serialize(&command).unwrap();
                    let decoded: RmuxCommand = bincode::deserialize(&bytes).unwrap();
                    assert_eq!(decoded, command);
                    let crate::RmuxCommandKind::SplitWindow(value) = decoded.command else {
                        panic!("expected split SDK value");
                    };
                    // This is an explicit raw-IPC fixture, not a product command
                    // consumer. Retain the original request/response field and
                    // frame checks without exposing a public SDK conversion.
                    let request = Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                        target: match value.target {
                            SplitTargetSpec::Session(name) => SplitWindowTarget::Session(name),
                            SplitTargetSpec::Pane(pane) => {
                                SplitWindowTarget::Pane(pane.into_proto())
                            }
                        },
                        direction: match value.direction {
                            SplitDirectionSpec::Vertical => rmux_proto::SplitDirection::Vertical,
                            SplitDirectionSpec::Horizontal => {
                                rmux_proto::SplitDirection::Horizontal
                            }
                        },
                        before: value.before,
                        command: value.process.command,
                        process_command: value
                            .process
                            .process_command
                            .map(crate::ProcessCommandSpec::into_proto),
                        environment: value.process.environment,
                        start_directory: None,
                        keep_alive_on_exit: None,
                        detached: false,
                        size: None,
                        preserve_zoom: false,
                        full_size: false,
                        stdin_payload: None,
                    }));
                    assert_eq!(transport.request(request).await.unwrap(), response);
                };
                let wire = async {
                    exchange(&mut server, expected, response.clone()).await;
                    assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
                };
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    tokio::join!(operation, wire);
                })
                .await
                .unwrap();
            }
        }
    }
}

#[tokio::test]
async fn pane_snapshot_preserves_legacy_slot_and_capable_identity_request_paths() {
    use rmux_proto::{
        CommandOutput, HandshakeRequest, HandshakeResponse, ListPanesRequest, ListPanesResponse,
        PaneSnapshotCursor, PaneSnapshotRefRequest, PaneSnapshotRequest, PaneSnapshotResponse,
        PaneTargetRef, CAPABILITY_HANDSHAKE, CAPABILITY_SDK_PANE_BY_ID,
    };
    for text in NAMES {
        for (w, p) in INDICES {
            for by_id in [false, true] {
                let (client, mut server) = tokio::io::duplex(8192);
                let operation = async {
                    let pane = crate::Pane::new(
                        PaneRef::new(name(text), w, p),
                        RmuxEndpoint::Default,
                        None,
                        TransportClient::spawn(client),
                    );
                    assert_eq!(
                        pane.proto_target_ref(),
                        PaneTargetRef::slot(PaneTarget::with_window(name(text), w, p))
                    );
                    let snapshot = pane.snapshot().await.unwrap();
                    assert_eq!(snapshot.revision, 77);
                    assert_eq!((snapshot.cols, snapshot.rows), (0, 0));
                    assert!(snapshot.cells.is_empty());
                };
                let wire = async {
                    exchange(
                        &mut server,
                        Request::ListPanes(Box::new(ListPanesRequest {
                            target: name(text),
                            target_window_index: Some(w),
                            format: Some("#{window_index}:#{pane_index}:#{pane_id}".into()),
                            filter: None,
                            sort_order: None,
                            reversed: false,
                        })),
                        Response::ListPanes(ListPanesResponse {
                            output: CommandOutput::from_stdout(format!("{w}:{p}:%42\n")),
                        }),
                    )
                    .await;
                    let mut handshake = HandshakeResponse::current();
                    if !by_id {
                        handshake
                            .capabilities
                            .retain(|capability| capability != CAPABILITY_SDK_PANE_BY_ID);
                    }
                    exchange(
                        &mut server,
                        Request::Handshake(HandshakeRequest::requiring([CAPABILITY_HANDSHAKE])),
                        Response::Handshake(handshake),
                    )
                    .await;
                    let request = if by_id {
                        Request::PaneSnapshotRef(PaneSnapshotRefRequest {
                            target: PaneTargetRef::by_id(name(text), crate::PaneId::new(42)),
                        })
                    } else {
                        Request::PaneSnapshot(PaneSnapshotRequest {
                            target: PaneTarget::with_window(name(text), w, p),
                        })
                    };
                    exchange(
                        &mut server,
                        request,
                        Response::PaneSnapshot(PaneSnapshotResponse {
                            cols: 0,
                            rows: 0,
                            cells: Vec::new(),
                            revision: 77,
                            cursor: PaneSnapshotCursor {
                                row: 0,
                                col: 0,
                                visible: false,
                                style: 0,
                            },
                        }),
                    )
                    .await;
                    assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
                };
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    tokio::join!(operation, wire);
                })
                .await
                .unwrap();
            }
        }
    }
}
