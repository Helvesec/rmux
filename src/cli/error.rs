use std::path::Path;

use rmux_client::{default_socket_path, AutoStartError, ClientError, NestedContextError};
use rmux_proto::{DisplayMessageDurationParseError, RmuxError};

use crate::tmux_error_surface::tmux_client_connect_error_message;

#[derive(Debug)]
pub(crate) struct ExitFailure {
    exit_code: i32,
    message: String,
    use_stderr: bool,
    message_termination: ExitMessageTermination,
    kind: ExitFailureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitMessageTermination {
    Line,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitFailureKind {
    Generic,
    AmbiguousTarget,
    ServerAbsent,
    UnsupportedWireVersion { got: u32 },
    Timeout,
}

impl ExitFailure {
    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn use_stderr(&self) -> bool {
        self.use_stderr
    }

    pub(crate) fn message_termination(&self) -> ExitMessageTermination {
        self.message_termination
    }

    pub(super) fn is_ambiguous_target(&self) -> bool {
        self.kind == ExitFailureKind::AmbiguousTarget
    }

    pub(super) fn is_server_absent(&self) -> bool {
        self.kind == ExitFailureKind::ServerAbsent
    }

    pub(super) fn is_unsupported_wire_version(&self) -> bool {
        matches!(self.kind, ExitFailureKind::UnsupportedWireVersion { .. })
    }

    pub(super) fn is_timeout(&self) -> bool {
        self.kind == ExitFailureKind::Timeout
    }

    pub(crate) fn new(exit_code: i32, message: impl Into<String>) -> Self {
        Self::new_with_kind(exit_code, message, ExitFailureKind::Generic)
    }

    pub(super) fn ambiguous_target(message: impl Into<String>) -> Self {
        Self::new_with_kind(1, message, ExitFailureKind::AmbiguousTarget)
    }

    fn new_with_kind(exit_code: i32, message: impl Into<String>, kind: ExitFailureKind) -> Self {
        Self {
            exit_code,
            message: message.into(),
            use_stderr: true,
            message_termination: ExitMessageTermination::Line,
            kind,
        }
    }

    pub(super) fn new_stdout(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
            use_stderr: false,
            message_termination: ExitMessageTermination::Line,
            kind: ExitFailureKind::Generic,
        }
    }

    pub(super) fn new_stdout_exact(exit_code: i32, message: impl Into<String>) -> Self {
        Self {
            exit_code,
            message: message.into(),
            use_stderr: false,
            message_termination: ExitMessageTermination::Exact,
            kind: ExitFailureKind::Generic,
        }
    }

    pub(super) fn from_clap(error: clap::Error) -> Self {
        let exit_code = match error.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => 0,
            _ => 1,
        };
        let message = tmux_compat_clap_message(&error);

        Self {
            exit_code,
            message,
            use_stderr: error.use_stderr(),
            message_termination: ExitMessageTermination::Line,
            kind: ExitFailureKind::Generic,
        }
    }

    pub(super) fn from_client(error: ClientError) -> Self {
        let kind = match &error {
            ClientError::Protocol(RmuxError::UnsupportedWireVersion { got, .. }) => {
                ExitFailureKind::UnsupportedWireVersion { got: *got }
            }
            ClientError::Io(source) if source.kind() == std::io::ErrorKind::TimedOut => {
                ExitFailureKind::Timeout
            }
            _ => ExitFailureKind::Generic,
        };
        Self::new_with_kind(1, error.to_string(), kind)
    }

    pub(super) fn from_client_connect(socket_path: &Path, error: ClientError) -> Self {
        if let Some(message) = tmux_client_connect_error_message(socket_path, &error) {
            return Self::new_with_kind(1, message, ExitFailureKind::ServerAbsent);
        }

        Self::from_client(error)
    }

    pub(super) fn from_auto_start(error: AutoStartError) -> Self {
        Self::new(1, error.to_string())
    }

    pub(super) fn with_socket_context(self, socket_path: &Path) -> Self {
        if let ExitFailureKind::UnsupportedWireVersion { got } = self.kind {
            return Self::incompatible_daemon(socket_path, got);
        }
        self
    }

    fn incompatible_daemon(socket_path: &Path, daemon_wire_version: u32) -> Self {
        let diagnostic = if (1..rmux_proto::RMUX_WIRE_VERSION).contains(&daemon_wire_version) {
            format!(
                "rmux: running daemon from an older release uses incompatible protocol {daemon_wire_version}; this client supports protocol {} on '{}'.",
                rmux_proto::RMUX_WIRE_VERSION,
                socket_path.display()
            )
        } else {
            format!(
                "rmux: running daemon on '{}' uses an incompatible protocol.",
                socket_path.display()
            )
        };
        Self::new(
            1,
            format!(
                "{diagnostic}\nrmux: run `{}` to stop it, then retry.",
                incompatible_daemon_kill_server_command(socket_path)
            ),
        )
    }
}

fn incompatible_daemon_kill_server_command(socket_path: &Path) -> String {
    if default_socket_path()
        .ok()
        .as_deref()
        .is_some_and(|default_path| default_path == socket_path)
    {
        return "rmux kill-server".to_owned();
    }

    format!("rmux -S {} kill-server", shell_quote_path(socket_path))
}

fn shell_quote_path(path: &Path) -> String {
    let text = path.display().to_string();
    if !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+=:@".contains(&byte))
    {
        return text;
    }

    #[cfg(windows)]
    {
        format!("\"{}\"", text.replace('"', "\"\""))
    }
    #[cfg(not(windows))]
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn tmux_compat_clap_message(error: &clap::Error) -> String {
    let message = error.to_string().trim_end().to_owned();
    let first_line = message.lines().next().unwrap_or(message.as_str());
    if message == "error: size missing"
        || message == "error: command join-pane: size missing"
        || message == "error: command move-pane: size missing"
    {
        return "size missing".to_owned();
    }
    if first_line.contains("invalid session name: session names must be non-empty") {
        return "invalid session: ".to_owned();
    }
    if let Some((_, detail)) = first_line.rsplit_once(": ") {
        if let Some(normalized) = normalized_invalid_value_detail(detail) {
            return normalized;
        }
    }
    if let Some(stripped) = message.strip_prefix("error: ") {
        if normalized_invalid_value_detail(stripped).is_some() {
            return stripped.to_owned();
        }
    }
    if let Some(stripped) = message.strip_prefix("error: command ") {
        return format!("command {stripped}");
    }
    // A command's `-?` prints tmux's usage line, whose trailing space is
    // kept for a command without flags; help requests keep their text.
    if error.kind() == clap::error::ErrorKind::UnknownArgument {
        let rendered = error.to_string();
        if let Some(usage) = rendered.strip_prefix("error: usage: ") {
            return format!("usage: {}", usage.strip_suffix('\n').unwrap_or(usage));
        }
    }
    if let Some((_, option)) = message.rsplit_once(": invalid option: ") {
        let option = option.lines().next().unwrap_or(option);
        return format!("invalid option: {option}");
    }
    message
}

fn normalized_invalid_value_detail(detail: &str) -> Option<String> {
    let is_display_message_delay_error = DisplayMessageDurationParseError::ALL
        .into_iter()
        .any(|error| error.as_str() == detail);
    if is_display_message_delay_error
        || matches!(
            detail,
            "width too small"
                | "width invalid"
                | "width too large"
                | "height too small"
                | "height invalid"
                | "height too large"
                | "adjustment invalid"
                | "adjustment too small"
                | "adjustment too large"
        )
    {
        return Some(detail.to_owned());
    }

    None
}

impl From<NestedContextError> for ExitFailure {
    fn from(error: NestedContextError) -> Self {
        Self::new(1, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::tmux_compat_clap_message;

    #[test]
    fn incompatible_daemon_diagnostic_retains_received_and_supported_protocols() {
        let socket = std::path::Path::new("/private/rmux-diagnostic/socket");
        let failure = super::ExitFailure::from_client(rmux_client::ClientError::Protocol(
            rmux_proto::RmuxError::UnsupportedWireVersion {
                got: 8,
                minimum: 9,
                maximum: 9,
            },
        ));
        assert!(failure.is_unsupported_wire_version());
        let failure = failure.with_socket_context(socket);
        assert_eq!(failure.exit_code(), 1);
        assert!(failure.use_stderr());
        assert_eq!(
            failure.message_termination(),
            super::ExitMessageTermination::Line
        );
        assert_eq!(
            failure.message(),
            "rmux: running daemon from an older release uses incompatible protocol 8; this client supports protocol 9 on '/private/rmux-diagnostic/socket'.\nrmux: run `rmux -S /private/rmux-diagnostic/socket kill-server` to stop it, then retry."
        );

        for got in [
            0,
            rmux_proto::RMUX_WIRE_VERSION,
            rmux_proto::RMUX_WIRE_VERSION + 1,
            u32::MAX,
        ] {
            let failure = super::ExitFailure::from_client(rmux_client::ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedWireVersion {
                    got,
                    minimum: 9,
                    maximum: 9,
                },
            ))
            .with_socket_context(socket);
            assert_eq!(failure.exit_code(), 1);
            assert!(failure.use_stderr());
            assert_eq!(
                failure.message_termination(),
                super::ExitMessageTermination::Line
            );
            assert_eq!(
                failure.message(),
                "rmux: running daemon on '/private/rmux-diagnostic/socket' uses an incompatible protocol.\nrmux: run `rmux -S /private/rmux-diagnostic/socket kill-server` to stop it, then retry.",
                "protocol {got} does not establish an older daemon"
            );
        }
    }

    #[test]
    fn display_message_delay_errors_keep_single_tmux_line() {
        for parse_error in [
            rmux_proto::DisplayMessageDurationParseError::Invalid,
            rmux_proto::DisplayMessageDurationParseError::TooSmall,
            rmux_proto::DisplayMessageDurationParseError::TooLarge,
        ] {
            let message = parse_error.to_string();
            let error = clap::Error::raw(clap::error::ErrorKind::InvalidValue, message.as_str());

            assert_eq!(tmux_compat_clap_message(&error), message);
        }
    }

    #[test]
    fn unrelated_clap_errors_keep_their_normal_prefix() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "unrelated invalid value",
        );

        assert_eq!(
            tmux_compat_clap_message(&error),
            "error: unrelated invalid value"
        );
    }

    #[test]
    fn clap_invalid_option_value_errors_keep_single_tmx_line() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "error: invalid value 'no-such-hook' for '<HOOK>': invalid option: no-such-hook\n\nFor more information, try '--help'.",
        );

        assert_eq!(
            tmux_compat_clap_message(&error),
            "invalid option: no-such-hook"
        );
    }

    #[test]
    fn resize_pane_dimension_errors_keep_single_tmux_line() {
        let error = clap::Error::raw(clap::error::ErrorKind::ValueValidation, "width too small");

        assert_eq!(tmux_compat_clap_message(&error), "width too small");
    }

    #[test]
    fn resize_pane_adjustment_errors_keep_single_tmux_line() {
        for message in [
            "adjustment invalid",
            "adjustment too small",
            "adjustment too large",
        ] {
            let error = clap::Error::raw(clap::error::ErrorKind::ValueValidation, message);

            assert_eq!(tmux_compat_clap_message(&error), message);
        }
    }

    #[test]
    fn invalid_value_dimension_errors_keep_single_tmux_line() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "error: invalid value '70000' for '-x <COLS>': width too large\n\nFor more information, try '--help'.",
        );

        assert_eq!(tmux_compat_clap_message(&error), "width too large");
    }

    #[test]
    fn empty_session_name_errors_keep_tmux_line() {
        let error = clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "error: invalid value '' for '<NEW_NAME>': invalid session name: session names must be non-empty\n\nFor more information, try '--help'.",
        );

        assert_eq!(tmux_compat_clap_message(&error), "invalid session: ");
    }
}
