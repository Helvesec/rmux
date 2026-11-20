//! Cached capability discovery for the blocking client.
use super::Connection;
use crate::ClientError;
use rmux_proto::capabilities::{capability_availability, CapabilityAvailability};
use rmux_proto::{HandshakeRequest, Request, Response, RmuxError};

impl Connection {
    /// Returns whether the connected daemon advertises a protocol capability.
    ///
    /// Optional client behavior uses this as a soft gate: older daemons that do
    /// not answer the handshake shape or report an error are treated as not
    /// supporting the capability, leaving the connection usable for legacy
    /// requests.
    pub fn supports_capability(&mut self, capability: &str) -> Result<bool, ClientError> {
        if let Some(capabilities) = &self.handshake_capabilities {
            return Ok(capability_availability(capability, capabilities)
                == CapabilityAvailability::Available);
        }

        match self.roundtrip(&Request::Handshake(HandshakeRequest::current()))? {
            Response::Handshake(response) => {
                self.handshake_capabilities = Some(response.capabilities);
                Ok(capability_availability(
                    capability,
                    self.handshake_capabilities
                        .as_ref()
                        .expect("handshake capabilities were just cached"),
                ) == CapabilityAvailability::Available)
            }
            Response::Error(error) => {
                if matches!(&error.error, RmuxError::UnsupportedWireVersion { .. }) {
                    return Err(ClientError::Protocol(error.error));
                }
                self.handshake_capabilities = Some(Vec::new());
                Ok(false)
            }
            _ => {
                self.handshake_capabilities = Some(Vec::new());
                Ok(false)
            }
        }
    }
}
