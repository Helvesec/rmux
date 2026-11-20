//! SDK facade errors.
//!
//! `RmuxError` is the SDK-facing error boundary for daemon-backed operations.
//! It intentionally does not implement [`Clone`], even when it wraps cloneable
//! lower-crate protocol diagnostics.

use std::error::Error;
use std::fmt;
use std::io;

use crate::broadcast::PartialBroadcastFailure;
use crate::diagnostics;
use crate::wait::WaitTimeoutError;
use crate::{PaneId, SessionName};

mod protocol;
pub use protocol::ProtocolError;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod transport_tests;

const PROTOCOL_HINT: &str =
    "check the request and daemon state, then retry after correcting the request";
const TRANSPORT_HINT: &str = "verify the rmux daemon is running and the endpoint is reachable";

/// SDK facade error type for daemon-backed operations.
///
/// The type is deliberately not `Clone`: error surfaces that need duplication
/// should wrap in `Arc` rather than fan out cheap copies of opaque diagnostics.
#[derive(Debug)]
#[non_exhaustive]
pub enum RmuxError {
    /// A capability or operation is not supported by the negotiated
    /// daemon. Carries a stable feature identifier and a visible recovery
    /// hint so the SDK can map lower-crate typed unsupported errors to a
    /// consistent surface.
    #[non_exhaustive]
    Unsupported {
        /// Stable, machine-readable identifier for the unsupported
        /// operation. Used by callers that pattern-match on capabilities.
        feature: String,
        /// Visible recovery hint shown after the human-readable message.
        hint: String,
    },
    /// A protocol-level daemon response or local protocol validation failure.
    #[non_exhaustive]
    Protocol {
        /// SDK-owned diagnostic preserved as the source error.
        source: ProtocolError,
    },
    /// A local transport failure while communicating with the daemon.
    #[non_exhaustive]
    Transport {
        /// Operation that was attempted when the transport failed.
        operation: String,
        /// Underlying I/O failure preserved as the source error.
        source: io::Error,
    },
    /// Multiple SDK diagnostics collected while evaluating one operation.
    #[non_exhaustive]
    Collect {
        /// Aggregated diagnostics preserved as the source error.
        source: CollectError,
    },
    /// A broadcast reached at least one pane and failed for at least one pane.
    #[non_exhaustive]
    PartialBroadcast {
        /// Per-pane successes and failures.
        source: PartialBroadcastFailure,
    },
    /// A visible wait timed out and retained the last observed snapshot.
    #[non_exhaustive]
    WaitTimeout {
        /// Timeout details and last visible snapshot.
        source: WaitTimeoutError,
    },
    /// A stable pane id no longer resolves in the addressed session.
    #[non_exhaustive]
    PaneNotFound {
        /// Session searched for the pane id.
        session_name: SessionName,
        /// Stable pane id requested by the caller.
        pane_id: PaneId,
    },
    /// The pane still has a running process and replacement was not requested.
    #[non_exhaustive]
    ProcessStillRunning {
        /// Server diagnostic describing the active process conflict.
        message: String,
    },
    /// A daemon-side process spawn failed.
    #[non_exhaustive]
    SpawnFailed {
        /// Server diagnostic describing the spawn failure.
        message: String,
    },
    /// A caller supplied an invalid regular expression.
    #[non_exhaustive]
    InvalidRegex {
        /// Pattern supplied by the caller.
        pattern: String,
        /// Regex compiler diagnostic.
        message: String,
    },
    /// A daemon-side owned-session lease was not found or no longer matches.
    #[non_exhaustive]
    OwnedSessionLeaseLost {
        /// Server diagnostic describing the lost lease.
        message: String,
    },
}

impl RmuxError {
    /// Creates an unsupported-feature error with a stable identifier and
    /// visible recovery hint.
    #[must_use]
    pub fn unsupported(feature: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::Unsupported {
            feature: feature.into(),
            hint: hint.into(),
        }
    }

    /// Creates an SDK protocol error from an SDK-owned diagnostic.
    ///
    /// Use [`ProtocolError::new`] for a caller-supplied diagnostic. Transport
    /// errors are converted privately by the SDK; the former public protocol
    /// error/response conversions have been removed before 1.0.
    #[must_use]
    pub fn protocol(source: ProtocolError) -> Self {
        Self::Protocol { source }
    }

    pub(crate) fn protocol_source(&self) -> Option<&rmux_proto::RmuxError> {
        match self {
            Self::Protocol { source } => Some(&source.inner),
            _ => None,
        }
    }

    pub(crate) fn from_proto(error: rmux_proto::RmuxError) -> Self {
        match error {
            rmux_proto::RmuxError::UnsupportedWireVersion {
                got,
                minimum,
                maximum,
            } => Self::unsupported(
                diagnostics::FEATURE_PROTOCOL_WIRE_VERSION,
                format!(
                    "upgrade the rmux daemon or use an SDK that supports wire version {got} \
                     (supported range {minimum}..={maximum})"
                ),
            ),
            rmux_proto::RmuxError::UnsupportedCapability { feature, supported } => {
                let hint = diagnostics::unsupported_capability_hint(&feature, &supported);
                Self::unsupported(feature, hint)
            }
            rmux_proto::RmuxError::UnknownCommand(command) => {
                let feature = diagnostics::command_feature_id(&command);
                Self::unsupported(
                    feature,
                    format!(
                        "upgrade the rmux daemon or use a command advertised by the negotiated \
                         command inventory before sending `{command}`"
                    ),
                )
            }
            source => map_known_protocol_error(source),
        }
    }

    /// Creates an SDK transport error for a daemon communication operation.
    #[must_use]
    pub fn transport(operation: impl Into<String>, source: io::Error) -> Self {
        Self::Transport {
            operation: operation.into(),
            source,
        }
    }

    /// Creates an SDK aggregate error from collected diagnostics.
    #[must_use]
    pub fn collect(source: CollectError) -> Self {
        Self::Collect { source }
    }

    /// Creates a partial-broadcast error with per-pane results.
    #[must_use]
    pub fn partial_broadcast(source: PartialBroadcastFailure) -> Self {
        Self::PartialBroadcast { source }
    }

    /// Creates a visible-wait timeout error.
    #[must_use]
    pub fn wait_timeout(source: WaitTimeoutError) -> Self {
        Self::WaitTimeout { source }
    }

    /// Creates a typed stable-pane-missing error.
    #[must_use]
    pub fn pane_not_found(session_name: SessionName, pane_id: PaneId) -> Self {
        Self::PaneNotFound {
            session_name,
            pane_id,
        }
    }

    /// Creates a typed regex validation error.
    #[must_use]
    pub fn invalid_regex(pattern: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidRegex {
            pattern: pattern.into(),
            message: message.into(),
        }
    }

    /// Returns the visible recovery hint associated with this error,
    /// if one is recorded for the variant.
    ///
    /// Aggregate errors return `None`; inspect the contained diagnostics with
    /// [`CollectError::errors`] to read each individual hint.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Unsupported { hint, .. } => Some(hint),
            Self::Protocol { .. } => Some(PROTOCOL_HINT),
            Self::Transport { .. } => Some(TRANSPORT_HINT),
            Self::PaneNotFound { .. }
            | Self::ProcessStillRunning { .. }
            | Self::SpawnFailed { .. }
            | Self::InvalidRegex { .. }
            | Self::OwnedSessionLeaseLost { .. }
            | Self::Collect { .. }
            | Self::PartialBroadcast { .. }
            | Self::WaitTimeout { .. } => None,
        }
    }

    /// Returns the stable feature identifier when the error variant carries
    /// one. The identifier is intended for log keys and capability matching,
    /// not user-facing copy.
    #[must_use]
    pub fn feature(&self) -> Option<&str> {
        match self {
            Self::Unsupported { feature, .. } => Some(feature),
            Self::Protocol { .. }
            | Self::Transport { .. }
            | Self::Collect { .. }
            | Self::PartialBroadcast { .. }
            | Self::WaitTimeout { .. }
            | Self::PaneNotFound { .. }
            | Self::ProcessStillRunning { .. }
            | Self::SpawnFailed { .. }
            | Self::InvalidRegex { .. }
            | Self::OwnedSessionLeaseLost { .. } => None,
        }
    }
}

fn map_known_protocol_error(source: rmux_proto::RmuxError) -> RmuxError {
    match source {
        rmux_proto::RmuxError::PaneNotFound {
            session_name,
            pane_id,
        } => RmuxError::PaneNotFound {
            session_name,
            pane_id,
        },
        rmux_proto::RmuxError::ProcessStillRunning => RmuxError::ProcessStillRunning {
            message: rmux_proto::RmuxError::ProcessStillRunning.to_string(),
        },
        rmux_proto::RmuxError::SpawnFailed { message } => RmuxError::SpawnFailed { message },
        rmux_proto::RmuxError::OwnedSessionLeaseLost { session_name } => {
            RmuxError::OwnedSessionLeaseLost {
                message: rmux_proto::RmuxError::OwnedSessionLeaseLost { session_name }.to_string(),
            }
        }
        inner => RmuxError::protocol(ProtocolError { inner }),
    }
}

impl From<io::Error> for RmuxError {
    fn from(error: io::Error) -> Self {
        Self::transport("communicate with rmux daemon", error)
    }
}

impl From<crate::SessionNameError> for RmuxError {
    fn from(_: crate::SessionNameError) -> Self {
        Self::from_proto(rmux_proto::RmuxError::EmptySessionName)
    }
}

impl From<CollectError> for RmuxError {
    fn from(error: CollectError) -> Self {
        Self::collect(error)
    }
}

/// Aggregated SDK diagnostics produced by collection-style operations.
///
/// The individual diagnostics remain available through [`CollectError::errors`]
/// and their display output is preserved when the aggregate is formatted,
/// including per-error `hint:` lines.
#[derive(Debug, Default)]
pub struct CollectError {
    errors: Vec<RmuxError>,
}

impl CollectError {
    /// Creates an aggregate from SDK diagnostics.
    #[must_use]
    pub fn new(errors: Vec<RmuxError>) -> Self {
        Self { errors }
    }

    /// Returns the collected diagnostics.
    #[must_use]
    pub fn errors(&self) -> &[RmuxError] {
        &self.errors
    }

    /// Returns the number of collected diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Returns `true` when no diagnostics were collected.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Appends one SDK diagnostic to the aggregate.
    pub fn push(&mut self, error: RmuxError) {
        self.errors.push(error);
    }

    /// Consumes the aggregate and returns the collected diagnostics.
    #[must_use]
    pub fn into_errors(self) -> Vec<RmuxError> {
        self.errors
    }
}

impl From<Vec<RmuxError>> for CollectError {
    fn from(errors: Vec<RmuxError>) -> Self {
        Self::new(errors)
    }
}

impl FromIterator<RmuxError> for CollectError {
    fn from_iter<T: IntoIterator<Item = RmuxError>>(iter: T) -> Self {
        Self::new(iter.into_iter().collect())
    }
}

impl Extend<RmuxError> for CollectError {
    fn extend<T: IntoIterator<Item = RmuxError>>(&mut self, iter: T) {
        self.errors.extend(iter);
    }
}

impl fmt::Display for CollectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errors.as_slice() {
            [] => write!(formatter, "no SDK diagnostics were collected"),
            [error] => {
                writeln!(formatter, "1 SDK diagnostic collected:")?;
                write_numbered_error(formatter, 1, error)
            }
            errors => {
                writeln!(formatter, "{} SDK diagnostics collected:", errors.len())?;
                for (index, error) in errors.iter().enumerate() {
                    if index > 0 {
                        writeln!(formatter)?;
                    }
                    write_numbered_error(formatter, index + 1, error)?;
                }
                Ok(())
            }
        }
    }
}

impl Error for CollectError {}

impl fmt::Display for RmuxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { feature, hint } => {
                write!(formatter, "unsupported feature `{feature}`\nhint: {hint}")
            }
            Self::Protocol { source } => {
                write!(
                    formatter,
                    "rmux protocol error: {source}\nhint: {PROTOCOL_HINT}"
                )
            }
            Self::Transport { operation, source } => {
                write!(
                    formatter,
                    "rmux transport error while {operation}: {source}\nhint: {TRANSPORT_HINT}"
                )
            }
            Self::Collect { source } => source.fmt(formatter),
            Self::PartialBroadcast { source } => source.fmt(formatter),
            Self::WaitTimeout { source } => source.fmt(formatter),
            Self::PaneNotFound {
                session_name,
                pane_id,
            } => write!(
                formatter,
                "pane id {} was not found in session {session_name}",
                pane_id
            ),
            Self::ProcessStillRunning { message } => {
                write!(formatter, "pane process still running: {message}")
            }
            Self::SpawnFailed { message } => write!(formatter, "pane spawn failed: {message}"),
            Self::InvalidRegex { pattern, message } => {
                write!(formatter, "invalid regex `{pattern}`: {message}")
            }
            Self::OwnedSessionLeaseLost { message } => {
                write!(formatter, "owned session lease lost: {message}")
            }
        }
    }
}

impl Error for RmuxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unsupported { .. } => None,
            Self::Protocol { source } => Some(source),
            Self::Transport { source, .. } => Some(source),
            Self::Collect { source } => Some(source),
            Self::PartialBroadcast { source } => Some(source),
            Self::WaitTimeout { source } => Some(source),
            Self::PaneNotFound { .. }
            | Self::ProcessStillRunning { .. }
            | Self::SpawnFailed { .. }
            | Self::InvalidRegex { .. }
            | Self::OwnedSessionLeaseLost { .. } => None,
        }
    }
}

/// SDK result alias parameterised over the SDK facade [`RmuxError`].
pub type Result<T> = core::result::Result<T, RmuxError>;

trait NonCloneGuard {}

impl<T: Clone> NonCloneGuard for T {}
impl NonCloneGuard for RmuxError {}
impl NonCloneGuard for CollectError {}
impl NonCloneGuard for ProtocolError {}

const _: fn() = sdk_errors_remain_non_clone;

#[allow(dead_code)]
fn sdk_errors_remain_non_clone() {
    fn assert_non_clone_guard<T: NonCloneGuard>() {}

    assert_non_clone_guard::<RmuxError>();
    assert_non_clone_guard::<CollectError>();
    assert_non_clone_guard::<ProtocolError>();
}

fn write_numbered_error(
    formatter: &mut fmt::Formatter<'_>,
    index: usize,
    error: &RmuxError,
) -> fmt::Result {
    let rendered = error.to_string();
    let mut lines = rendered.lines();

    let Some(first) = lines.next() else {
        return write!(formatter, "{index}. <empty SDK diagnostic>");
    };

    write!(formatter, "{index}. {first}")?;
    for line in lines {
        write!(formatter, "\n   {line}")?;
    }
    Ok(())
}
