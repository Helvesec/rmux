//! Nested-session detection via `$RMUX`.

use std::error::Error as StdError;
use std::fmt;

/// The client context inferred from the `$RMUX` environment variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientContext {
    /// No `$RMUX` variable is set - the client is outside any multiplexer.
    Outside,
    /// `$RMUX` is set - the client is inside an existing multiplexer session.
    Nested,
}

impl ClientContext {
    /// Returns `true` when the client is inside a nested multiplexer session.
    #[must_use]
    pub const fn is_nested(self) -> bool {
        matches!(self, Self::Nested)
    }
}

/// Error returned when a command requires a nested client context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestedContextError;

impl fmt::Display for NestedContextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("switch-client requires a nested client context")
    }
}

impl StdError for NestedContextError {}

/// Detects the client context by inspecting the `$RMUX` environment variable.
///
/// A non-empty `$RMUX` value indicates a nested context. An absent or empty
/// value indicates the client is outside any multiplexer session.
#[must_use]
pub fn detect_context() -> ClientContext {
    detect_context_from_env(std::env::var_os("RMUX").as_deref())
}

/// Returns an error when the supplied context is not nested.
pub fn ensure_nested_context(context: ClientContext) -> Result<(), NestedContextError> {
    if context.is_nested() {
        Ok(())
    } else {
        Err(NestedContextError)
    }
}

/// Detects the current client context and validates that it is nested.
pub fn require_nested_context() -> Result<(), NestedContextError> {
    ensure_nested_context(detect_context())
}

/// Pure detection logic that does not access the environment directly.
///
/// Exposed for deterministic unit testing.
fn detect_context_from_env(tmux_value: Option<&std::ffi::OsStr>) -> ClientContext {
    match tmux_value {
        Some(value) if !value.is_empty() => ClientContext::Nested,
        _ => ClientContext::Outside,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect_context_from_env, ensure_nested_context, require_nested_context, ClientContext,
        NestedContextError,
    };
    use std::ffi::OsStr;
    use std::sync::Mutex;

    static RMUX_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn absent_rmux_is_outside() {
        assert_eq!(detect_context_from_env(None), ClientContext::Outside);
    }

    #[test]
    fn empty_rmux_is_outside() {
        assert_eq!(
            detect_context_from_env(Some(OsStr::new(""))),
            ClientContext::Outside
        );
    }

    #[test]
    fn nonempty_rmux_is_nested() {
        assert_eq!(
            detect_context_from_env(Some(OsStr::new("/tmp/rmux-1000/default,12345,0"))),
            ClientContext::Nested
        );
    }

    #[test]
    fn any_nonempty_value_is_nested() {
        assert_eq!(
            detect_context_from_env(Some(OsStr::new("x"))),
            ClientContext::Nested
        );
    }

    #[test]
    fn is_nested_accessor() {
        assert!(ClientContext::Nested.is_nested());
        assert!(!ClientContext::Outside.is_nested());
    }

    #[test]
    fn ensure_nested_context_rejects_outside_contexts() {
        assert_eq!(
            ensure_nested_context(ClientContext::Outside),
            Err(NestedContextError)
        );
        assert_eq!(ensure_nested_context(ClientContext::Nested), Ok(()));
    }

    #[test]
    fn require_nested_context_reads_env() {
        let _guard = RMUX_ENV_LOCK.lock().expect("rmux env lock");
        let original = std::env::var_os("RMUX");

        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("RMUX") };
        assert_eq!(super::detect_context(), ClientContext::Outside);
        assert_eq!(require_nested_context(), Err(NestedContextError));

        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("RMUX", "/tmp/rmux-1000/default,1,0") };
        assert_eq!(super::detect_context(), ClientContext::Nested);
        assert_eq!(require_nested_context(), Ok(()));

        match original {
            // TODO: Audit that the environment access only happens in single-threaded code.
            Some(value) => unsafe { std::env::set_var("RMUX", value) },
            // TODO: Audit that the environment access only happens in single-threaded code.
            None => unsafe { std::env::remove_var("RMUX") },
        }
    }
}
