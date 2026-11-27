//! SDK diagnostics and stable unsupported-feature identifiers.
//!
//! Public helpers consume SDK errors and require no transport types.

use crate::RmuxError;

/// Stable SDK feature id for unsupported detached wire versions.
pub const FEATURE_PROTOCOL_WIRE_VERSION: &str = "protocol.wire_version";
/// Stable SDK feature id for daemon capability negotiation support.
pub const FEATURE_PROTOCOL_CAPABILITIES: &str = rmux_proto::CAPABILITY_HANDSHAKE;
/// Stable SDK feature id for Unix-domain socket transport support.
pub const FEATURE_TRANSPORT_UNIX_SOCKET: &str = "transport.unix_socket";
/// Stable SDK feature id for Windows named-pipe transport support.
pub const FEATURE_TRANSPORT_WINDOWS_PIPE: &str = "transport.windows_pipe";
/// Stable SDK feature id for explicit daemon shutdown.
pub const FEATURE_DAEMON_SHUTDOWN: &str = rmux_proto::CAPABILITY_DAEMON_SHUTDOWN;

/// Severity of an SDK diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticSeverity {
    /// A requested feature is unavailable with the connected daemon.
    Unsupported,
    /// A protocol or transport error prevented the operation from completing.
    Error,
}

/// Structured SDK diagnostic suitable for logs and UI surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    severity: DiagnosticSeverity,
    feature: Option<String>,
    message: String,
    hint: Option<String>,
}

impl Diagnostic {
    /// Creates a diagnostic for an unsupported feature id.
    #[must_use]
    pub fn unsupported(feature: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Unsupported,
            feature: Some(feature.into()),
            message: "unsupported rmux feature".to_owned(),
            hint: Some(hint.into()),
        }
    }

    /// Creates an error diagnostic without an unsupported-feature id.
    #[must_use]
    pub fn error(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            feature: None,
            message: message.into(),
            hint: Some(hint.into()),
        }
    }

    /// Returns the diagnostic severity.
    #[must_use]
    pub const fn severity(&self) -> DiagnosticSeverity {
        self.severity
    }

    /// Returns the stable unsupported-feature id, when available.
    #[must_use]
    pub fn feature(&self) -> Option<&str> {
        self.feature.as_deref()
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the recovery hint, when available.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

/// Returns the stable unsupported-feature id for an SDK error.
#[must_use]
pub fn unsupported_feature_id(error: &RmuxError) -> Option<String> {
    error.feature().map(str::to_owned)
}

/// Converts an SDK error into a structured diagnostic.
///
/// Protocol diagnostics retain their message and recovery hint separately.
/// Typed SDK outcomes retain their SDK display and optional hint. Aggregate
/// diagnostics retain ordering; inspect their source for individual errors.
/// Before 1.0 this helper accepted a transport error instead of an SDK error.
#[must_use]
pub fn protocol_diagnostic(error: &RmuxError) -> Diagnostic {
    match error {
        RmuxError::Unsupported { feature, hint } => Diagnostic::unsupported(feature, hint),
        RmuxError::Protocol { source } => Diagnostic::error(
            format!("rmux protocol error: {source}"),
            error.hint().expect("protocol errors have a recovery hint"),
        ),
        RmuxError::Transport { operation, source } => Diagnostic::error(
            format!("rmux transport error while {operation}: {source}"),
            error.hint().expect("transport errors have a recovery hint"),
        ),
        error => Diagnostic {
            severity: DiagnosticSeverity::Error,
            feature: None,
            message: error.to_string(),
            hint: error.hint().map(str::to_owned),
        },
    }
}

/// Builds the stable SDK feature id for an unsupported command name.
#[must_use]
pub fn command_feature_id(command: &str) -> String {
    let token = command
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => character,
            _ => '_',
        })
        .collect::<String>();

    if token.is_empty() {
        "command.<empty>".to_owned()
    } else {
        format!("command.{token}")
    }
}

/// Builds a visible recovery hint for an unsupported capability.
#[must_use]
pub fn unsupported_capability_hint(feature: &str, supported: &[String]) -> String {
    if supported.is_empty() {
        return format!("connect to an rmux daemon that advertises capability `{feature}`");
    }

    format!(
        "connect to an rmux daemon that advertises capability `{feature}`; supported capabilities: {}",
        supported.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_unsupported_errors_map_to_stable_feature_ids() {
        assert_eq!(
            unsupported_feature_id(&RmuxError::from_proto(
                rmux_proto::RmuxError::UnsupportedWireVersion {
                    got: 2,
                    minimum: 1,
                    maximum: 1,
                }
            ))
            .as_deref(),
            Some(FEATURE_PROTOCOL_WIRE_VERSION)
        );
        assert_eq!(
            unsupported_feature_id(&RmuxError::from_proto(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: "capability.future".to_owned(),
                    supported: vec![rmux_proto::CAPABILITY_DETACHED_RPC.to_owned()],
                }
            ))
            .as_deref(),
            Some("capability.future")
        );
        assert_eq!(
            unsupported_feature_id(&RmuxError::from_proto(
                rmux_proto::RmuxError::UnknownCommand("display-menu!".to_owned())
            ))
            .as_deref(),
            Some("command.display-menu_")
        );
    }

    #[test]
    fn protocol_diagnostic_preserves_unsupported_capability_hint() {
        let diagnostic = protocol_diagnostic(&RmuxError::from_proto(
            rmux_proto::RmuxError::UnsupportedCapability {
                feature: "capability.future".to_owned(),
                supported: vec![rmux_proto::CAPABILITY_HANDSHAKE.to_owned()],
            },
        ));

        assert_eq!(diagnostic.severity(), DiagnosticSeverity::Unsupported);
        assert_eq!(diagnostic.feature(), Some("capability.future"));
        assert!(diagnostic
            .hint()
            .expect("hint")
            .contains(rmux_proto::CAPABILITY_HANDSHAKE));
    }
}
