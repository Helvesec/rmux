use std::fmt;

use super::WebShareAccess;

impl fmt::Debug for WebShareAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebShareAccess")
            .field(
                "allow_loopback_development_origins",
                &self.allow_loopback_development_origins,
            )
            .field("expected_origin", &self.expected_origin)
            .field("expires_at", &self.expires_at)
            .field("_connection_permit", &self._connection_permit)
            .field("_spectator_lease", &self._spectator_lease)
            .field("_operator_lease", &self._operator_lease)
            .field("lease_book", &self.lease_book)
            .field("max_operators", &self.max_operators)
            .field("max_spectators", &self.max_spectators)
            .field("operator", &self.operator)
            .field("spectator", &self.spectator)
            .field(
                "spectator_pairing_code",
                &self.spectator_pairing_code.as_ref().map(|_| "[REDACTED]"),
            )
            .field("revoke_rx", &self.revoke_rx)
            .field("role", &self.role)
            .field("share_id", &self.share_id)
            .field("target", &self.target)
            .field("controls", &self.controls)
            .field("terminal_palette", &self.terminal_palette)
            .field("show_viewers", &self.show_viewers)
            .finish()
    }
}
