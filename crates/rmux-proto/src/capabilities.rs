//! Detached RPC capability handshake DTOs.

use serde::{Deserialize, Serialize};

use crate::{RmuxError, RMUX_WIRE_VERSION};

/// Stable feature id for the detached bincode RPC transport.
pub const CAPABILITY_DETACHED_RPC: &str = "rpc.detached";
/// Stable feature id for the capabilities handshake request.
pub const CAPABILITY_HANDSHAKE: &str = "protocol.capabilities";
/// Stable feature id for framed protocol errors returned as `Response::Error`.
pub const CAPABILITY_FRAMED_ERRORS: &str = "protocol.framed_errors";
/// Stable feature id for `attach-session` framed-to-raw stream upgrades.
pub const CAPABILITY_ATTACH_STREAM: &str = "stream.attach";
/// Stable feature id for attach-stream resize messages that carry pixel geometry.
pub const CAPABILITY_ATTACH_RESIZE_GEOMETRY: &str = "stream.attach.resize_geometry";
/// Stable feature id for coalescible attach-stream render messages.
pub const CAPABILITY_ATTACH_RENDER: &str = "stream.attach.render";
/// Stable feature id for attach-stream Windows console key messages.
pub const CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY: &str = "stream.attach.windows_console_key";
/// Stable feature id for control-mode framed-to-raw stream upgrades.
pub const CAPABILITY_CONTROL_STREAM: &str = "stream.control";
/// Stable feature id for daemon shutdown over detached RPC.
pub const CAPABILITY_DAEMON_SHUTDOWN: &str = "daemon.shutdown";
/// Stable feature id for daemon version and activity metadata.
pub const CAPABILITY_DAEMON_STATUS: &str = "daemon.status";
/// Stable feature id for safe idle-daemon shutdown during client upgrades.
pub const CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE: &str = "daemon.shutdown_if_idle";
/// Stable feature id for daemon-backed SDK waits and cancellation.
pub const CAPABILITY_SDK_WAITS: &str = "sdk.waits";
/// Stable feature id for two-phase SDK waits that acknowledge the armed state.
pub const CAPABILITY_SDK_WAITS_ARMED: &str = "sdk.waits.armed";
/// Stable feature id for SDK pane operations that target stable pane ids.
pub const CAPABILITY_SDK_PANE_BY_ID: &str = "sdk.pane.by_id";
/// Stable feature id for daemon-side SDK pane input broadcast.
pub const CAPABILITY_SDK_PANE_BROADCAST: &str = "sdk.pane.broadcast";
/// Stable feature id for daemon-side app-owned session leases.
pub const CAPABILITY_SDK_SESSION_LEASE: &str = "sdk.session.lease";
/// Stable feature id for explicit SDK process launch modes.
pub const CAPABILITY_SDK_PROCESS_COMMAND: &str = "sdk.process.command";
/// Stable feature id for target-client aware command request variants.
pub const CAPABILITY_TARGET_CLIENT_COMMANDS: &str = "commands.target_client";
/// Stable feature id for CLI pane commands with daemon-side target resolution.
pub const CAPABILITY_CLI_TARGET_ACTIONS: &str = "commands.cli_target_actions";
/// Stable feature id for `capture-pane` with daemon-side target resolution.
pub const CAPABILITY_CLI_CAPTURE_TARGET_ACTION: &str = "commands.cli_capture_target_action";
/// Stable feature id for browser-visible pane sharing.
///
/// This optional capability is advertised by daemons compiled with their web
/// listener enabled rather than by the protocol baseline capability list.
pub const CAPABILITY_WEB_SHARE: &str = "web.share";

/// Capabilities advertised by this protocol build.
pub const SUPPORTED_CAPABILITIES: &[&str] = &[
    CAPABILITY_DETACHED_RPC,
    CAPABILITY_HANDSHAKE,
    CAPABILITY_FRAMED_ERRORS,
    CAPABILITY_ATTACH_STREAM,
    CAPABILITY_ATTACH_RESIZE_GEOMETRY,
    CAPABILITY_ATTACH_RENDER,
    CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY,
    CAPABILITY_CONTROL_STREAM,
    CAPABILITY_DAEMON_SHUTDOWN,
    CAPABILITY_DAEMON_STATUS,
    CAPABILITY_DAEMON_SHUTDOWN_IF_IDLE,
    CAPABILITY_SDK_WAITS,
    CAPABILITY_SDK_WAITS_ARMED,
    CAPABILITY_SDK_PANE_BY_ID,
    CAPABILITY_SDK_PANE_BROADCAST,
    CAPABILITY_SDK_SESSION_LEASE,
    CAPABILITY_SDK_PROCESS_COMMAND,
    CAPABILITY_TARGET_CLIENT_COMMANDS,
    CAPABILITY_CLI_TARGET_ACTIONS,
    CAPABILITY_CLI_CAPTURE_TARGET_ACTION,
];

/// Client-to-server version and capability negotiation request.
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

        if let Some(feature) = self
            .required_capabilities
            .iter()
            .find(|feature| !supported_capabilities.contains(&feature.as_str()))
        {
            return Err(RmuxError::UnsupportedCapability {
                feature: feature.clone(),
                supported: supported_capabilities
                    .iter()
                    .copied()
                    .map(str::to_owned)
                    .collect(),
            });
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
        HandshakeResponse, CAPABILITY_ATTACH_RENDER, CAPABILITY_ATTACH_WINDOWS_CONSOLE_KEY,
        CAPABILITY_CLI_CAPTURE_TARGET_ACTION, CAPABILITY_CLI_TARGET_ACTIONS,
        CAPABILITY_SDK_WAITS_ARMED,
    };

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
    }

    #[test]
    fn current_handshake_advertises_cli_target_action_capabilities() {
        let response = HandshakeResponse::current();

        for expected in [
            CAPABILITY_CLI_TARGET_ACTIONS,
            CAPABILITY_CLI_CAPTURE_TARGET_ACTION,
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
}
