use rmux_proto::{BindKeyRequest, ListKeysRequest, Request, Response, UnbindKeyRequest};

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `bind-key` request over the detached RPC channel.
    pub fn bind_key(&mut self, request: BindKeyRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::BindKey(Box::new(request)))
    }

    /// Sends an `unbind-key` request over the detached RPC channel.
    pub fn unbind_key(&mut self, request: UnbindKeyRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::UnbindKey(request))
    }

    /// Sends a `list-keys` request over the detached RPC channel.
    pub fn list_keys(&mut self, request: ListKeysRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ListKeys(Box::new(request)))
    }
}
