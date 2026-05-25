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
        .request(Request::WebShare(WebShareRequest::List(
            ListWebSharesRequest,
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::List(response)) => {
            Ok(response.shares.into_iter().map(Into::into).collect())
        }
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share list", response)),
    }
}

async fn stop_web_share(transport: &TransportClient, id: &str) -> Result<bool> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::Stop(
            StopWebShareRequest {
                share_id: id.to_owned(),
            },
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::Stopped(response)) => Ok(response.stopped),
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share stop", response)),
    }
}

async fn stop_all_web_shares(transport: &TransportClient) -> Result<usize> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::StopAll(
            StopAllWebSharesRequest,
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::StoppedAll(response)) => {
            Ok(usize::try_from(response.stopped).unwrap_or(usize::MAX))
        }
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share stop-all", response)),
    }
}

async fn lookup_summary(transport: &TransportClient, id: &str) -> Result<WebShareSummary> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::Lookup(
            LookupWebShareRequest {
                share_id: id.to_owned(),
            },
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::Lookup(response)) => {
            response.share.map(Into::into).ok_or_else(|| {
                RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "web share not found".to_owned(),
                ))
            })
        }
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share lookup", response)),
    }
}

async fn web_config(transport: &TransportClient) -> Result<WebConfigInfo> {
    require_web_share(transport).await?;
    let response = transport
        .request(Request::WebShare(WebShareRequest::Config(
            WebShareConfigRequest,
        )))
        .await?;
    match response {
        Response::WebShare(WebShareResponse::Config(response)) => Ok(response.listener.into()),
        Response::Error(error) => Err(error.into()),
        response => Err(unexpected_response("web-share config", response)),
    }
}

async fn require_web_share(transport: &TransportClient) -> Result<()> {
    crate::capabilities::require(transport, &[CAPABILITY_WEB_SHARE]).await
}

fn token_from_url(url: &str) -> Option<&str> {
    url.split_once("t=")
        .map(|(_, token)| token.split('&').next().unwrap_or(token))
}

fn unexpected_response(operation: &str, response: Response) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "rmux daemon sent `{}` response for {operation}",
        response.command_name()
    )))
}
