use super::{read_request, write_response};
use crate::transport::TransportClient;
use crate::web_share::value_tests::{assert_handle, created, scopes};
use crate::{Pane, PaneId, PaneRef, RmuxEndpoint, RmuxError, SessionName};
use rmux_proto::{
    CommandOutput, HandshakeResponse, ListPanesResponse, PaneTargetRef, Request, Response,
    WebShareRequest, WebShareResponse, WebShareScope, CAPABILITY_WEB_SHARE,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, DuplexStream};

fn pane(transport: TransportClient, stable: bool, timeout: Option<Duration>) -> Pane {
    let target = PaneRef::new(SessionName::new("alpha").unwrap(), 1, 3);
    if stable {
        Pane::new_by_id(
            target,
            PaneId::new(7),
            RmuxEndpoint::Default,
            timeout,
            transport,
        )
    } else {
        Pane::new(target, RmuxEndpoint::Default, timeout, transport)
    }
}

async fn handshake(server: &mut DuplexStream, available: bool) {
    assert!(matches!(read_request(server).await, Request::Handshake(_)));
    let mut response = HandshakeResponse::current();
    response.capabilities.retain(|c| c != CAPABILITY_WEB_SHARE);
    if available {
        response.capabilities.push(CAPABILITY_WEB_SHARE.into());
    }
    write_response(server, Response::Handshake(response)).await;
}

async fn no_more_requests(server: &mut DuplexStream) {
    let mut trailing = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), server.read_to_end(&mut trailing))
        .await
        .unwrap()
        .unwrap();
    assert!(trailing.is_empty(), "no extra RPC after failure/drop");
}

#[tokio::test]
async fn capability_failure_precedes_identity_and_create_for_both_pane_handles() {
    for stable in [false, true] {
        let (client, mut server) = tokio::io::duplex(8192);
        let pane = pane(TransportClient::spawn(client), stable, None);
        let task = tokio::spawn(async move { pane.share().await });
        handshake(&mut server, false).await;
        let Err(error) = task.await.unwrap() else {
            panic!("missing capability must fail")
        };
        assert!(
            matches!(error, RmuxError::Unsupported { feature, .. } if feature == CAPABILITY_WEB_SHARE)
        );
        no_more_requests(&mut server).await;
    }
}

#[tokio::test]
async fn absent_slot_never_creates_a_share() {
    let (client, mut server) = tokio::io::duplex(8192);
    let pane = pane(TransportClient::spawn(client), false, None);
    let task = tokio::spawn(async move { pane.share().await });
    handshake(&mut server, true).await;
    assert!(matches!(
        read_request(&mut server).await,
        Request::ListPanes(_)
    ));
    write_response(
        &mut server,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("1:4:%8\n"),
        }),
    )
    .await;
    let Err(error) = task.await.unwrap() else {
        panic!("absent slot must fail")
    };
    assert!(error.to_string().contains("pane index does not exist"));
    no_more_requests(&mut server).await;
}

#[tokio::test]
async fn both_pane_entry_paths_resolve_identity_then_map_successful_create() {
    for stable in [false, true] {
        for (returned, expected) in scopes() {
            let (client, mut server) = tokio::io::duplex(8192);
            let pane = pane(TransportClient::spawn(client), stable, None);
            let task = tokio::spawn(async move { pane.share().await });
            handshake(&mut server, true).await;
            let Request::ListPanes(request) = read_request(&mut server).await else {
                panic!("identity before create")
            };
            assert_eq!(request.target.as_str(), "alpha");
            assert_eq!(
                request.target_window_index,
                if stable { None } else { Some(1) }
            );
            write_response(
                &mut server,
                Response::ListPanes(ListPanesResponse {
                    output: CommandOutput::from_stdout("1:3:%7\n"),
                }),
            )
            .await;
            let Request::WebShare(request) = read_request(&mut server).await else {
                panic!("create after identity")
            };
            let WebShareRequest::Create(request) = *request else {
                panic!("create")
            };
            assert_eq!(
                request.scope,
                WebShareScope::Pane(PaneTargetRef::by_id(
                    SessionName::new("alpha").unwrap(),
                    PaneId::new(7)
                ))
            );
            assert!(!request.controls);
            write_response(
                &mut server,
                Response::WebShare(Box::new(WebShareResponse::Created(created(returned, 2)))),
            )
            .await;
            let handle = task.await.unwrap().unwrap();
            assert_handle(&handle, &expected, 2);
            drop(handle);
            no_more_requests(&mut server).await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn identity_resolution_shares_the_creation_deadline() {
    let (client, mut server) = tokio::io::duplex(8192);
    let pane = pane(
        TransportClient::spawn(client),
        false,
        Some(Duration::from_millis(100)),
    );
    let task = tokio::spawn(async move { pane.share().await });
    assert!(matches!(
        read_request(&mut server).await,
        Request::Handshake(_)
    ));
    tokio::time::advance(Duration::from_millis(90)).await;
    let mut response = HandshakeResponse::current();
    if !response
        .capabilities
        .iter()
        .any(|c| c == CAPABILITY_WEB_SHARE)
    {
        response.capabilities.push(CAPABILITY_WEB_SHARE.into());
    }
    write_response(&mut server, Response::Handshake(response)).await;
    assert!(matches!(
        read_request(&mut server).await,
        Request::ListPanes(_)
    ));
    tokio::time::advance(Duration::from_millis(20)).await;
    let Err(error) = task.await.unwrap() else {
        panic!("one creation deadline")
    };
    assert!(
        matches!(error, RmuxError::Transport { source, .. } if source.kind() == std::io::ErrorKind::TimedOut)
    );
    no_more_requests(&mut server).await;
}
