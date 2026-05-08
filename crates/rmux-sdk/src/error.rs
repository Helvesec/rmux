//! SDK facade errors.
//!
//! `RmuxError` is the SDK-facing error boundary for daemon-backed operations.
//! It intentionally does not implement [`Clone`], even when it wraps cloneable
//! lower-crate protocol diagnostics.

use std::error::Error;
use std::fmt;
use std::io;

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
        /// Lower-crate protocol diagnostic preserved as the source error.
        source: rmux_proto::RmuxError,
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

    /// Creates an SDK protocol error from a lower-crate protocol diagnostic.
    ///
    /// Negotiation and capability mismatches are normalized to
    /// [`RmuxError::Unsupported`] so callers can use [`RmuxError::feature`] and
    /// [`RmuxError::hint`] without parsing lower-crate display text.
    #[must_use]
    pub fn protocol(error: rmux_proto::RmuxError) -> Self {
        match error {
            rmux_proto::RmuxError::UnsupportedWireVersion {
                got,
                minimum,
                maximum,
            } => Self::unsupported(
                "protocol.wire_version",
                format!(
                    "upgrade the rmux daemon or use an SDK that supports wire version {got} \
                     (supported range {minimum}..={maximum})"
                ),
            ),
            rmux_proto::RmuxError::UnknownCommand(command) => {
                let feature = command_feature(&command);
                Self::unsupported(
                    feature,
                    format!(
                        "upgrade the rmux daemon or use a command advertised by the negotiated \
                         command inventory before sending `{command}`"
                    ),
                )
            }
            source => Self::Protocol { source },
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
            Self::Collect { .. } => None,
        }
    }

    /// Returns the stable feature identifier when the error variant carries
    /// one. The identifier is intended for log keys and capability matching,
    /// not user-facing copy.
    #[must_use]
    pub fn feature(&self) -> Option<&str> {
        match self {
            Self::Unsupported { feature, .. } => Some(feature),
            Self::Protocol { .. } | Self::Transport { .. } | Self::Collect { .. } => None,
        }
    }
}

impl From<rmux_proto::RmuxError> for RmuxError {
    fn from(error: rmux_proto::RmuxError) -> Self {
        Self::protocol(error)
    }
}

impl From<rmux_proto::ErrorResponse> for RmuxError {
    fn from(response: rmux_proto::ErrorResponse) -> Self {
        Self::protocol(response.error)
    }
}

impl From<io::Error> for RmuxError {
    fn from(error: io::Error) -> Self {
        Self::transport("communicate with rmux daemon", error)
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
        }
    }
}

/// SDK result alias parameterised over the SDK facade [`RmuxError`].
pub type Result<T> = core::result::Result<T, RmuxError>;

trait NonCloneGuard {}

impl<T: Clone> NonCloneGuard for T {}
impl NonCloneGuard for RmuxError {}
impl NonCloneGuard for CollectError {}

const _: fn() = sdk_errors_remain_non_clone;

fn sdk_errors_remain_non_clone() {
    fn assert_non_clone_guard<T: NonCloneGuard>() {}

    assert_non_clone_guard::<RmuxError>();
    assert_non_clone_guard::<CollectError>();
}

fn command_feature(command: &str) -> String {
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
