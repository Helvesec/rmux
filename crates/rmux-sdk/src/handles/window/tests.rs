use super::*;
use crate::transport::OperationDeadline;
use crate::PaneExitState;
use rmux_proto::{
    encode_frame, CommandOutput, FrameDecoder, LayoutName, ListPanesRequest, ListPanesResponse,
    ListSessionsRequest, ListSessionsResponse, ListWindowsRequest, ListWindowsResponse,
    RenameWindowResponse, ResizeWindowResponse, SelectLayoutResponse, SelectWindowResponse,
    TerminalSize, WindowListEntry, WindowTarget,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Duration;

fn alpha() -> rmux_proto::SessionName {
    rmux_proto::SessionName::new("alpha").expect("valid session")
}

fn target() -> WindowRef {
    WindowRef::new(alpha(), 2)
}

fn window(client: TransportClient) -> Window {
    Window::new(target(), RmuxEndpoint::Default, None, client)
}

async fn read_request(stream: &mut tokio::io::DuplexStream) -> Request {
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

async fn write_response(stream: &mut tokio::io::DuplexStream, response: Response) {
    let frame = encode_frame(&response).expect("response encodes");
    stream.write_all(&frame).await.expect("write response");
    stream.flush().await.expect("flush response");
}

#[tokio::test(start_paused = true)]
async fn returned_window_clears_the_deadline_of_its_creation_operation() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream).with_operation_deadline(
        OperationDeadline::from_timeout(Some(Duration::from_millis(50))),
    );
    let window = Window::new(
        target(),
        RmuxEndpoint::Default,
        Some(Duration::from_millis(50)),
        transport,
    );
    tokio::time::advance(Duration::from_millis(100)).await;

    let select = tokio::spawn(async move { window.select().await });
    let proto_target = WindowTarget::with_window(alpha(), 2);
    assert_eq!(
        read_request(&mut server_stream).await,
        Request::SelectWindow(SelectWindowRequest {
            target: proto_target.clone(),
        })
    );
    write_response(
        &mut server_stream,
        Response::SelectWindow(SelectWindowResponse {
            target: proto_target,
        }),
    )
    .await;
    select
        .await
        .expect("window request task must not panic")
        .expect("returned window starts a fresh operation");
}

#[tokio::test]
async fn window_mutation_methods_send_typed_requests() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let window = window(TransportClient::spawn(client_stream));
    let proto_target = WindowTarget::with_window(alpha(), 2);

    let rename = tokio::spawn({
        let window = window.clone();
        async move { window.rename("logs").await }
    });
    match read_request(&mut server_stream).await {
        Request::RenameWindow(request) => {
            assert_eq!(request.target, proto_target);
            assert_eq!(request.name, "logs");
        }
        request => panic!("expected rename-window, got {request:?}"),
    }
    write_response(
        &mut server_stream,
        Response::RenameWindow(RenameWindowResponse {
            target: proto_target.clone(),
        }),
    )
    .await;
    rename.await.expect("rename task").expect("rename succeeds");

    let select = tokio::spawn({
        let window = window.clone();
        async move { window.select().await }
    });
    assert_eq!(
        read_request(&mut server_stream).await,
        Request::SelectWindow(SelectWindowRequest {
            target: proto_target.clone()
        })
    );
    write_response(
        &mut server_stream,
        Response::SelectWindow(SelectWindowResponse {
            target: proto_target.clone(),
        }),
    )
    .await;
    select.await.expect("select task").expect("select succeeds");

    let resize = tokio::spawn({
        let window = window.clone();
        async move { window.resize(Some(120), Some(40)).await }
    });
    assert_eq!(
        read_request(&mut server_stream).await,
        Request::ResizeWindow(ResizeWindowRequest {
            target: proto_target.clone(),
            width: Some(120),
            height: Some(40),
            adjustment: None,
        })
    );
    write_response(
        &mut server_stream,
        Response::ResizeWindow(ResizeWindowResponse {
            target: proto_target.clone(),
        }),
    )
    .await;
    resize.await.expect("resize task").expect("resize succeeds");

    let layout = tokio::spawn({
        let window = window.clone();
        async move { window.select_layout(LayoutName::Tiled).await }
    });
    assert_eq!(
        read_request(&mut server_stream).await,
        Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Window(proto_target.clone()),
            layout: LayoutName::Tiled,
        })
    );
    write_response(
        &mut server_stream,
        Response::SelectLayout(SelectLayoutResponse {
            layout: LayoutName::Tiled,
        }),
    )
    .await;
    layout.await.expect("layout task").expect("layout succeeds");
}

#[tokio::test]
async fn window_info_reports_live_pane_process_and_exit_state() {
    let (client_stream, mut server_stream) = tokio::io::duplex(4096);
    let window = window(TransportClient::spawn(client_stream));
    let info = tokio::spawn(async move { window.info().await.expect("window info") });

    assert_eq!(
        read_request(&mut server_stream).await,
        Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}	#{session_id}".to_owned()),
            filter: None,
            sort_order: Some("name".to_owned()),
            reversed: false,
        })
    );
    write_response(
        &mut server_stream,
        Response::ListSessions(ListSessionsResponse {
            output: CommandOutput::from_stdout(
                "alpha	$1
",
            ),
        }),
    )
    .await;

    assert_eq!(
        read_request(&mut server_stream).await,
        Request::ListWindows(Box::new(ListWindowsRequest {
            target: alpha(),
            format: None,
            filter: None,
            sort_order: None,
            reversed: false,
        }))
    );
    write_response(
        &mut server_stream,
        Response::ListWindows(ListWindowsResponse {
            windows: vec![WindowListEntry {
                target: WindowTarget::with_window(alpha(), 2),
                window_id: "@7".to_owned(),
                name: None,
                pane_count: 3,
                size: TerminalSize::new(80, 24),
                layout: LayoutName::EvenHorizontal,
                active: true,
                last: false,
                rendered: String::new(),
            }],
            output: CommandOutput::from_stdout(""),
        }),
    )
    .await;

    assert_eq!(
        read_request(&mut server_stream).await,
        Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha(),
            target_window_index: Some(2),
            format: Some("#{window_index}:#{pane_index}:#{pane_id}:#{pane_active}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
    );
    write_response(
        &mut server_stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(
                "2:0:%5:1
2:1:%6:0
2:2:%7:0
",
            ),
        }),
    )
    .await;

    // The session-scoped live details probe — the same 18-field format
    // the pane-scoped snapshot uses, so every listed pane's process and
    // exit state reflects the daemon's current view.
    let details_format = "#{pane_id}	#{pane_pid}	#{pane_dead}	#{pane_dead_status}	#{pane_dead_signal}	#{pane_width}	#{pane_height}	#{cursor_x}	#{cursor_y}	#{cursor_flag}	#{cursor_shape}	#{history_bytes}	#{history_size}	#{pane_start_command}	#{pane_lifecycle_generation}	#{pane_lifecycle_revision}	#{pane_output_sequence}	#{pane_start_path}";
    assert_eq!(
        read_request(&mut server_stream).await,
        Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha(),
            target_window_index: None,
            format: Some(details_format.to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }))
    );
    let dead_line = "%5		1	3		80	24	0	0	1	0	0	0		0	0	0	";
    let running_line = "%6	41988	0			80	24	0	0	1	0	0	0		0	0	0	/tmp";
    write_response(
        &mut server_stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout(format!(
                "{dead_line}
{running_line}
"
            )),
        }),
    )
    .await;

    let snapshot = info.await.expect("info task must not panic");
    let dead = snapshot
        .pane(PaneId::new(5))
        .expect("the dead pane stays listed");
    assert_eq!(dead.process, PaneProcessState::Exited);
    assert_eq!(
        dead.exit_state,
        Some(PaneExitState {
            code: Some(3),
            signal: None,
            message: None,
        })
    );
    let running = snapshot
        .pane(PaneId::new(6))
        .expect("the running pane stays listed");
    assert_eq!(
        running.process,
        PaneProcessState::Running { pid: Some(41988) }
    );
    assert_eq!(running.working_directory.as_deref(), Some("/tmp"));
    let unprobed = snapshot
        .pane(PaneId::new(7))
        .expect("a pane the probe missed stays listed");
    assert_eq!(unprobed.process, PaneProcessState::Unknown);
    assert!(unprobed.exit_state.is_none());
}
