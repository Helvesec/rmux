use rmux_proto::{Request, Response, WebShareRequest};

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `web-share` request over the detached RPC channel.
    pub fn web_share(&mut self, request: WebShareRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::WebShare(request))
    }
}
