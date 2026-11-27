use super::*;

#[tokio::test]
async fn l05_1_sdk_ordinary_pane_drop_and_detach_are_wire_inert() {
    let (client_stream, mut peer) = tokio::io::duplex(4096);
    let transport = TransportClient::spawn(client_stream);
    let session_name = SessionName::new("inert-pane").expect("session name");
    let pane = super::super::Pane::new(
        PaneRef::new(session_name.clone(), 0, 0),
        RmuxEndpoint::Default,
        None,
        transport.clone(),
    );
    pane.clone().detach();
    drop(pane);
    // The retained transport stays usable; its first frame must be this probe,
    // never a close/kill caused by dropping either ordinary pane handle.
    let probe = tokio::spawn(async move {
        transport
            .request(Request::HasSession(HasSessionRequest {
                target: session_name,
            }))
            .await
    });
    assert!(matches!(
        read_transport_request(&mut peer).await,
        Request::HasSession(_)
    ));
    write_transport_response(
        &mut peer,
        &Response::HasSession(HasSessionResponse { exists: true }),
    )
    .await;
    assert!(matches!(
        probe.await.expect("probe task").expect("probe response"),
        Response::HasSession(_)
    ));
}
