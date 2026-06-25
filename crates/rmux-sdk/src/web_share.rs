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
    use super::token_from_url;

    #[test]
    fn token_from_url_reads_current_web_share_fragment_contract() {
        let url = "https://share.rmux.io/#e=ws://127.0.0.1:9777/share&t=abc123&theme=dark";
        assert_eq!(token_from_url(url), Some("abc123"));
    }
}
