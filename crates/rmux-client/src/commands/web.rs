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
        self.roundtrip(&Request::WebShare(request))
    }
}
