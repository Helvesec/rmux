use super::*;
use crate::transport::OperationDeadline;
use rmux_proto::{
    encode_frame, FrameDecoder, RenameWindowResponse, ResizeWindowResponse, SelectLayoutResponse,
    SelectWindowResponse, WindowTarget,
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
