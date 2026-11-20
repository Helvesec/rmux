#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use crate::control_mode::ControlModeUpgrade;
#[cfg(any(unix, windows))]
use crate::daemon::ShutdownHandle;
#[cfg(any(unix, windows))]
use crate::handler::{
    with_control_command_response_sink, with_control_queue_eof_cancellation,
    with_control_queue_identity, ControlClientIdentity, ControlCommandResponseSink,
    ControlQueueDrainLease, ControlQueueEofCancellation, RequestHandler,
};
#[cfg(any(unix, windows))]
use rmux_core::command_parser::{CommandArgument, ParsedCommands};
#[cfg(any(unix, windows))]
use rmux_ipc::LocalStream;
#[cfg(windows)]
use rmux_proto::CONTROL_STDIN_EOF_MARKER;
#[cfg(any(unix, windows))]
use rmux_proto::{format_guard_line, ControlGuardKind};
use rmux_proto::{ControlMode, RmuxError, SessionName, MAX_INITIAL_CONTROL_COMMANDS};
#[cfg(any(unix, windows))]
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(any(unix, windows))]
use std::io;
#[cfg(any(unix, windows))]
use std::pin::Pin;
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(all(test, unix))]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(any(unix, windows))]
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(any(unix, windows))]
use tokio::io::WriteHalf;
#[cfg(all(test, unix))]
use tokio::sync::Notify;
#[cfg(any(unix, windows))]
use tokio::sync::{mpsc, watch};
#[cfg(any(unix, windows))]
use tokio::task::JoinHandle;

#[cfg(any(unix, windows))]
mod input;
#[cfg(test)]
use input::append_control_input;
#[cfg(any(unix, windows))]
use input::append_control_input_with_rejection;
#[cfg(any(unix, windows))]
pub(crate) use input::MAX_CONTROL_LINE_BYTES;
#[cfg(all(test, unix))]
use input::{extract_complete_control_lines, MAX_QUEUED_CONTROL_LINES};

mod output_queue;
#[cfg(any(unix, windows))]
use output_queue::{ensure_control_newline, flush_output_queue, ControlOutputQueue};

mod pane_output_state;
#[cfg(any(unix, windows))]
pub(crate) use pane_output_state::{ControlPaneOutputAction, ControlPaneOutputActionKind};
#[cfg(any(unix, windows))]
use pane_output_state::{ControlPaneOutputRefresh, ControlPaneOutputStates};

mod transport_drain;
#[cfg(all(test, unix))]
use transport_drain::install_control_transport_drain_pause;
#[cfg(any(unix, windows))]
use transport_drain::ControlTransportDrain;

mod command_numbering;
#[cfg(any(unix, windows))]
use command_numbering::{ControlCommandNumbering, ControlCommandOrigin};

mod command_validation;
#[cfg(any(unix, windows))]
use command_validation::validate_control_command_arguments;

mod pane_event_delivery;
mod subscriptions;
#[cfg(any(unix, windows))]
use crate::pane_io::{PaneOutputAttachedClientReadDemandGuard, PaneOutputReadDemandGuard};
#[cfg(any(unix, windows))]
use pane_event_delivery::PaneEventSender;
#[cfg(any(unix, windows))]
use subscriptions::{
    drain_ready_pane_events, handle_pane_event, refresh_subscription_for_pane,
    refresh_subscriptions, PaneEvent, PaneSubscription, PaneSubscriptionStart,
    PaneSubscriptionState,
};

pub(crate) mod format_subscriptions;
#[cfg(any(unix, windows))]
pub(crate) use format_subscriptions::{
    validate_control_format_definition_batch, ControlFormatLimitExceeded,
    ControlFormatNotificationBatch, ControlFormatSubscriptions,
};

mod session_attachment;
#[cfg(any(unix, windows))]
use session_attachment::ControlSessionAttachment;

mod eof_completion;
#[cfg(any(unix, windows))]
use eof_completion::ControlEofCompletion;

#[cfg(any(unix, windows))]
const MAX_DEFERRED_CONTROL_NOTIFICATIONS: usize = 1024;
#[cfg(any(unix, windows))]
const MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES: usize = 4 * 1024 * 1024;
#[cfg(any(unix, windows))]
const CONTROL_PANE_EVENT_CAPACITY: usize = 256;
#[cfg(any(unix, windows))]
pub(crate) const CONTROL_SERVER_EVENT_CAPACITY: usize = 256;
pub(crate) fn validate_initial_control_command_count(count: usize) -> Result<(), RmuxError> {
    if count <= MAX_INITIAL_CONTROL_COMMANDS {
        return Ok(());
    }
    Err(RmuxError::Server(format!(
        "too many initial control-mode commands: {count} (maximum {MAX_INITIAL_CONTROL_COMMANDS})"
    )))
}

#[cfg(any(unix, windows))]
fn control_commands_require_drain(commands: &ParsedCommands) -> bool {
    !commands.assignments().is_empty()
        || !commands
            .commands()
            .iter()
            .all(control_command_is_cancel_safe_wait)
}

#[cfg(any(unix, windows))]
fn control_command_is_cancel_safe_wait(command: &rmux_core::command_parser::ParsedCommand) -> bool {
    if command.name() != "wait-for" {
        return false;
    }
    match command.arguments() {
        [CommandArgument::String(_channel)] => true,
        [CommandArgument::String(flag), CommandArgument::String(_channel)] => {
            matches!(flag.as_str(), "--" | "-L")
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ControlClientFlags {
    pub(crate) pause_after_millis: Option<u64>,
    pub(crate) no_output: bool,
    pub(crate) wait_exit: bool,
    pub(crate) ignore_size: bool,
    /// tmux's `CLIENT_READONLY` on a control client: set by `attach-session -r`
    /// or `-f read-only`, toggled by `switch-client -r`. It refuses this
    /// client's `send-keys` (other than `-X`); its other commands still run.
    pub(crate) read_only: bool,
}

impl ControlClientFlags {
    #[must_use]
    pub(crate) const fn uses_extended_output(self) -> bool {
        self.pause_after_millis.is_some()
    }

    pub(crate) fn apply_flag_string(&mut self, flags: &str) {
        for token in flags.split(',').filter(|token| !token.is_empty()) {
            let (remove, token) = token
                .strip_prefix('!')
                .map_or((false, token), |token| (true, token));
            match token {
                "ignore-size" | "ignoresize" => self.ignore_size = !remove,
                "no-output" => self.no_output = !remove,
                "wait-exit" => self.wait_exit = !remove,
                // tmux 3.7b `server_client_set_flags()` masks `!read-only` off a
                // read-only client, so the flag string can set it but never
                // clear it; only `switch-client -r` does.
                "read-only" | "readonly" => self.read_only |= !remove,
                "pause-after" => self.pause_after_millis = (!remove).then_some(0),
                token => {
                    let Some(seconds) = token
                        .strip_prefix("pause-after=")
                        .and_then(|value| value.parse::<u32>().ok())
                    else {
                        continue;
                    };
                    if !remove {
                        self.pause_after_millis = Some(u64::from(seconds).saturating_mul(1000));
                    } else {
                        self.pause_after_millis = None;
                    }
                }
            }
        }
    }

    /// tmux 3.7b `switch-client -r`: a read-only client loses both
    /// `CLIENT_READONLY` and `CLIENT_IGNORESIZE`, any other gains both.
    pub(crate) fn toggle_read_only(&mut self) {
        let read_only = !self.read_only;
        self.read_only = read_only;
        self.ignore_size = read_only;
    }
}

/// The flag change a control-session commit applies to its client, on a
/// prospective copy, so the geometry chosen under the same locks already
/// counts the client with its new `ignore-size`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ControlFlagChange {
    #[default]
    Keep,
    /// `attach-session [-r] [-f flags]`: tmux 3.7b `cmd_attach_session()` runs
    /// `server_client_set_flags()` on the `-f` list, then `-r` adds
    /// `CLIENT_READONLY|CLIENT_IGNORESIZE`.
    Attach { flags: Vec<String>, read_only: bool },
    /// `switch-client -r`.
    ToggleReadOnly,
}

impl ControlFlagChange {
    pub(crate) fn apply(&self, flags: &mut ControlClientFlags) {
        match self {
            Self::Keep => {}
            Self::Attach {
                flags: values,
                read_only,
            } => {
                for value in values {
                    flags.apply_flag_string(value);
                }
                if *read_only {
                    flags.read_only = true;
                    flags.ignore_size = true;
                }
            }
            Self::ToggleReadOnly => flags.toggle_read_only(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ControlServerEvent {
    SessionChanged(Option<SessionName>),
    SessionChangedAt {
        session_name: SessionName,
        pane_sequences: Vec<(u32, u64)>,
    },
    FormatSubscriptions(Vec<String>),
    PaneOutputActions(Vec<ControlPaneOutputAction>),
    Refresh,
    Notification(String),
    Exit(Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlQueueCommandOrigin {
    SourceFile { depth: usize },
    IfShell,
    RunShell { depth: usize },
    QueueContinuation,
}

#[derive(Debug, Clone)]
pub(crate) enum ControlCommandResponseEvent {
    FrameStarted {
        origin: ControlQueueCommandOrigin,
    },
    Notification(String),
    PaneOutputActions(Vec<ControlPaneOutputAction>),
    FrameCompleted {
        stdout: Vec<u8>,
        error: Option<RmuxError>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ControlCommandResult {
    pub(crate) stdout: Vec<u8>,
    pub(crate) error: Option<rmux_proto::RmuxError>,
    pub(crate) source_file_error: Option<rmux_proto::RmuxError>,
    pub(crate) execution_error: Option<rmux_proto::RmuxError>,
    pub(crate) exit_status: Option<i32>,
    pub(crate) server_shutdown_started: bool,
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(crate) struct ControlLifecycle {
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) shutdown_handle: ShutdownHandle,
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(crate) struct ControlUpgradeInput {
    buffered_bytes: Vec<u8>,
    initial_command_count: usize,
    mode: ControlMode,
}

#[cfg(any(unix, windows))]
impl ControlUpgradeInput {
    #[cfg(test)]
    pub(crate) fn new(buffered_bytes: Vec<u8>, initial_command_count: usize) -> Self {
        Self::with_mode(buffered_bytes, initial_command_count, ControlMode::Plain)
    }

    pub(crate) fn with_mode(
        buffered_bytes: Vec<u8>,
        initial_command_count: usize,
        mode: ControlMode,
    ) -> Self {
        Self {
            buffered_bytes,
            initial_command_count,
            mode,
        }
    }
}

#[cfg(any(unix, windows))]
pub(crate) async fn forward_control(
    stream: LocalStream,
    handler: Arc<RequestHandler>,
    control_identity: ControlClientIdentity,
    upgrade_input: ControlUpgradeInput,
    shutdown: watch::Receiver<()>,
    server_events: mpsc::Receiver<ControlServerEvent>,
    lifecycle: ControlLifecycle,
) -> io::Result<()> {
    with_control_queue_identity(
        control_identity,
        forward_control_inner(
            stream,
            handler,
            control_identity,
            upgrade_input,
            shutdown,
            server_events,
            lifecycle,
        ),
    )
    .await
}

#[cfg(any(unix, windows))]
mod timing;
#[cfg(any(unix, windows))]
use timing::*;

#[cfg(any(unix, windows))]
mod read_demand;
#[cfg(any(unix, windows))]
use read_demand::*;

#[cfg(any(unix, windows))]
mod eof;
#[cfg(any(unix, windows))]
use eof::*;

#[cfg(any(unix, windows))]
mod server_events;
#[cfg(any(unix, windows))]
use server_events::*;

#[cfg(any(unix, windows))]
mod response_events;
#[cfg(any(unix, windows))]
use response_events::*;

#[cfg(any(unix, windows))]
mod event_state;
#[cfg(any(unix, windows))]
use event_state::*;

#[cfg(any(unix, windows))]
mod active_command;
#[cfg(any(unix, windows))]
use active_command::*;

#[cfg(any(unix, windows))]
mod transport;
#[cfg(any(unix, windows))]
use transport_state::forward_control_inner;
#[cfg(any(unix, windows))]
mod transport_commands;
#[cfg(any(unix, windows))]
mod transport_completion;
#[cfg(any(unix, windows))]
mod transport_eof;
#[cfg(any(unix, windows))]
mod transport_state;

#[cfg(windows)]
fn consume_control_eof_marker(
    input_buffer: &mut Vec<u8>,
    queued_lines: &mut VecDeque<String>,
    queued_input_bytes: &mut usize,
) -> bool {
    let Some(marker_index) = queued_lines.iter().position(|line| {
        line == CONTROL_STDIN_EOF_MARKER || line.ends_with(CONTROL_STDIN_EOF_MARKER)
    }) else {
        return false;
    };

    // The Windows client writes this private terminal marker immediately
    // before closing its named-pipe writer. Observe it even while a blocking
    // command is active; waiting for the normal command-dequeue path would
    // deadlock because wait-for cancellation itself depends on input_closed.
    queued_lines.truncate(marker_index);
    *queued_input_bytes = queued_lines.iter().map(String::len).sum();
    input_buffer.clear();
    true
}

#[cfg(any(unix, windows))]
fn unix_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(all(test, any(unix, windows)))]
mod deferred_tests {
    use super::{
        DeferredServerEvents, MAX_DEFERRED_CONTROL_NOTIFICATIONS,
        MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES,
    };

    #[test]
    fn deferred_control_notifications_are_bounded() {
        let mut deferred = DeferredServerEvents::default();

        for index in 0..MAX_DEFERRED_CONTROL_NOTIFICATIONS {
            deferred.defer_notification(format!("%message {index}"));
        }

        assert_eq!(
            deferred.notifications.len(),
            MAX_DEFERRED_CONTROL_NOTIFICATIONS
        );
        assert!(deferred.exit_reason.is_none());

        deferred.defer_notification("%message overflow".to_owned());

        assert!(deferred.notifications.is_empty());
        assert_eq!(
            deferred.exit_reason,
            Some(Some("control notification queue exceeded".to_owned()))
        );

        deferred.defer_notification("%message after-overflow".to_owned());
        assert!(deferred.notifications.is_empty());
    }

    #[test]
    fn deferred_control_notification_bytes_are_bounded_and_accounted_on_pop() {
        let mut deferred = DeferredServerEvents::default();
        let first = "x".repeat(MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2);
        let second = "y".repeat(MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2);
        deferred.defer_notification(first.clone());
        deferred.defer_notification(second);
        assert_eq!(
            deferred.notification_bytes,
            MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES
        );

        assert_eq!(deferred.pop_notification(), Some(first));
        assert_eq!(
            deferred.notification_bytes,
            MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2
        );

        deferred.defer_notification("z".repeat(MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2 + 1));
        assert!(deferred.notifications.is_empty());
        assert_eq!(deferred.notification_bytes, 0);
        assert!(deferred.exit_reason.is_some());
    }
}

#[cfg(all(test, windows))]
mod windows_eof_marker_tests {
    use std::collections::VecDeque;

    use super::{
        append_control_input, arm_control_eof_transition, consume_control_eof_marker,
        wait_for_control_eof_transition, CONTROL_EOF_GRACE,
    };
    use rmux_proto::CONTROL_STDIN_EOF_MARKER;

    #[test]
    fn windows_eof_marker_discards_an_incomplete_command_prefix() {
        let mut input_buffer = Vec::new();
        let mut queued_lines = VecDeque::new();
        let mut queued_bytes = 0;
        let input = format!("display-message -p must-not-run{CONTROL_STDIN_EOF_MARKER}\n");
        append_control_input(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
            input.as_bytes(),
        )
        .expect("private EOF marker input parses");

        assert!(consume_control_eof_marker(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
        ));
        assert!(queued_lines.is_empty());
        assert_eq!(queued_bytes, 0);
        assert!(input_buffer.is_empty());
    }

    #[tokio::test]
    async fn windows_eof_marker_uses_the_global_persistent_deadline() {
        let mut input_buffer = Vec::new();
        let mut queued_lines = VecDeque::new();
        let mut queued_bytes = 0;
        append_control_input(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
            format!("wait-for marker{CONTROL_STDIN_EOF_MARKER}\n").as_bytes(),
        )
        .expect("private EOF marker input parses");
        assert!(consume_control_eof_marker(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
        ));

        let mut transition = None;
        arm_control_eof_transition(&mut transition);
        let initial_deadline = transition
            .as_ref()
            .expect("marker deadline is armed")
            .deadline();
        arm_control_eof_transition(&mut transition);
        assert_eq!(
            transition
                .as_ref()
                .expect("marker deadline stays armed")
                .deadline(),
            initial_deadline,
            "another post-marker frame must not extend the global budget"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                wait_for_control_eof_transition(&mut transition),
            )
            .await
            .is_err(),
            "the marker deadline leaves a bounded grace for fast output"
        );
        assert!(
            tokio::time::timeout(
                CONTROL_EOF_GRACE + std::time::Duration::from_millis(100),
                wait_for_control_eof_transition(&mut transition),
            )
            .await
            .is_ok(),
            "the marker deadline still expires within its global budget"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "control/tests.rs"]
mod tests;
