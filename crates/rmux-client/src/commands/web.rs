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
        let is_create = matches!(request, WebShareRequest::Create(_));
        let request = Request::WebShare(Box::new(request));
        if is_create {
            // Tunnel providers can legitimately take longer than the ordinary
            // detached RPC timeout while waiting for their public endpoint.
            self.roundtrip_without_read_timeout(&request)
        } else {
            self.roundtrip(&request)
        }
    }
}
