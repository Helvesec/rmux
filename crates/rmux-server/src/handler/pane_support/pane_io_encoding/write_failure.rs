//! Typed classification of pane input write failures: dead pane, congestion,
//! rejected console injection, other.

use std::io;

use rmux_proto::{RmuxError, SessionName};

/// Canonical error for a pane input write whose target process is gone.
/// `PaneTerminalStore::clone_pane_master_if_alive` reports the same text.
const DEAD_PANE_INPUT_ERROR: &str = "target pane has exited";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneInputFailureKind {
    PaneGone,
    #[cfg(any(windows, test))]
    Congested,
    /// The pane is alive but its console refused one Windows console-input
    /// injection (attach, key record, mode read or console interrupt).
    #[cfg(any(windows, test))]
    ConsoleInjectionRejected,
    Other,
}

impl PaneInputFailureKind {
    /// `cause=` value of the exit-log line for an attached input dropped for
    /// this reason; `None` for a failure that closes the attach instead.
    fn attached_drop_cause(self) -> Option<&'static str> {
        match self {
            Self::PaneGone => Some("pane-gone"),
            #[cfg(any(windows, test))]
            Self::Congested => Some("congested"),
            #[cfg(any(windows, test))]
            Self::ConsoleInjectionRejected => Some("console-injection-rejected"),
            Self::Other => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct PaneInputWriteFailure {
    kind: PaneInputFailureKind,
    error: RmuxError,
}

impl PaneInputWriteFailure {
    pub(super) fn from_pty(
        error: io::Error,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Self {
        if is_dead_pane_write_error(&error) {
            return Self::pane_gone();
        }
        #[cfg(windows)]
        if error.kind() == io::ErrorKind::TimedOut {
            return Self::congested(RmuxError::Server(format!(
                "failed to write to pane {session_name}:{window_index}.{pane_index}: {error}"
            )));
        }
        Self::other(RmuxError::Server(format!(
            "failed to write to pane {session_name}:{window_index}.{pane_index}: {error}"
        )))
    }

    #[cfg(any(windows, test))]
    pub(super) fn from_console(
        error: &io::Error,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Self {
        if is_dead_pane_console_input_error(error) {
            return Self::pane_gone();
        }
        let congested = error.kind() == io::ErrorKind::TimedOut;
        let error = RmuxError::Server(format!(
            "failed to write console input to pane \
             {session_name}:{window_index}.{pane_index}: {error}"
        ));
        if congested {
            return Self::congested(error);
        }
        Self {
            kind: PaneInputFailureKind::ConsoleInjectionRejected,
            error,
        }
    }

    fn pane_gone() -> Self {
        Self {
            kind: PaneInputFailureKind::PaneGone,
            error: RmuxError::Server(DEAD_PANE_INPUT_ERROR.to_owned()),
        }
    }

    #[cfg(any(windows, test))]
    fn congested(error: RmuxError) -> Self {
        Self {
            kind: PaneInputFailureKind::Congested,
            error,
        }
    }

    pub(super) fn other(error: RmuxError) -> Self {
        Self {
            kind: PaneInputFailureKind::Other,
            error,
        }
    }

    pub(super) fn into_error(self) -> RmuxError {
        self.error
    }

    /// Attached input is best-effort once it leaves the state lock: a pane may
    /// exit or stop draining between target resolution and the write, and a
    /// live pane console may refuse one injection while its foreground program
    /// changes. Drop only that input in those typed cases; command paths still
    /// receive [`Self::into_error`], and every other failure still closes the
    /// attach.
    ///
    /// A rejected console injection must not close the attach: the attach
    /// forwarder answers an input error with the ordinary attach-stop
    /// sequence, which the client reads as a clean end and exits 0 with no
    /// message, so one refused Ctrl-C would silently end a healthy attach.
    ///
    /// Every dropped input leaves one `event=attached-input-dropped` line in
    /// `RMUX_ATTACH_EXIT_LOG`: daemons have no tracing subscriber, and a lost
    /// key is otherwise indistinguishable from a key the client never sent.
    pub(super) fn into_attached_result(self) -> Result<(), RmuxError> {
        match self.kind.attached_drop_cause() {
            Some(cause) => {
                crate::diagnostic_log::record_attached_input_dropped(cause, &self.error);
                Ok(())
            }
            None => Err(self.error),
        }
    }
}

pub(in crate::handler) fn is_dead_pane_write_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ) || is_unix_pty_eio(error)
}

#[cfg(unix)]
fn is_unix_pty_eio(error: &io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

#[cfg(not(unix))]
fn is_unix_pty_eio(_error: &io::Error) -> bool {
    false
}

/// Whether a Windows console injection failed because the pane's console is
/// gone.
///
/// `AttachConsole` reports `ERROR_INVALID_HANDLE` for a process that has no
/// console and `ERROR_INVALID_PARAMETER` for a process that no longer exists,
/// and a console torn down between the attach and the injection surfaces as
/// `ERROR_FILE_NOT_FOUND` when opening `CONIN$`. Those are the ConPTY
/// equivalents of the broken pipe the PTY byte path already classifies as a
/// dead pane, so they must reach callers as the same dead-pane error.
#[cfg(any(windows, test))]
fn is_dead_pane_console_input_error(error: &io::Error) -> bool {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_INVALID_HANDLE: i32 = 6;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    is_dead_pane_write_error(error)
        || matches!(
            error.raw_os_error(),
            Some(ERROR_FILE_NOT_FOUND | ERROR_INVALID_HANDLE | ERROR_INVALID_PARAMETER)
        )
}

#[cfg(any(windows, test))]
fn pane_console_input_failure(
    error: &io::Error,
    session_name: &SessionName,
    window_index: u32,
    pane_index: u32,
) -> PaneInputWriteFailure {
    PaneInputWriteFailure::from_console(error, session_name, window_index, pane_index)
}

#[cfg(test)]
mod dead_pane_input_tests {
    use super::*;

    /// `AttachConsole`/`CONIN$` codes for a pane process that is gone.
    const GONE_PANE_CONSOLE_ERRORS: [i32; 3] = [2, 6, 87];
    /// `ERROR_GEN_FAILURE`: transient console churn, not a dead pane.
    const EXHAUSTED_TRANSIENT_CONSOLE_ERROR: i32 = 31;
    /// Codes a live pane console can return for one injection:
    /// `ERROR_INVALID_FUNCTION`, `ERROR_ACCESS_DENIED` and an exhausted
    /// `ERROR_GEN_FAILURE`. Raw code 5 is `EIO`, a dead PTY, on Unix hosts,
    /// so `ERROR_ACCESS_DENIED` is only meaningful where it is that code.
    const REFUSED_LIVE_CONSOLE_ERRORS: &[i32] = &[
        1,
        #[cfg(windows)]
        5,
        EXHAUSTED_TRANSIENT_CONSOLE_ERROR,
    ];

    fn console_failure(raw_os_error: i32) -> PaneInputWriteFailure {
        pane_console_input_failure(
            &io::Error::from_raw_os_error(raw_os_error),
            &SessionName::new("console-input").expect("valid session"),
            0,
            0,
        )
    }

    #[test]
    fn attached_input_survives_a_console_write_to_a_pane_that_exited() {
        for raw_os_error in GONE_PANE_CONSOLE_ERRORS {
            let failure = console_failure(raw_os_error);
            assert!(
                failure.into_attached_result().is_ok(),
                "a console key written to a pane that exited must not end the \
                 attach connection (os error {raw_os_error})"
            );
        }
    }

    #[test]
    fn console_writes_report_the_same_dead_pane_error_as_pane_bytes() {
        for raw_os_error in GONE_PANE_CONSOLE_ERRORS {
            let failure = console_failure(raw_os_error);
            assert_eq!(
                failure.kind,
                PaneInputFailureKind::PaneGone,
                "os error {raw_os_error} must retain a typed dead-pane classification"
            );
            assert_eq!(
                failure.into_error(),
                RmuxError::Server(DEAD_PANE_INPUT_ERROR.to_owned()),
                "os error {raw_os_error} must reach send-keys as the canonical dead-pane error"
            );
        }
    }

    #[test]
    fn other_console_write_failures_stay_reportable() {
        let failure = console_failure(EXHAUSTED_TRANSIENT_CONSOLE_ERROR);

        assert!(
            failure
                .error
                .to_string()
                .contains("failed to write console input to pane console-input:0.0"),
            "unexpected error: {}",
            failure.error
        );
        assert_eq!(failure.kind, PaneInputFailureKind::ConsoleInjectionRejected);
        assert!(failure.into_error().to_string().contains("os error 31"));
    }

    #[test]
    fn attached_input_survives_a_console_injection_the_live_pane_refused() {
        for &raw_os_error in REFUSED_LIVE_CONSOLE_ERRORS {
            assert!(
                console_failure(raw_os_error).into_attached_result().is_ok(),
                "one console input a live pane refused must not end the attach \
                 connection (os error {raw_os_error}): the client would read the \
                 stop as a clean end and exit 0"
            );
            assert!(
                console_failure(raw_os_error)
                    .into_error()
                    .to_string()
                    .contains(&format!("os error {raw_os_error}")),
                "send-keys must still report os error {raw_os_error}"
            );
        }
    }

    #[test]
    fn attached_pane_byte_write_failures_still_close_the_attach() {
        let failure = PaneInputWriteFailure::from_pty(
            io::Error::new(io::ErrorKind::PermissionDenied, "pane write refused"),
            &SessionName::new("pty-input").expect("valid session"),
            0,
            0,
        );

        assert_eq!(failure.kind, PaneInputFailureKind::Other);
        assert!(failure.into_attached_result().is_err());
    }

    #[test]
    fn attached_input_drops_conpty_congestion_without_hiding_command_failure() {
        let session = SessionName::new("console-input").expect("valid session");
        let attached_failure = PaneInputWriteFailure::from_console(
            &io::Error::new(io::ErrorKind::TimedOut, "transient ConPTY congestion"),
            &session,
            0,
            0,
        );
        assert_eq!(
            attached_failure.kind,
            PaneInputFailureKind::Congested,
            "timeout must remain distinct from a dead pane"
        );
        assert!(
            attached_failure.into_attached_result().is_ok(),
            "a congested pane drops only the current attached input"
        );

        let command_failure = PaneInputWriteFailure::from_console(
            &io::Error::new(io::ErrorKind::TimedOut, "transient ConPTY congestion"),
            &session,
            0,
            0,
        );
        assert!(
            command_failure
                .into_error()
                .to_string()
                .contains("transient ConPTY congestion"),
            "command paths must still report the timeout"
        );
    }

    #[test]
    fn every_dropped_attached_input_leaves_one_exit_log_line() {
        let _lock = crate::test_env::lock_blocking();
        let log = std::env::temp_dir().join(format!(
            "rmux-attached-input-dropped-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let _log_env = crate::test_env::EnvVarGuard::set(
            "RMUX_ATTACH_EXIT_LOG",
            Some(log.to_str().expect("utf-8 temp path")),
        );
        let refused = SessionName::new("drop-log-refused").expect("valid session");
        let congested = SessionName::new("drop-log-congested").expect("valid session");
        let closing = SessionName::new("drop-log-closing").expect("valid session");

        let drops = [
            PaneInputWriteFailure::from_console(
                &io::Error::from_raw_os_error(EXHAUSTED_TRANSIENT_CONSOLE_ERROR),
                &refused,
                2,
                1,
            ),
            PaneInputWriteFailure::from_console(
                &io::Error::new(io::ErrorKind::TimedOut, "ConPTY input stalled"),
                &congested,
                0,
                0,
            ),
            console_failure(GONE_PANE_CONSOLE_ERRORS[0]),
        ];
        for failure in drops {
            assert!(failure.into_attached_result().is_ok());
        }
        let closing_failure = PaneInputWriteFailure::from_pty(
            io::Error::new(io::ErrorKind::PermissionDenied, "pane write refused"),
            &closing,
            0,
            0,
        );
        assert!(closing_failure.into_attached_result().is_err());

        let contents = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        let dropped: Vec<&str> = contents
            .lines()
            .filter(|line| line.contains(" event=attached-input-dropped "))
            .collect();
        let has_line = |cause: &str, details: &[&str]| {
            dropped.iter().any(|line| {
                line.contains(&format!(" cause={cause} "))
                    && details.iter().all(|detail| line.contains(detail))
            })
        };
        assert!(
            has_line(
                "console-injection-rejected",
                &["pane drop-log-refused:2.1", "os error 31"]
            ),
            "a refused console injection must name its pane and OS error:\n{contents}"
        );
        assert!(
            has_line("congested", &["pane drop-log-congested:0.0"]),
            "a congested drop must leave an exit-log line:\n{contents}"
        );
        assert!(
            has_line("pane-gone", &[DEAD_PANE_INPUT_ERROR]),
            "a drop to an exited pane must leave an exit-log line:\n{contents}"
        );
        assert!(
            !contents.contains("drop-log-closing"),
            "a failure that closes the attach is reported as attach-error, not as a drop:\n{contents}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn attached_pty_timeout_uses_the_same_typed_congestion_policy() {
        let session = SessionName::new("conpty-input").expect("valid session");
        let failure = PaneInputWriteFailure::from_pty(
            io::Error::new(io::ErrorKind::TimedOut, "ConPTY input stalled"),
            &session,
            0,
            0,
        );

        assert_eq!(failure.kind, PaneInputFailureKind::Congested);
        assert!(failure.into_attached_result().is_ok());
    }
}
