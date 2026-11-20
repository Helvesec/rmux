//! Detached RPC capability handshake DTOs.

use serde::{Deserialize, Serialize};

use crate::{RmuxError, RMUX_WIRE_VERSION};

mod registry;
pub use registry::*;

/// Builds the capability inventory for a binary with the supplied optional
/// features enabled.
///
/// The protocol baseline stays feature-independent, while binaries compiled
/// with browser sharing must advertise that optional capability consistently
/// from both their local inventory and daemon handshake.
#[must_use]
pub fn capabilities_for_features(web_share: bool) -> Vec<&'static str> {
    CAPABILITY_REGISTRY
        .iter()
        .filter(|entry| entry.advertised(web_share))
        .map(|entry| entry.id)
        .collect()
}

/// Client-to-server version and capability negotiation request.
///
/// The detached frame envelope remains mandatory and exact-versioned. These
/// min/max fields are an advisory post-decode compatibility window for peers
/// that already share the current envelope version. Required capabilities are
/// mandatory: the daemon must reject the request if any listed capability is
/// absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeRequest {
    /// Lowest detached RPC wire version accepted by the caller.
    pub minimum_wire_version: u32,
    /// Highest detached RPC wire version accepted by the caller.
    pub maximum_wire_version: u32,
    /// Capability ids the caller requires before issuing follow-up requests.
    pub required_capabilities: Vec<String>,
}

impl HandshakeRequest {
    /// Builds a current-version handshake with no mandatory capabilities.
    #[must_use]
    pub fn current() -> Self {
        Self::requiring(std::iter::empty::<&str>())
    }

    /// Builds a current-version handshake with explicit mandatory capabilities.
    #[must_use]
    pub fn requiring<I, S>(required_capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            minimum_wire_version: RMUX_WIRE_VERSION,
            maximum_wire_version: RMUX_WIRE_VERSION,
            required_capabilities: required_capabilities
                .into_iter()
                .map(|capability| capability.as_ref().to_owned())
                .collect(),
        }
    }

    /// Whether this request explicitly opts into a registered connection feature.
    /// This classifies the requested mode only; it does not validate or admit the request.
    #[must_use]
    pub fn enables_connection_capability(&self, id: &str) -> bool {
        capability_descriptor(id).is_some_and(|entry| {
            entry.negotiation == CapabilityNegotiation::RequiredOnConnection
                && self
                    .required_capabilities
                    .iter()
                    .any(|required| required == id)
        })
    }

    /// Validates this request against the supplied capability list.
    pub fn validate_against(&self, supported_capabilities: &[&str]) -> Result<(), RmuxError> {
        if self.minimum_wire_version > RMUX_WIRE_VERSION
            || self.maximum_wire_version < RMUX_WIRE_VERSION
        {
            return Err(RmuxError::UnsupportedWireVersion {
                got: RMUX_WIRE_VERSION,
                minimum: self.minimum_wire_version,
                maximum: self.maximum_wire_version,
            });
        }

        for feature in &self.required_capabilities {
            resolve_capability(
                feature,
                CapabilityRequirement::Required,
                supported_capabilities,
            )?;
        }

        Ok(())
    }
}

/// Server-to-client version and capability negotiation response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandshakeResponse {
    /// Detached RPC wire version selected for this connection.
    pub wire_version: u32,
    /// Capability ids supported by the daemon.
    pub capabilities: Vec<String>,
}

impl HandshakeResponse {
    /// Builds the response advertised by this protocol build.
    #[must_use]
    pub fn current() -> Self {
        Self {
            wire_version: RMUX_WIRE_VERSION,
            capabilities: SUPPORTED_CAPABILITIES
                .iter()
                .copied()
                .map(str::to_owned)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        capabilities_for_features, HandshakeRequest, HandshakeResponse, CAPABILITY_ATTACH_RENDER,
        CAPABILITY_ATTACH_SERVER_EXITED, CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY,
        CAPABILITY_CLI_CAPTURE_TARGET_ACTION, CAPABILITY_CLI_LIST_WINDOWS_ALL_QUEUE,
        CAPABILITY_CLI_TARGET_ACTIONS, CAPABILITY_HANDSHAKE,
        CAPABILITY_RESIZE_PANE_FLOATING_ABSOLUTE_V1, CAPABILITY_RESIZE_PANE_POSITION_V1,
        CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY, CAPABILITY_SDK_PANE_RAW_RECOVERY,
        CAPABILITY_SDK_PANE_SPLIT_IDENTITY, CAPABILITY_SDK_PANE_SURFACE_STREAM,
        CAPABILITY_SDK_SESSION_LEASE_BY_ID, CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
        CAPABILITY_SDK_WAITS_ARMED, CAPABILITY_WEB_SHARE,
    };
    use crate::{RmuxError, RMUX_WIRE_VERSION};

    #[test]
    fn current_handshake_advertises_attach_stream_capabilities() {
        let response = HandshakeResponse::current();

        assert!(response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACH_RENDER));
        assert!(response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY));
        assert!(response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACH_SERVER_EXITED));
    }

    #[test]
    fn current_handshake_advertises_cli_target_action_capabilities() {
        let response = HandshakeResponse::current();

        for expected in [
            CAPABILITY_CLI_TARGET_ACTIONS,
            CAPABILITY_CLI_CAPTURE_TARGET_ACTION,
            CAPABILITY_CLI_LIST_WINDOWS_ALL_QUEUE,
            CAPABILITY_SDK_WAITS_ARMED,
        ] {
            assert!(
                response
                    .capabilities
                    .iter()
                    .any(|capability| capability == expected),
                "missing capability {expected}"
            );
        }
    }

    #[test]
    fn current_handshake_versions_session_lease_identity_addressing() {
        let response = HandshakeResponse::current();

        assert!(!response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_SDK_SESSION_LEASE_BY_ID));
        assert!(response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2));
    }

    #[test]
    fn current_handshake_advertises_owned_session_stable_identity() {
        let response = HandshakeResponse::current();

        assert!(response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY));
    }

    #[test]
    fn current_handshake_advertises_atomic_sdk_split_identity() {
        let response = HandshakeResponse::current();

        assert!(response
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_SDK_PANE_SPLIT_IDENTITY));
    }

    #[test]
    fn current_handshake_advertises_floating_absolute_resize() {
        assert!(HandshakeResponse::current()
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_RESIZE_PANE_FLOATING_ABSOLUTE_V1));
    }

    #[test]
    fn current_handshake_advertises_floating_position_translation() {
        assert!(HandshakeResponse::current()
            .capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_RESIZE_PANE_POSITION_V1));
    }

    #[test]
    fn current_handshake_advertises_both_pane_stream_projections() {
        let response = HandshakeResponse::current();

        for expected in [
            CAPABILITY_SDK_PANE_RAW_RECOVERY,
            CAPABILITY_SDK_PANE_SURFACE_STREAM,
        ] {
            assert!(
                response
                    .capabilities
                    .iter()
                    .any(|capability| capability == expected),
                "missing capability {expected}"
            );
        }
    }

    #[test]
    fn optional_web_capability_follows_the_compiled_feature() {
        assert!(!capabilities_for_features(false).contains(&CAPABILITY_WEB_SHARE));
        assert!(capabilities_for_features(true).contains(&CAPABILITY_WEB_SHARE));
    }

    #[test]
    fn current_handshake_uses_exact_wire_window() {
        let request = HandshakeRequest::current();

        assert_eq!(request.minimum_wire_version, RMUX_WIRE_VERSION);
        assert_eq!(request.maximum_wire_version, RMUX_WIRE_VERSION);
    }

    #[test]
    fn handshake_wire_window_is_advisory_after_envelope_decode() {
        let request = HandshakeRequest {
            minimum_wire_version: RMUX_WIRE_VERSION.saturating_sub(1),
            maximum_wire_version: RMUX_WIRE_VERSION + 1,
            required_capabilities: vec![CAPABILITY_HANDSHAKE.to_owned()],
        };

        request
            .validate_against(&[CAPABILITY_HANDSHAKE])
            .expect("post-decode compatible wire window should validate");

        let future_only = HandshakeRequest {
            minimum_wire_version: RMUX_WIRE_VERSION + 1,
            maximum_wire_version: RMUX_WIRE_VERSION + 1,
            required_capabilities: Vec::new(),
        };
        assert!(matches!(
            future_only.validate_against(&[CAPABILITY_HANDSHAKE]),
            Err(RmuxError::UnsupportedWireVersion { .. })
        ));
    }

    #[test]
    fn required_handshake_capabilities_are_mandatory() {
        let request = HandshakeRequest::requiring(["missing.capability"]);

        assert!(matches!(
            request.validate_against(&[CAPABILITY_HANDSHAKE]),
            Err(RmuxError::UnsupportedCapability { feature, .. })
                if feature == "missing.capability"
        ));
    }
}
