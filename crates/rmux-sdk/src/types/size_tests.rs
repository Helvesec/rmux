use crate::transport::TransportClient;
use crate::{
    EnsureSession, EnsureSessionPolicy, InfoSnapshot, Pane, PaneId, PaneRef, Rmux, RmuxEndpoint,
    SessionId, SessionName, TerminalSizeSpec, Window, WindowId, WindowRef,
};
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, HasSessionResponse, ListPanesRequest,
    ListPanesResponse, ListSessionsRequest, ListSessionsResponse, ListWindowsRequest,
    ListWindowsResponse, NewSessionResponse, Request, Response, TerminalSize, WindowListEntry,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

// Literal u16 contract from the base's field copies and protocol DTO, including
// independent zero axes. Expectations never pass through a candidate conversion.
const SIZES: [(u16, u16); 6] = [
    (0, 0),
    (0, 37),
    (91, 0),
    (123, 45),
    (65535, 1),
    (65535, 65535),
];

fn alpha() -> SessionName {
    SessionName::new("alpha").unwrap()
}

async fn read_request(stream: &mut DuplexStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(request) = decoder.next_frame::<Request>().unwrap() {
            return request;
        }
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "client closed before the expected request");
        decoder.push_bytes(&buffer[..count]);
    }
}

async fn write_response(stream: &mut DuplexStream, response: Response) {
    stream
        .write_all(&encode_frame(&response).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn ensure_session_preserves_literal_sizes_none_defaults_and_request_order() {
    for dimensions in std::iter::once(None).chain(SIZES.map(Some)) {
        for (policy, exists) in [
            (EnsureSessionPolicy::CreateOnly, false),
            (EnsureSessionPolicy::CreateOrReuse, false),
            (EnsureSessionPolicy::CreateOrReuse, true),
            (EnsureSessionPolicy::ReuseOnly, true),
        ] {
            let (client, mut server) = tokio::io::duplex(4096);
            let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client), None);
            let mut builder = EnsureSession::named(alpha()).policy(policy);
            if let Some((cols, rows)) = dimensions {
                builder = builder.size(TerminalSizeSpec::new(cols, rows));
            }
            let operation = async {
                let session = rmux.ensure_session(builder).await.unwrap();
                assert_eq!(session.name(), &alpha());
                assert_eq!(session.was_created(), !exists);
                drop(session);
                drop(rmux);
            };
            let wire = async {
                if policy != EnsureSessionPolicy::CreateOnly {
                    match read_request(&mut server).await {
                        Request::HasSession(request) => {
                            assert_eq!(request.target.as_str(), "=alpha")
                        }
                        other => panic!("expected has-session first, got {other:?}"),
                    }
                    write_response(
                        &mut server,
                        Response::HasSession(HasSessionResponse { exists }),
                    )
                    .await;
                }
                if policy != EnsureSessionPolicy::ReuseOnly {
                    let Request::NewSessionExt(request) = read_request(&mut server).await else {
                        panic!("expected new-session without a geometry probe");
                    };
                    assert_eq!(
                        request.size,
                        dimensions.map(|(cols, rows)| TerminalSize { cols, rows })
                    );
                    assert_eq!(request.session_name, Some(alpha()));
                    assert!(request.detached);
                    assert_eq!(request.attach_if_exists, exists);
                    assert_eq!(request.working_directory, None);
                    assert_eq!(request.environment, None);
                    assert_eq!(request.group_target, None);
                    assert_eq!(request.window_name, None);
                    assert_eq!(request.command, None);
                    assert_eq!(request.process_command, None);
                    assert_eq!(request.flags, None);
                    assert_eq!(request.print_format, None);
                    assert!(!request.print_session_info);
                    assert!(!request.detach_other_clients);
                    assert!(!request.kill_other_clients);
                    assert!(!request.skip_environment_update);
                    write_response(
                        &mut server,
                        Response::NewSession(NewSessionResponse {
                            session_name: alpha(),
                            detached: true,
                            output: None,
                        }),
                    )
                    .await;
                }
                assert_eq!(
                    server.read(&mut [0_u8; 1]).await.unwrap(),
                    0,
                    "unexpected trailing request"
                );
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(operation, wire);
            })
            .await
            .unwrap();
        }
    }
}

#[tokio::test]
async fn owned_session_creation_preserves_sizes_none_and_identity_print_fields() {
    for dimensions in std::iter::once(None).chain(SIZES.map(Some)) {
        let (client, mut server) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client);
        let mut builder = EnsureSession::named(alpha()).create_only();
        if let Some((cols, rows)) = dimensions {
            builder = builder.size(TerminalSizeSpec::new(cols, rows));
        }
        let operation = async {
            let (session, id) = crate::ensure::create_owned_session(
                builder,
                &[],
                RmuxEndpoint::Default,
                None,
                transport,
            )
            .await
            .unwrap();
            assert_eq!(id, SessionId::new(7));
            assert_eq!(session.name(), &alpha());
            assert!(session.was_created());
        };
        let wire = async {
            let Request::NewSessionExt(request) = read_request(&mut server).await else {
                panic!("expected owned new-session without a geometry probe");
            };
            assert_eq!(
                request.size,
                dimensions.map(|(cols, rows)| TerminalSize { cols, rows })
            );
            assert!(request.print_session_info);
            assert_eq!(request.print_format.as_deref(), Some("#{session_id}"));
            assert!(request.detached);
            assert!(!request.attach_if_exists);
            write_response(
                &mut server,
                Response::NewSession(NewSessionResponse {
                    session_name: alpha(),
                    detached: true,
                    output: Some(CommandOutput::from_stdout("$7\n")),
                }),
            )
            .await;
            assert_eq!(server.read(&mut [0_u8; 1]).await.unwrap(), 0);
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(operation, wire);
        })
        .await
        .unwrap();
    }
}

fn sessions() -> (Request, Response) {
    (
        Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}\t#{session_id}".into()),
            filter: None,
            sort_order: Some("name".into()),
            reversed: false,
        }),
        Response::ListSessions(ListSessionsResponse {
            output: CommandOutput::from_stdout("alpha\t$7\n"),
        }),
    )
}

fn windows(size: Option<(u16, u16)>) -> (Request, Response) {
    (
        Request::ListWindows(Box::new(ListWindowsRequest {
            target: alpha(),
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        })),
        Response::ListWindows(ListWindowsResponse {
            windows: size
                .into_iter()
                .map(|(cols, rows)| WindowListEntry {
                    target: rmux_proto::WindowTarget::with_window(alpha(), 2),
                    window_id: "@9".into(),
                    name: None,
                    pane_count: 1,
                    size: TerminalSize { cols, rows },
                    layout: rmux_proto::LayoutName::Tiled,
                    active: true,
                    last: false,
                    rendered: String::new(),
                })
                .collect(),
            output: CommandOutput::from_stdout(""),
        }),
    )
}

fn panes(index: Option<u32>, format: &str, output: &str) -> (Request, Response) {
    (
        Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha(),
            target_window_index: index,
            format: Some(format.into()),
            filter: None,
            sort_order: None,
            reversed: false,
        })),
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(output),
        }),
    )
}

async fn serve(mut stream: DuplexStream, exchanges: Vec<(Request, Response)>) {
    for (expected, response) in exchanges {
        assert_eq!(read_request(&mut stream).await, expected);
        write_response(&mut stream, response).await;
    }
    assert_eq!(
        stream.read(&mut [0_u8; 1]).await.unwrap(),
        0,
        "unexpected trailing request"
    );
}

async fn info(pane_path: bool, exchanges: Vec<(Request, Response)>) -> InfoSnapshot {
    let (client, server) = tokio::io::duplex(8192);
    let operation = async {
        let transport = TransportClient::spawn(client);
        if pane_path {
            Pane::new(
                PaneRef::new(alpha(), 2, 3),
                RmuxEndpoint::Default,
                None,
                transport,
            )
            .info()
            .await
            .unwrap()
        } else {
            Window::new(
                WindowRef::new(alpha(), 2),
                RmuxEndpoint::Default,
                None,
                transport,
            )
            .info()
            .await
            .unwrap()
        }
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(operation, serve(server, exchanges)).0
    })
    .await
    .unwrap()
}

const LOCATION: &str = "#{window_index}\t#{pane_index}\t#{pane_id}\t#{session_id}\t#{window_id}";
const LIST: &str = "#{window_index}:#{pane_index}:#{pane_id}";
const DETAILS: &str = "#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_dead_status}\t#{pane_dead_signal}\t#{pane_width}\t#{pane_height}\t#{cursor_x}\t#{cursor_y}\t#{cursor_flag}\t#{cursor_shape}\t#{history_bytes}\t#{history_size}\t#{pane_start_command}\t#{pane_lifecycle_generation}\t#{pane_lifecycle_revision}\t#{pane_output_sequence}\t#{pane_start_path}";

fn assert_parent(snapshot: &InfoSnapshot, cols: u16, rows: u16) {
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].id, SessionId::new(7));
    assert_eq!(snapshot.sessions[0].size, TerminalSizeSpec::default());
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(snapshot.windows[0].id, WindowId::new(9));
    assert_eq!(snapshot.windows[0].name, None);
    assert_eq!(snapshot.windows[0].size, TerminalSizeSpec { cols, rows });
}

/// Detail sizes for the shared live details probe: `(0, 0)` falls back to
/// the window size, any other pair is published literally.
const DETAIL_SIZES: [(u16, u16); 3] = [(0, 0), (0, 17), (65535, 23)];

fn details_line(cols: u16, rows: u16) -> String {
    format!("%42\t\t0\t\t\t{cols}\t{rows}\t0\t0\t1\t0\t0\t0\t\t0\t0\t0\t\n")
}

fn expected_pane_size(window: (u16, u16), detail: (u16, u16)) -> TerminalSizeSpec {
    let (cols, rows) = if detail == (0, 0) { window } else { detail };
    TerminalSizeSpec { cols, rows }
}

#[tokio::test]
async fn window_info_preserves_literal_response_sizes() {
    for (cols, rows) in SIZES {
        for (detail_cols, detail_rows) in DETAIL_SIZES {
            let snapshot = info(
                false,
                vec![
                    sessions(),
                    windows(Some((cols, rows))),
                    panes(
                        Some(2),
                        "#{window_index}:#{pane_index}:#{pane_id}:#{pane_active}",
                        "2:3:%42:1\n",
                    ),
                    // Window::info decorates its panes with the same
                    // session-scoped live details probe as Pane::info.
                    panes(None, DETAILS, &details_line(detail_cols, detail_rows)),
                ],
            )
            .await;
            assert_parent(&snapshot, cols, rows);
            assert_eq!(snapshot.panes.len(), 1);
            assert_eq!(snapshot.panes[0].id, PaneId::new(42));
            assert_eq!(
                snapshot.panes[0].size,
                expected_pane_size((cols, rows), (detail_cols, detail_rows))
            );
        }
    }
}

#[tokio::test]
async fn pane_info_preserves_response_sizes_and_existing_detail_fallback() {
    for (cols, rows) in SIZES {
        for (detail_cols, detail_rows) in DETAIL_SIZES {
            let details = details_line(detail_cols, detail_rows);
            let snapshot = info(
                true,
                vec![
                    panes(Some(2), LOCATION, "2\t3\t%42\t$7\t@9\n"),
                    panes(None, LOCATION, "2\t3\t%42\t$7\t@9\n"),
                    sessions(),
                    windows(Some((cols, rows))),
                    panes(Some(2), LIST, "2:3:%42\n"),
                    panes(None, DETAILS, &details),
                    sessions(),
                    windows(Some((cols, rows))),
                    panes(Some(2), LIST, "2:3:%42\n"),
                ],
            )
            .await;
            assert_parent(&snapshot, cols, rows);
            assert_eq!(snapshot.panes.len(), 1);
            assert_eq!(snapshot.panes[0].id, PaneId::new(42));
            assert_eq!(
                snapshot.panes[0].size,
                expected_pane_size((cols, rows), (detail_cols, detail_rows))
            );
            assert_eq!(snapshot.panes[0].command, None);
            assert_eq!(snapshot.panes[0].working_directory, None);
        }
    }
}

#[tokio::test]
async fn absent_pane_preserves_parent_sizes_and_missing_window_defaults() {
    for (cols, rows) in SIZES {
        let snapshot = info(
            true,
            vec![
                panes(Some(2), LOCATION, ""),
                sessions(),
                windows(Some((cols, rows))),
            ],
        )
        .await;
        assert_parent(&snapshot, cols, rows);
        assert!(snapshot.panes.is_empty());
    }
    for pane_path in [false, true] {
        let mut exchanges = Vec::new();
        if pane_path {
            exchanges.push(panes(Some(2), LOCATION, ""));
        }
        exchanges.extend([sessions(), windows(None)]);
        let snapshot = info(pane_path, exchanges).await;
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].size, TerminalSizeSpec::default());
        assert!(snapshot.windows.is_empty());
        assert!(snapshot.panes.is_empty());
    }
}
