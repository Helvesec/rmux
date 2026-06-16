//! Nested-session detection via inherited multiplexer environment.

use std::error::Error as StdError;
use std::fmt;

/// The client context inferred from inherited multiplexer environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientContext {
    /// No multiplexer variable is set - the client is outside any multiplexer.
    Outside,
    /// A multiplexer variable is set - the client is inside a session.
    Nested,
}

/// The inherited multiplexer environment that made the client nested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientContextParent {
    /// No multiplexer environment was inherited.
    None,
    /// The client is inside an RMUX session.
    Rmux,
    /// The client is inside another tmux-compatible multiplexer session.
    Tmux,
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

/// Detects the client context by inspecting inherited multiplexer environment.
///
/// A non-empty value indicates a nested context. Absent or empty values
/// indicate the client is outside any multiplexer session.
#[must_use]
pub fn detect_context() -> ClientContext {
    detect_context_from_env(
        std::env::var_os("RMUX").as_deref(),
        std::env::var_os("TMUX").as_deref(),
    )
}

/// Detects which inherited multiplexer environment, if any, is present.
#[must_use]
pub fn detect_parent() -> ClientContextParent {
    detect_parent_from_env(
        std::env::var_os("RMUX").as_deref(),
        std::env::var_os("TMUX").as_deref(),
    )
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
fn detect_context_from_env(
    rmux_value: Option<&std::ffi::OsStr>,
    tmux_value: Option<&std::ffi::OsStr>,
) -> ClientContext {
    detect_parent_from_env(rmux_value, tmux_value).context()
}

fn detect_parent_from_env(
    rmux_value: Option<&std::ffi::OsStr>,
    tmux_value: Option<&std::ffi::OsStr>,
) -> ClientContextParent {
    if rmux_value.is_some_and(|value| !value.is_empty()) {
        return ClientContextParent::Rmux;
    }
    if tmux_value.is_some_and(|value| !value.is_empty()) {
        return ClientContextParent::Tmux;
    }
    ClientContextParent::None
}

impl ClientContextParent {
    fn context(self) -> ClientContext {
        match self {
            Self::None => ClientContext::Outside,
            Self::Rmux | Self::Tmux => ClientContext::Nested,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect_context_from_env, detect_parent_from_env, ensure_nested_context,
        require_nested_context, ClientContext, ClientContextParent, NestedContextError,
    };
    use std::ffi::OsStr;
    use std::sync::Mutex;

    static RMUX_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn absent_rmux_is_outside() {
        assert_eq!(detect_context_from_env(None, None), ClientContext::Outside);
        assert_eq!(
            detect_parent_from_env(None, None),
            ClientContextParent::None
        );
    }

    #[test]
    fn empty_rmux_is_outside() {
        assert_eq!(
            detect_context_from_env(Some(OsStr::new("")), Some(OsStr::new(""))),
            ClientContext::Outside
        );
        assert_eq!(
            detect_parent_from_env(Some(OsStr::new("")), Some(OsStr::new(""))),
            ClientContextParent::None
        );
    }

    #[test]
    fn nonempty_rmux_is_nested() {
        assert_eq!(
            detect_context_from_env(Some(OsStr::new("/tmp/rmux-1000/default,12345,0")), None),
            ClientContext::Nested
        );
        assert_eq!(
            detect_parent_from_env(Some(OsStr::new("/tmp/rmux-1000/default,12345,0")), None),
            ClientContextParent::Rmux
        );
    }

    #[test]
    fn nonempty_tmux_is_nested() {
        assert_eq!(
            detect_context_from_env(None, Some(OsStr::new("/tmp/rmux-1000/default,12345,0"))),
            ClientContext::Nested
        );
        assert_eq!(
            detect_parent_from_env(None, Some(OsStr::new("/tmp/rmux-1000/default,12345,0"))),
            ClientContextParent::Tmux
        );
    }

    #[test]
    fn any_nonempty_value_is_nested() {
        assert_eq!(
            detect_context_from_env(Some(OsStr::new("x")), None),
            ClientContext::Nested
        );
    }

    #[test]
    fn rmux_parent_takes_precedence_over_tmux_parent() {
        assert_eq!(
            detect_parent_from_env(Some(OsStr::new("rmux")), Some(OsStr::new("tmux"))),
            ClientContextParent::Rmux
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
        let original_tmux = std::env::var_os("TMUX");

        std::env::remove_var("RMUX");
        std::env::remove_var("TMUX");
        assert_eq!(super::detect_context(), ClientContext::Outside);
        assert_eq!(require_nested_context(), Err(NestedContextError));

        std::env::set_var("RMUX", "/tmp/rmux-1000/default,1,0");
        assert_eq!(super::detect_context(), ClientContext::Nested);
        assert_eq!(require_nested_context(), Ok(()));

        std::env::remove_var("RMUX");
        std::env::set_var("TMUX", "/tmp/rmux-1000/default,1,0");
        assert_eq!(super::detect_context(), ClientContext::Nested);
        assert_eq!(require_nested_context(), Ok(()));

        match original {
            Some(value) => std::env::set_var("RMUX", value),
            None => std::env::remove_var("RMUX"),
        }
        match original_tmux {
            Some(value) => std::env::set_var("TMUX", value),
            None => std::env::remove_var("TMUX"),
        }
    }
}
