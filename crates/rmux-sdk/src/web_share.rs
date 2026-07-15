//! Browser-visible pane sharing helpers.

mod builder;
mod handle;
mod types;

pub use builder::WebShareBuilder;
pub use handle::{WebShareHandle, WebShareLookup};
pub use types::{WebConfigInfo, WebShareSummary};

use rmux_proto::{
    ListWebSharesRequest, LookupWebShareRequest, Request, Response, StopAllWebSharesRequest,
    StopWebShareRequest, WebShareConfigRequest, WebShareRequest, WebShareResponse,
    CAPABILITY_WEB_SHARE,
};

use crate::transport::TransportClient;
use crate::{Result, RmuxError};

async fn list_web_shares(transport: &TransportClient) -> Result<Vec<WebShareSummary>> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(Box::new(WebShareRequest::List(
            ListWebSharesRequest,
        ))))
        .await?;
    match response {
        Response::WebShare(response) => match *response {
            WebShareResponse::List(response) => {
                Ok(response.shares.into_iter().map(Into::into).collect())
            }
            other => Err(unexpected_response(
                "web-share list",
                Response::WebShare(Box::new(other)),
            )),
        },
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share list", response)),
    }
}

async fn stop_web_share(transport: &TransportClient, id: &str) -> Result<bool> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(Box::new(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: id.to_owned(),
            },
        ))))
        .await?;
    match response {
        Response::WebShare(response) => match *response {
            WebShareResponse::Stopped(response) => Ok(response.stopped),
            other => Err(unexpected_response(
                "web-share stop",
                Response::WebShare(Box::new(other)),
            )),
        },
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share stop", response)),
    }
}

async fn stop_all_web_shares(transport: &TransportClient) -> Result<usize> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(Box::new(WebShareRequest::StopAll(
            StopAllWebSharesRequest,
        ))))
        .await?;
    match response {
        Response::WebShare(response) => match *response {
            WebShareResponse::StoppedAll(response) => {
                Ok(usize::try_from(response.stopped).unwrap_or(usize::MAX))
            }
            other => Err(unexpected_response(
                "web-share stop-all",
                Response::WebShare(Box::new(other)),
            )),
        },
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share stop-all", response)),
    }
}

async fn lookup_summary(transport: &TransportClient, id: &str) -> Result<WebShareSummary> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(Box::new(WebShareRequest::Lookup(
            LookupWebShareRequest {
                share_id: id.to_owned(),
            },
        ))))
        .await?;
    match response {
        Response::WebShare(response) => match *response {
            WebShareResponse::Lookup(response) => response.share.map(Into::into).ok_or_else(|| {
                RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "web share not found".to_owned(),
                ))
            }),
            other => Err(unexpected_response(
                "web-share lookup",
                Response::WebShare(Box::new(other)),
            )),
        },
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share lookup", response)),
    }
}

async fn web_config(transport: &TransportClient) -> Result<WebConfigInfo> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(Box::new(WebShareRequest::Config(
            WebShareConfigRequest,
        ))))
        .await?;
    match response {
        Response::WebShare(response) => match *response {
            WebShareResponse::Config(response) => Ok(response.listener.into()),
            other => Err(unexpected_response(
                "web-share config",
                Response::WebShare(Box::new(other)),
            )),
        },
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share config", response)),
    }
}

async fn require_web_share(transport: &TransportClient) -> Result<()> {
    crate::capabilities::require(transport, &[CAPABILITY_WEB_SHARE]).await
}

fn token_from_url(url: &str) -> Option<&str> {
    let fragment = url.split_once('#')?.1;
    fragment.split('&').find_map(|param| {
        let (key, value) = param.split_once('=')?;
        (key == "t").then_some(value)
    })
}

fn unexpected_response(operation: &str, response: Response) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent `{}` response for {operation}",
        response.command_name()
    )))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rmux_proto::{
        capabilities_for_features, encode_frame, CommandOutput, FrameDecoder, HandshakeResponse,
        Request, Response, SessionName, WebShareCreatedResponse, WebShareResponse, WebShareScope,
        WebShareStoppedResponse, RMUX_WIRE_VERSION,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{token_from_url, WebShareHandle};
    use crate::transport::{OperationDeadline, TransportClient};

    #[test]
    fn token_from_url_reads_current_web_share_fragment_contract() {
        let url = "https://share.rmux.io/#e=ws://127.0.0.1:9777/share&t=abc123&theme=dark";
        assert_eq!(token_from_url(url), Some("abc123"));
    }

    #[tokio::test]
    async fn returned_web_share_clears_the_creation_operation_deadline() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);
        let transport = TransportClient::spawn(client_stream)
            .with_default_timeout(Some(Duration::from_secs(1)))
            .with_operation_deadline(OperationDeadline::from_timeout(Some(
                Duration::from_millis(10),
            )));
        let handle = WebShareHandle::new(
            transport,
            WebShareCreatedResponse {
                share_id: "share-1".to_owned(),
                scope: WebShareScope::Session(
                    SessionName::new("alpha").expect("valid session name"),
                ),
                spectator_url: None,
                operator_url: None,
                tunnel_provider: None,
                tunnel_public_url: None,
                expires_at_unix: None,
                operator_pairing_code: None,
                spectator_pairing_code: None,
                max_spectators: None,
                max_operators: None,
                operator: false,
                spectator: true,
                controls: false,
                kill_session_on_expire: false,
                output: CommandOutput::from_stdout(Vec::new()),
            },
        );
        tokio::time::sleep(Duration::from_millis(20)).await;

        let mut stop = tokio::spawn(async move { handle.stop().await });
        let request = tokio::select! {
            result = &mut stop => panic!("returned web-share retained an expired deadline: {result:?}"),
            request = read_transport_request(&mut server_stream) => request,
        };
        let Some(request) = request else {
            panic!(
                "web-share transport closed before a request: {:?}",
                stop.await
            );
        };
        assert!(matches!(request, Request::Handshake(_)));
        write_transport_response(
            &mut server_stream,
            Response::Handshake(HandshakeResponse {
                wire_version: RMUX_WIRE_VERSION,
                capabilities: capabilities_for_features(true)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            }),
        )
        .await;
        assert!(matches!(
            read_transport_request(&mut server_stream).await,
            Some(Request::WebShare(_))
        ));
        write_transport_response(
            &mut server_stream,
            Response::WebShare(Box::new(WebShareResponse::Stopped(
                WebShareStoppedResponse {
                    share_id: "share-1".to_owned(),
                    stopped: true,
                    output: CommandOutput::from_stdout(Vec::new()),
                },
            ))),
        )
        .await;
        stop.await
            .expect("web-share request task must not panic")
            .expect("returned web-share handle starts a fresh operation");
    }

    async fn read_transport_request(stream: &mut tokio::io::DuplexStream) -> Option<Request> {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 256];
        loop {
            if let Some(request) = decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return Some(request);
            }
            let read = stream.read(&mut buffer).await.expect("read request");
            if read == 0 {
                return None;
            }
            decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_transport_response(stream: &mut tokio::io::DuplexStream, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        stream.write_all(&frame).await.expect("write response");
        stream.flush().await.expect("flush response");
    }
}
