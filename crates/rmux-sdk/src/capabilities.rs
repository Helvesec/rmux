//! SDK capability negotiation helpers.

use std::sync::Arc;

use crate::diagnostics::FEATURE_PROTOCOL_CAPABILITIES;
use crate::transport::TransportClient;
use crate::{Result, RmuxError};
use rmux_proto::{
    HandshakeRequest, ProcessCommand, Request, Response, CAPABILITY_HANDSHAKE,
    CAPABILITY_SDK_PROCESS_COMMAND, RMUX_WIRE_VERSION,
};

pub(crate) async fn require(client: &TransportClient, capabilities: &[&str]) -> Result<()> {
    require_with_handshake(client, &[], capabilities).await
}

/// Non-erroring probe: whether the daemon advertises every listed capability.
pub(crate) async fn supports(client: &TransportClient, capabilities: &[&str]) -> Result<bool> {
    let supported = negotiated_capabilities_requiring(client, &[]).await?;
    Ok(capabilities
        .iter()
        .all(|capability| supported.iter().any(|entry| entry == capability)))
}

pub(crate) async fn require_with_handshake(
    client: &TransportClient,
    required_capabilities: &[&str],
    capabilities: &[&str],
) -> Result<()> {
    let supported = negotiated_capabilities_requiring(client, required_capabilities).await?;
    for capability in capabilities {
        ensure_capability(&supported, capability)?;
    }
    Ok(())
}

pub(crate) async fn require_process_command_if_present(
    client: &TransportClient,
    process_command: Option<&ProcessCommand>,
) -> Result<()> {
    if process_command.is_some() {
        require(client, &[CAPABILITY_SDK_PROCESS_COMMAND]).await?;
    }
    Ok(())
}

pub(crate) async fn negotiated_capabilities(client: &TransportClient) -> Result<Arc<[String]>> {
    negotiated_capabilities_requiring(client, &[]).await
}

async fn negotiated_capabilities_requiring(
    client: &TransportClient,
    required_capabilities: &[&str],
) -> Result<Arc<[String]>> {
    if required_capabilities.is_empty() {
        if let Some(capabilities) = client.cached_capabilities().await {
            return Ok(capabilities);
        }
    }

    let required = std::iter::once(CAPABILITY_HANDSHAKE)
        .chain(required_capabilities.iter().copied())
        .collect::<Vec<_>>();
    let response = client
        .request(Request::Handshake(HandshakeRequest::requiring(required)))
        .await
        .map_err(normalize_handshake_error)?;

    match response {
        Response::Handshake(response) => {
            ensure_selected_wire_version(response.wire_version)?;
            ensure_capability(&response.capabilities, CAPABILITY_HANDSHAKE)?;
            Ok(client.cache_capabilities(response.capabilities).await)
        }
        Response::Error(error) => Err(error.into()),
        response => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "rmux daemon sent `{}` response for capability handshake",
            response.command_name()
        )))),
    }
}

pub(crate) fn is_unavailable(error: &RmuxError, capability: &str) -> bool {
    matches!(
        error,
        RmuxError::Unsupported { feature, .. }
            if feature == capability || feature == FEATURE_PROTOCOL_CAPABILITIES
    )
}

fn ensure_selected_wire_version(wire_version: u32) -> Result<()> {
    if wire_version == RMUX_WIRE_VERSION {
        return Ok(());
    }

    Err(RmuxError::protocol(
        rmux_proto::RmuxError::UnsupportedWireVersion {
            got: wire_version,
            minimum: RMUX_WIRE_VERSION,
            maximum: RMUX_WIRE_VERSION,
        },
    ))
}

fn ensure_capability(supported: &[String], capability: &str) -> Result<()> {
    if supported.iter().any(|supported| supported == capability) {
        return Ok(());
    }

    Err(RmuxError::protocol(
        rmux_proto::RmuxError::UnsupportedCapability {
            feature: capability.to_owned(),
            supported: supported.to_vec(),
        },
    ))
}

fn normalize_handshake_error(error: RmuxError) -> RmuxError {
    match error {
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::Decode(message),
        } => unsupported_handshake_error(&message),
        RmuxError::Unsupported { feature, .. }
            if feature == crate::diagnostics::command_feature_id("handshake") =>
        {
            unsupported_handshake_error("daemon did not recognize the handshake request")
        }
        error => error,
    }
}

fn unsupported_handshake_error(detail: &str) -> RmuxError {
    RmuxError::unsupported(
        FEATURE_PROTOCOL_CAPABILITIES,
        format!(
            "upgrade the rmux daemon to one that advertises `{CAPABILITY_HANDSHAKE}` before using SDK capability negotiation; {detail}"
        ),
    )
}
