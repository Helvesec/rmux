use std::future::{Future, IntoFuture};
use std::time::Duration;

use super::{read_transport_request, write_transport_response};
use crate::transport::{OperationDeadline, TransportClient};
use crate::web_share::value_tests::{assert_handle, assert_summary, created, scopes, summary};
use crate::web_share::{
    list_web_shares, lookup_summary, stop_all_web_shares, stop_web_share, web_config,
};
use crate::{WebConfigInfo, WebShareBuilder, WebShareHandle, WebShareLookup};
use rmux_proto::*;
use tokio::io::{AsyncReadExt, DuplexStream};

async fn exchange<T>(
    future: impl Future<Output = T>,
    server: &mut DuplexStream,
    expected: WebShareRequest,
    response: WebShareResponse,
) -> T {
    let peer = async {
        let mut request = read_transport_request(server).await.unwrap();
        if matches!(request, Request::Handshake(_)) {
            write_transport_response(
                server,
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: capabilities_for_features(true)
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                }),
            )
            .await;
            request = read_transport_request(server).await.unwrap();
        }
        assert_eq!(request, Request::WebShare(Box::new(expected)));
        write_transport_response(server, Response::WebShare(Box::new(response))).await;
    };
    let (value, ()) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(future, peer) })
            .await
            .expect("bounded local response exchange");
    value
}

fn output() -> CommandOutput {
    CommandOutput::from_stdout("inert CLI-only output")
}

fn lookup_request() -> WebShareRequest {
    WebShareRequest::Lookup(LookupWebShareRequest {
        share_id: " literal share id ".into(),
    })
}

#[tokio::test]
async fn list_lookup_fresh_summaries_and_active_counts_preserve_all_fields() {
    let (client, mut server) = tokio::io::duplex(8192);
    let transport = TransportClient::spawn(client);
    let cases = scopes();
    for variant in 0..3 {
        let values = exchange(
            list_web_shares(&transport),
            &mut server,
            WebShareRequest::List(ListWebSharesRequest),
            WebShareResponse::List(WebShareListResponse {
                shares: cases
                    .iter()
                    .map(|(wire, _)| summary(wire.clone(), variant))
                    .collect(),
                output: output(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(values.len(), 3);
        for (value, (wire, scope)) in values.iter().zip(&cases) {
            assert_summary(value, scope, variant);
            let looked_up = exchange(
                lookup_summary(&transport, " literal share id "),
                &mut server,
                lookup_request(),
                WebShareResponse::Lookup(WebShareLookupResponse {
                    share: Some(summary(wire.clone(), variant)),
                    output: output(),
                }),
            )
            .await
            .unwrap();
            assert_summary(&looked_up, scope, variant);
            let expired = transport
                .clone()
                .with_default_timeout(Some(Duration::from_secs(1)))
                .with_operation_deadline(OperationDeadline::from_timeout(Some(Duration::ZERO)));
            let lookup = WebShareLookup::new(expired.clone(), looked_up);
            let handle = WebShareHandle::new(expired, created(wire.clone(), variant));
            let next = (variant + 1) % 3;
            let response = || {
                WebShareResponse::Lookup(WebShareLookupResponse {
                    share: Some(summary(wire.clone(), next)),
                    output: output(),
                })
            };
            let fresh = exchange(lookup.summary(), &mut server, lookup_request(), response())
                .await
                .unwrap();
            assert_summary(&fresh, scope, next);
            assert_summary(lookup.cached_summary(), scope, variant);
            let fresh = exchange(handle.summary(), &mut server, lookup_request(), response())
                .await
                .unwrap();
            assert_summary(&fresh, scope, next);
            let spectators = exchange(
                handle.spectators_active(),
                &mut server,
                lookup_request(),
                response(),
            )
            .await
            .unwrap();
            let operators = exchange(
                handle.operators_active(),
                &mut server,
                lookup_request(),
                response(),
            )
            .await
            .unwrap();
            assert_eq!(spectators, [0, 3, u16::MAX][next]);
            assert_eq!(operators, [u16::MAX, 5, 0][next]);
        }
    }
    let empty = exchange(
        list_web_shares(&transport),
        &mut server,
        WebShareRequest::List(ListWebSharesRequest),
        WebShareResponse::List(WebShareListResponse {
            shares: vec![],
            output: output(),
        }),
    )
    .await
    .unwrap();
    assert!(empty.is_empty());
    let absent = exchange(
        lookup_summary(&transport, " literal share id "),
        &mut server,
        lookup_request(),
        WebShareResponse::Lookup(WebShareLookupResponse {
            share: None,
            output: output(),
        }),
    )
    .await
    .unwrap_err();
    assert!(absent.to_string().contains("web share not found"));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), server.read_u8())
            .await
            .is_err(),
        "no extra RPC"
    );
}

#[tokio::test]
async fn create_response_scope_is_authoritative_and_handles_reuse_transport() {
    for (wire, scope) in scopes() {
        for variant in 0..3 {
            let (client, mut server) = tokio::io::duplex(8192);
            let transport = TransportClient::spawn(client);
            // Deliberately different from the returned scope: never reconstruct the response
            // from a builder's requested slot, cached identity or session.
            let requested = WebShareScope::Session(SessionName::new("requested").unwrap());
            let handle = exchange(
                WebShareBuilder::new(&transport, requested.clone()).into_future(),
                &mut server,
                WebShareRequest::Create(CreateWebShareRequest {
                    scope: requested,
                    public_base_url: None,
                    tunnel_provider: None,
                    frontend_url: None,
                    ttl_seconds: None,
                    expires_at_unix: None,
                    max_spectators: None,
                    max_operators: None,
                    url_options: WebShareUrlOptions::default(),
                    require_pin: true,
                    operator_pin: None,
                    spectator_pin: None,
                    terminal_palette: None,
                    operator: true,
                    spectator: true,
                    controls: true,
                    kill_session_on_expire: false,
                }),
                WebShareResponse::Created(created(wire.clone(), variant)),
            )
            .await
            .unwrap();
            assert_handle(&handle, &scope, variant);
            let clone = handle.clone();
            drop(handle);
            // Keep the same server connection; no reconnect, handshake or create is needed.
            let stop_peer = async {
                assert_eq!(
                    read_transport_request(&mut server).await,
                    Some(Request::WebShare(Box::new(WebShareRequest::Stop(
                        StopWebShareRequest {
                            share_id: " literal share id ".into(),
                        }
                    ))))
                );
                write_transport_response(
                    &mut server,
                    Response::WebShare(Box::new(WebShareResponse::Stopped(
                        WebShareStoppedResponse {
                            share_id: " literal share id ".into(),
                            stopped: true,
                            output: output(),
                        },
                    ))),
                )
                .await;
            };
            let (stopped, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(clone.stop(), stop_peer)
            })
            .await
            .unwrap();
            stopped.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(10), server.read_u8())
                    .await
                    .is_err(),
                "no drop/stop RPC beyond explicit stop"
            );
        }
    }
}

#[tokio::test]
async fn config_and_stop_siblings_keep_exact_results() {
    let (client, mut server) = tokio::io::duplex(8192);
    let transport = TransportClient::spawn(client);
    for (host, port, origin) in [("", 0, ""), (" [::1] ", u16::MAX, " literal origin ")] {
        let result = exchange(
            web_config(&transport),
            &mut server,
            WebShareRequest::Config(WebShareConfigRequest),
            WebShareResponse::Config(WebShareConfigResponse {
                listener: WebShareListener {
                    host: host.into(),
                    port,
                    frontend_origin: origin.into(),
                },
                output: output(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            WebConfigInfo {
                host: host.into(),
                port,
                frontend_origin: origin.into()
            }
        );
    }
    for stopped in [false, true] {
        let result = exchange(
            stop_web_share(&transport, " literal share id "),
            &mut server,
            WebShareRequest::Stop(StopWebShareRequest {
                share_id: " literal share id ".into(),
            }),
            WebShareResponse::Stopped(WebShareStoppedResponse {
                share_id: " literal share id ".into(),
                stopped,
                output: output(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(result, stopped);
        let lookup = WebShareLookup::new(
            transport.clone(),
            crate::WebShareSummary::from_proto(summary(scopes().remove(0).0, 0)),
        );
        let clone = lookup.clone();
        drop(lookup);
        exchange(
            clone.stop(),
            &mut server,
            WebShareRequest::Stop(StopWebShareRequest {
                share_id: " literal share id ".into(),
            }),
            WebShareResponse::Stopped(WebShareStoppedResponse {
                share_id: " literal share id ".into(),
                stopped,
                output: output(),
            }),
        )
        .await
        .unwrap();
    }
    for stopped in [0, u32::MAX] {
        let result = exchange(
            stop_all_web_shares(&transport),
            &mut server,
            WebShareRequest::StopAll(StopAllWebSharesRequest),
            WebShareResponse::StoppedAll(WebShareStoppedAllResponse {
                stopped,
                output: output(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(result, usize::try_from(stopped).unwrap_or(usize::MAX));
    }
}
