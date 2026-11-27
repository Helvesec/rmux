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
        if self.handshake_capabilities.is_none() {
            let response = self.roundtrip(&Request::Handshake(HandshakeRequest::current()))?;
            self.cache_handshake_response(response)?;
        }
        Ok(capability_availability(
            capability,
            self.handshake_capabilities.as_deref().unwrap_or_default(),
        ) == CapabilityAvailability::Available)
    }

    /// Returns `(supports_runtime_expansion, raw_alias_snapshot_response)`.
    ///
    /// Pipeline the two independent read-only requests on a fresh connection;
    /// cached connections need only the snapshot. Both replies are consumed
    /// before returning. No execution request is sent here. Discard the
    /// connection on a transport or incompatible-wire error.
    pub fn command_alias_preflight(&mut self) -> Result<(bool, Response), ClientError> {
        let needs_handshake = self.handshake_capabilities.is_none();
        if needs_handshake {
            self.write_request(&Request::Handshake(HandshakeRequest::current()))?;
        }
        self.write_request(&Request::ShowOptions(rmux_proto::ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::ServerGlobal,
            name: Some("command-alias".to_owned()),
            value_only: true,
            include_inherited: false,
            quiet: true,
            include_hooks: false,
        }))?;
        if needs_handshake {
            let response = self.read_response()?;
            self.cache_handshake_response(response)?;
        }
        let aliases = self.read_response()?;
        let expansion =
            self.supports_capability(rmux_proto::CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION)?;
        Ok((expansion, aliases))
    }

    fn cache_handshake_response(&mut self, response: Response) -> Result<(), ClientError> {
        match response {
            Response::Handshake(response) => {
                self.handshake_capabilities = Some(response.capabilities);
            }
            Response::Error(error) => {
                if matches!(&error.error, RmuxError::UnsupportedWireVersion { .. }) {
                    return Err(ClientError::Protocol(error.error));
                }
                self.handshake_capabilities = Some(Vec::new());
            }
            _ => {
                self.handshake_capabilities = Some(Vec::new());
            }
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use rmux_proto::{
        encode_frame, CommandOutput, ErrorResponse, FrameDecoder, HandshakeResponse,
        HasSessionRequest, HasSessionResponse, OptionScopeSelector, ShowOptionsResponse,
        CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION, RMUX_WIRE_VERSION,
    };
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    fn next(stream: &mut UnixStream, decoder: &mut FrameDecoder) -> Request {
        let mut bytes = [0; 8192];
        loop {
            if let Some(request) = decoder.next_frame().unwrap() {
                return request;
            }
            let n = stream.read(&mut bytes).unwrap();
            assert_ne!(n, 0);
            decoder.push_bytes(&bytes[..n]);
        }
    }

    fn assert_snapshot(request: Request) {
        let Request::ShowOptions(request) = request else {
            panic!("expected a read-only snapshot, got {request:?}")
        };
        assert_eq!(request.scope, OptionScopeSelector::ServerGlobal);
        assert_eq!(request.name.as_deref(), Some("command-alias"));
        assert!(request.value_only && request.quiet);
        assert!(!request.include_hooks && !request.include_inherited);
    }

    fn snapshot() -> Response {
        Response::ShowOptions(ShowOptionsResponse {
            scope: OptionScopeSelector::ServerGlobal,
            output: CommandOutput::from_stdout("probe=display-message -p raw\nvalue\n"),
        })
    }

    #[test]
    fn alias_preflight_pipelines_reads_and_preserves_cache_and_response_boundaries() {
        for (handshake, supported) in [
            (
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: vec![CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION.to_owned()],
                }),
                true,
            ),
            (
                Response::Handshake(HandshakeResponse {
                    wire_version: RMUX_WIRE_VERSION,
                    capabilities: vec![],
                }),
                false,
            ),
            (
                Response::Error(ErrorResponse {
                    error: RmuxError::UnknownCommand("handshake".to_owned()),
                }),
                false,
            ),
        ] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let server = std::thread::spawn(move || {
                let mut decoder = FrameDecoder::new();
                assert!(matches!(
                    next(&mut server, &mut decoder),
                    Request::Handshake(_)
                ));
                // Both requests must arrive before the first reply: this detects
                // the extra serialized round trip deterministically.
                assert_snapshot(next(&mut server, &mut decoder));
                let mut replies = encode_frame(&handshake).unwrap();
                replies.extend(encode_frame(&snapshot()).unwrap());
                server.write_all(&replies).unwrap();
                // A second snapshot uses the cached negotiation and consumes only
                // its own response; an ordinary RPC must still align afterwards.
                assert_snapshot(next(&mut server, &mut decoder));
                server
                    .write_all(&encode_frame(&snapshot()).unwrap())
                    .unwrap();
                assert!(matches!(
                    next(&mut server, &mut decoder),
                    Request::HasSession(_)
                ));
                server
                    .write_all(
                        &encode_frame(&Response::HasSession(HasSessionResponse { exists: true }))
                            .unwrap(),
                    )
                    .unwrap();
            });
            let mut connection = Connection::new(client).unwrap();
            assert_eq!(
                connection.command_alias_preflight().unwrap(),
                (supported, snapshot())
            );
            assert_eq!(
                connection
                    .supports_capability(CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION)
                    .unwrap(),
                supported
            );
            assert_eq!(
                connection.command_alias_preflight().unwrap(),
                (supported, snapshot())
            );
            assert_eq!(
                connection
                    .roundtrip(&Request::HasSession(HasSessionRequest {
                        target: rmux_proto::SessionName::from_canonical("probe").unwrap()
                    }))
                    .unwrap(),
                Response::HasSession(HasSessionResponse { exists: true })
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn alias_preflight_wire_rejection_and_missing_reply_are_terminal() {
        for response in [
            None,
            Some(Response::Error(ErrorResponse {
                error: RmuxError::UnsupportedWireVersion {
                    got: 1,
                    minimum: RMUX_WIRE_VERSION,
                    maximum: RMUX_WIRE_VERSION,
                },
            })),
            Some(Response::Handshake(HandshakeResponse {
                wire_version: RMUX_WIRE_VERSION,
                capabilities: vec![CAPABILITY_CLI_RUNTIME_COMMAND_EXPANSION.to_owned()],
            })),
        ] {
            let (client, mut server) = UnixStream::pair().unwrap();
            server
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let server = std::thread::spawn(move || {
                let mut decoder = FrameDecoder::new();
                assert!(matches!(
                    next(&mut server, &mut decoder),
                    Request::Handshake(_)
                ));
                assert_snapshot(next(&mut server, &mut decoder));
                if let Some(response) = response {
                    server.write_all(&encode_frame(&response).unwrap()).unwrap();
                }
                // Neither an execution offer nor a retry may be sent.
                assert!(decoder.next_frame::<Request>().unwrap().is_none());
            });
            let mut connection = Connection::new(client).unwrap();
            assert!(connection.command_alias_preflight().is_err());
            drop(connection);
            server.join().unwrap();
        }
    }
}
