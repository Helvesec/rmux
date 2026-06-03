use rmux_proto::{Request, Response, RmuxError, WebShareRequest, CAPABILITY_WEB_SHARE};

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `web-share` request over the detached RPC channel.
    pub fn web_share(&mut self, request: WebShareRequest) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_WEB_SHARE)? {
            return Err(ClientError::Protocol(RmuxError::UnsupportedCapability {
                feature: CAPABILITY_WEB_SHARE.to_owned(),
                supported: Vec::new(),
            }));
        }
        let request = Request::WebShare(request);
        if matches!(request, Request::WebShare(WebShareRequest::Create(_))) {
            // Tunnel providers can legitimately take longer than the ordinary
            // detached RPC timeout while waiting for their public endpoint.
            self.roundtrip_without_read_timeout(&request)
        } else {
            self.roundtrip(&request)
        }
    }
}
