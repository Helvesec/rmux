use rmux_core::{
    command_inventory::command_short_option_spec,
    command_parser::{CommandArgument, ParsedCommand},
    ManagedSessionStore, TargetFindContext,
};
use rmux_proto::{Request, RmuxError, Target};

use crate::pane_terminals::HandlerState;

use super::super::StableTargetIdentity;
use super::implicit_pane_target;
use super::queue::QueueInvocation;

#[cfg(test)]
#[path = "../../handler_scripting/queue_exact_target/test_pause.rs"]
mod test_pause;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueueExactTargetRole {
    Window,
    Pane,
}

pub(super) const QUEUE_EXACT_TARGET_COVERAGE: &[(&str, QueueExactTargetRole)] = &[
    ("rename-window", QueueExactTargetRole::Window),
    ("kill-pane", QueueExactTargetRole::Pane),
    ("new-pane", QueueExactTargetRole::Pane),
];

#[derive(Debug)]
pub(super) enum QueueExactTargetCapture {
    NotCovered,
    OptionAbsent,
    Captured(StableTargetIdentity),
    RequiredUnavailable(RmuxError),
}

impl QueueExactTargetCapture {
    /// tmux 3.7b `cmd_find_target()` takes an omitted target from the queue
    /// item's own state (`cmdq_get_current()`: the target a hook inherited
    /// from the command that fired it) and asks the client
    /// (`cmd_find_from_client()`) only when that state is absent. Bind ordinary
    /// pane input to an explicit queue state here, before any client binding;
    /// [`Self::bind`] keeps the client fallback for every other queue.
    ///
    /// The caller passes only explicit states (command hooks, shell branches,
    /// sourced files, menus). A key binding's state is implicit: tmux moves it
    /// with `select-pane`, and the client fallback is what follows that move
    /// here. A lifecycle hook passes only while its notification state is
    /// valid; otherwise its target is a substitute for a removed object, where
    /// tmux has no state and asks the client.
    pub(super) fn bind_queue_current(
        invocation: &mut QueueInvocation,
        sessions: &ManagedSessionStore,
        find_context: &TargetFindContext,
    ) -> Result<(), RmuxError> {
        if find_context.current_follows_client() || find_context.current().is_none() {
            return Ok(());
        }
        let target = match invocation {
            QueueInvocation::Request(Request::SendKeysExt(request))
                if !request.dispatch_key_table && !request.forward_mouse_event =>
            {
                &mut request.target
            }
            QueueInvocation::Request(Request::SendKeysExt2(request))
                if !request.dispatch_key_table && !request.forward_mouse_event =>
            {
                &mut request.target
            }
            _ => return Ok(()),
        };
        if target.is_none() {
            *target = Some(implicit_pane_target(sessions, find_context, "send-keys")?);
        }
        Ok(())
    }

    // Ordinary pane input must bind the implicit pane under the same lock as
    // parsing, before the queue can yield. Client key-table/mouse dispatch has
    // its own target selection and is deliberately left on that entry path.
    pub(super) fn bind(
        invocation: &mut QueueInvocation,
        state: &HandlerState,
        current_session: Option<&rmux_proto::SessionName>,
    ) -> Result<(), RmuxError> {
        let (target, keyed_route, names_client) = match invocation {
            QueueInvocation::Request(Request::SendKeysExt(request)) => (
                &mut request.target,
                request.dispatch_key_table || request.forward_mouse_event,
                false,
            ),
            QueueInvocation::Request(Request::SendKeysExt2(request)) => (
                &mut request.target,
                request.dispatch_key_table || request.forward_mouse_event,
                true,
            ),
            _ => return Ok(()),
        };
        if target.is_some() || keyed_route {
            return Ok(());
        }
        match super::super::pane_support::resolve_input_target(state, None, current_session) {
            Ok(resolved) => *target = Some(resolved),
            // With `-c`, send-keys judges the named client before its target:
            // a missing client stays a quiet no-op, so leave the target to
            // the handler, which resolves it from the same requester.
            Err(_) if names_client => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    pub(super) fn capture(
        command: &ParsedCommand,
        invocation: &QueueInvocation,
        state: &mut HandlerState,
    ) -> Self {
        if command.name() == "send-keys" {
            let target = match invocation {
                QueueInvocation::Request(Request::SendKeys(request)) => Some(&request.target),
                QueueInvocation::Request(Request::SendKeysExt(request))
                    if !request.dispatch_key_table && !request.forward_mouse_event =>
                {
                    request.target.as_ref()
                }
                QueueInvocation::Request(Request::SendKeysExt2(request))
                    if !request.dispatch_key_table && !request.forward_mouse_event =>
                {
                    request.target.as_ref()
                }
                _ => None,
            };
            return match target {
                Some(target) => {
                    match StableTargetIdentity::capture(state, Target::Pane(target.clone())) {
                        Ok(identity) => Self::Captured(identity),
                        Err(error) => Self::RequiredUnavailable(error),
                    }
                }
                None => Self::NotCovered,
            };
        }
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
        let target = match (*role, invocation) {
            // `new-pane` is the one covered command whose target is already
            // resolved when it reaches this guard: an omitted `-t` was bound to
            // the pane one is on while the parse held the lock, and the queue
            // then waits. That pane has to be held to its identity exactly like
            // a written one, or whatever replaces it between the parse and the
            // commit receives an opening addressed to its predecessor.
            (QueueExactTargetRole::Pane, QueueInvocation::NewPane(command)) => {
                Target::Pane(command.target.clone())
            }
            _ if !has_target => return Self::OptionAbsent,
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
