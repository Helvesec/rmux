use crate::web::WebShareConnectionCounts;
use rmux_proto::WebTerminalPalette;
use serde::Serialize;

#[cfg(test)]
#[path = "../outbound_pin_debug_tests.rs"]
mod tests;

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ServerMessage<'a> {
    Ready {
        protocol_version: u16,
        capabilities: &'static [&'static str],
        pane_size: PaneSize,
        scope: &'a str,
        share_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_name: Option<&'a str>,
        role: &'a str,
        operator: bool,
        operator_access: bool,
        spectator_access: bool,
        controls: bool,
        show_viewers: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        spectator_pairing_code: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ttl_remaining_seconds: Option<u64>,
        #[serde(flatten)]
        connection_counts: WebShareConnectionCounts,
        #[serde(skip_serializing_if = "Option::is_none")]
        terminal_palette: Option<&'a WebTerminalPalette>,
    },
    ViewerCount {
        #[serde(flatten)]
        connection_counts: WebShareConnectionCounts,
    },
    ShareRevoked {
        reason: &'a str,
    },
}

impl std::fmt::Debug for ServerMessage<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready {
                protocol_version,
                capabilities,
                pane_size,
                scope,
                share_id,
                session_name,
                role,
                operator,
                operator_access,
                spectator_access,
                controls,
                show_viewers,
                spectator_pairing_code,
                ttl_remaining_seconds,
                connection_counts,
                terminal_palette,
            } => formatter
                .debug_struct("Ready")
                .field("protocol_version", protocol_version)
                .field("capabilities", capabilities)
                .field("pane_size", pane_size)
                .field("scope", scope)
                .field("share_id", share_id)
                .field("session_name", session_name)
                .field("role", role)
                .field("operator", operator)
                .field("operator_access", operator_access)
                .field("spectator_access", spectator_access)
                .field("controls", controls)
                .field("show_viewers", show_viewers)
                .field(
                    "spectator_pairing_code",
                    &spectator_pairing_code.map(|_| "[REDACTED]"),
                )
                .field("ttl_remaining_seconds", ttl_remaining_seconds)
                .field("connection_counts", connection_counts)
                .field("terminal_palette", terminal_palette)
                .finish(),
            Self::ViewerCount { connection_counts } => formatter
                .debug_struct("ViewerCount")
                .field("connection_counts", connection_counts)
                .finish(),
            Self::ShareRevoked { reason } => formatter
                .debug_struct("ShareRevoked")
                .field("reason", reason)
                .finish(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct PaneSize {
    pub(super) cols: u16,
    pub(super) rows: u16,
}
