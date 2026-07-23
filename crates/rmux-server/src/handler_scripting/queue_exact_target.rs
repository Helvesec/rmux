use rmux_core::{
    command_inventory::command_short_option_spec,
    command_parser::{CommandArgument, ParsedCommand},
};
use rmux_proto::{Request, RmuxError, Target};

use crate::pane_terminals::HandlerState;

use super::super::StableTargetIdentity;
use super::queue::QueueInvocation;

#[cfg(test)]
#[path = "queue_exact_target/test_pause.rs"]
mod test_pause;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueExactTargetRole {
    Window,
    Pane,
}

pub(super) const QUEUE_EXACT_TARGET_COVERAGE: &[(&str, QueueExactTargetRole)] = &[
    ("rename-window", QueueExactTargetRole::Window),
    ("kill-pane", QueueExactTargetRole::Pane),
];

#[derive(Debug)]
pub(super) enum QueueExactTargetCapture {
    NotCovered,
    OptionAbsent,
    Captured(StableTargetIdentity),
    RequiredUnavailable(RmuxError),
}

impl QueueExactTargetCapture {
    pub(super) fn capture(
        command: &ParsedCommand,
        invocation: &QueueInvocation,
        state: &mut HandlerState,
    ) -> Self {
        let Some((declared_command, role)) = QUEUE_EXACT_TARGET_COVERAGE
            .iter()
            .find(|(declared_command, _)| *declared_command == command.name())
        else {
            return Self::NotCovered;
        };
        let Some(has_target) = has_short_option_value(command, 't') else {
            return Self::RequiredUnavailable(RmuxError::Server(format!(
                "queued {} target guard has no short-option inventory",
                declared_command
            )));
        };
        if !has_target {
            return Self::OptionAbsent;
        }
        let target = match (*role, invocation) {
            (
                QueueExactTargetRole::Window,
                QueueInvocation::Request(Request::RenameWindow(request)),
            ) => Target::Window(request.target.clone()),
            (QueueExactTargetRole::Pane, QueueInvocation::Request(Request::KillPane(request))) => {
                Target::Pane(request.target.clone())
            }
            _ => {
                return Self::RequiredUnavailable(RmuxError::Server(format!(
                    "queued {} target guard did not receive its declared request role",
                    declared_command
                )))
            }
        };
        match StableTargetIdentity::capture(state, target) {
            Ok(identity) => Self::Captured(identity),
            Err(error) => Self::RequiredUnavailable(error),
        }
    }

    pub(super) fn into_identity(self) -> Result<Option<StableTargetIdentity>, RmuxError> {
        match self {
            Self::NotCovered | Self::OptionAbsent => Ok(None),
            Self::Captured(identity) => Ok(Some(identity)),
            Self::RequiredUnavailable(error) => Err(error),
        }
    }
}

pub(super) fn has_short_option_value(command: &ParsedCommand, expected: char) -> Option<bool> {
    let spec = command_short_option_spec(command.name())?;
    let arguments = command.arguments();
    let mut index = 0;
    while index < arguments.len() {
        let Some(argument) = arguments[index].as_string() else {
            index += 1;
            continue;
        };
        if argument == "--" {
            break;
        }
        let Some(flags) = argument.strip_prefix('-').filter(|flags| !flags.is_empty()) else {
            index += 1;
            continue;
        };
        if flags.starts_with('-') {
            index += 1;
            continue;
        }
        let mut characters = flags.char_indices().peekable();
        while let Some((_, flag)) = characters.next() {
            if spec.takes_value(flag) {
                let attached_start = characters
                    .peek()
                    .map(|(offset, _)| *offset)
                    .unwrap_or(flags.len());
                let has_value = attached_start < flags.len()
                    || arguments
                        .get(index + 1)
                        .and_then(CommandArgument::as_string)
                        .is_some();
                if flag == expected && has_value {
                    return Some(true);
                }
                if attached_start == flags.len() {
                    index += 1;
                }
                break;
            }
            if !spec.is_boolean(flag) {
                break;
            }
        }
        index += 1;
    }
    Some(false)
}

#[cfg(test)]
pub(crate) use test_pause::{
    install_queue_exact_target_capture_pause, pause_after_queue_exact_target_capture,
    QueueExactTargetCapturePause,
};

#[cfg(test)]
mod tests {
    use rmux_core::command_parser::CommandParser;
    use rmux_proto::{KillPaneRequest, PaneTarget, RenameWindowRequest, SessionName, WindowTarget};

    use super::*;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    #[test]
    fn coverage_inventory_has_one_unique_role_per_command() {
        let mut commands = QUEUE_EXACT_TARGET_COVERAGE
            .iter()
            .map(|(command, _)| *command)
            .collect::<Vec<_>>();
        commands.sort_unstable();
        commands.dedup();
        assert_eq!(commands.len(), QUEUE_EXACT_TARGET_COVERAGE.len());
        for (command, _) in QUEUE_EXACT_TARGET_COVERAGE {
            let spec = command_short_option_spec(command)
                .unwrap_or_else(|| panic!("{command} must have a short-option inventory"));
            assert!(
                spec.takes_value('t'),
                "{command} must declare its guarded -t value"
            );
        }
    }

    #[test]
    fn covered_command_without_target_is_explicitly_absent() {
        let command = CommandParser::new()
            .parse("rename-window guarded")
            .expect("command parses")
            .commands()[0]
            .clone();
        let target = session_name("alpha");
        let invocation = QueueInvocation::Request(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(target, 0),
            name: "guarded".to_owned(),
        }));
        let mut state = HandlerState::default();
        assert!(matches!(
            QueueExactTargetCapture::capture(&command, &invocation, &mut state),
            QueueExactTargetCapture::OptionAbsent
        ));
    }

    #[test]
    fn explicit_target_capture_failure_is_not_treated_as_absence() {
        let command = CommandParser::new()
            .parse("rename-window -t @999 unavailable")
            .expect("command parses")
            .commands()[0]
            .clone();
        let missing = session_name("missing");
        let invocation = QueueInvocation::Request(Request::RenameWindow(RenameWindowRequest {
            target: WindowTarget::with_window(missing, 0),
            name: "unavailable".to_owned(),
        }));
        let mut state = HandlerState::default();
        assert!(matches!(
            QueueExactTargetCapture::capture(&command, &invocation, &mut state),
            QueueExactTargetCapture::RequiredUnavailable(_)
        ));
    }

    #[test]
    fn declared_request_role_mismatch_fails_closed() {
        let command = CommandParser::new()
            .parse("rename-window -t @1 guarded")
            .expect("command parses")
            .commands()[0]
            .clone();
        let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
        let invocation = QueueInvocation::Request(Request::KillPane(KillPaneRequest {
            target,
            kill_all_except: false,
        }));
        let mut state = HandlerState::default();
        assert!(matches!(
            QueueExactTargetCapture::capture(&command, &invocation, &mut state),
            QueueExactTargetCapture::RequiredUnavailable(_)
        ));
    }
}
