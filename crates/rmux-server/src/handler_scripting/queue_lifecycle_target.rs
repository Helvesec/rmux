use std::sync::Arc;

use rmux_core::{command_parser::ParsedCommand, SessionStore, TargetFindContext};
use rmux_proto::{Request, RmuxError, Target};

use super::super::lifecycle_support::{LeaseResolution, LifecycleTargetLease};
use super::super::StableTargetIdentity;
use super::implicit_pane_target;
use super::queue::QueueInvocation;
use super::queue_exact_target::has_short_option_value;
use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleUse {
    Unused,
    Explicit,
    Implicit,
}

#[derive(Debug)]
pub(super) struct QueueLifecycleTargetPlan {
    command: &'static str,
    primary: RoleUse,
    source: RoleUse,
    destination: RoleUse,
}

#[derive(Debug, Default)]
pub(super) struct QueueLifecycleTargetCapture {
    identities: Vec<StableTargetIdentity>,
    retained_target: Option<Arc<LifecycleTargetLease>>,
    retained_identity: Option<StableTargetIdentity>,
}

impl QueueLifecycleTargetPlan {
    pub(super) fn for_command(command: &ParsedCommand) -> Result<Option<Self>, RmuxError> {
        let target = || role_for_flag(command, 't');
        let plan = match command.name() {
            "rename-window" => Self::primary("rename-window", target()?),
            "capture-pane" => Self::primary("capture-pane", target()?),
            "new-window" => Self::primary("new-window", target()?),
            "lock-session" => Self::primary("lock-session", target()?),
            "send-keys" => {
                let primary = if has_flag(command, 'c')? && !has_flag(command, 't')? {
                    RoleUse::Unused
                } else {
                    target()?
                };
                Self::primary("send-keys", primary)
            }
            "display-message" => {
                let primary = if has_flag(command, 'c')? && !has_flag(command, 't')? {
                    RoleUse::Unused
                } else {
                    target()?
                };
                Self::primary("display-message", primary)
            }
            "join-pane" => Self {
                command: "join-pane",
                primary: RoleUse::Unused,
                source: role_for_flag(command, 's')?,
                destination: target()?,
            },
            _ => return Ok(None),
        };
        Ok(Some(plan))
    }

    fn primary(command: &'static str, primary: RoleUse) -> Self {
        Self {
            command,
            primary,
            source: RoleUse::Unused,
            destination: RoleUse::Unused,
        }
    }

    pub(super) fn resolve_parse_target(
        &self,
        retained_target: &Arc<LifecycleTargetLease>,
        state: &HandlerState,
    ) -> Result<Option<Target>, RmuxError> {
        if !self.uses_any_role() {
            return Ok(None);
        }
        match retained_target.resolve(state) {
            LeaseResolution::Live(target) => Ok(Some(target)),
            LeaseResolution::Retired(_) if self.uses_implicit_role() => {
                Err(RmuxError::Server(format!(
                    "queued {} lifecycle target retired before parsing",
                    self.command
                )))
            }
            LeaseResolution::Replaced if self.uses_implicit_role() => {
                Err(RmuxError::Server(format!(
                    "queued {} lifecycle target was replaced before parsing",
                    self.command
                )))
            }
            LeaseResolution::Retired(_) | LeaseResolution::Replaced => Ok(None),
        }
    }

    pub(super) fn bind_implicit_target(
        &self,
        invocation: &mut QueueInvocation,
        sessions: &SessionStore,
        find_context: &TargetFindContext,
    ) -> Result<(), RmuxError> {
        if self.command != "send-keys" || self.primary != RoleUse::Implicit {
            return Ok(());
        }
        let target = implicit_pane_target(sessions, find_context, "send-keys")?;
        match invocation {
            QueueInvocation::Request(Request::SendKeysExt(request)) if request.target.is_none() => {
                request.target = Some(target);
            }
            QueueInvocation::Request(Request::SendKeysExt2(request))
                if request.target.is_none() =>
            {
                request.target = Some(target);
            }
            QueueInvocation::Request(Request::SendKeys(request)) if request.target == target => {}
            _ => {
                return Err(RmuxError::Server(
                    "queued send-keys lifecycle target could not be bound".to_owned(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn capture(
        self,
        invocation: &QueueInvocation,
        state: &mut HandlerState,
        retained_target: Arc<LifecycleTargetLease>,
    ) -> Result<QueueLifecycleTargetCapture, RmuxError> {
        let retained_identity = if self.uses_implicit_role() {
            match retained_target.resolve(state) {
                LeaseResolution::Live(target) => {
                    Some(StableTargetIdentity::capture(state, target)?)
                }
                LeaseResolution::Retired(_) => {
                    return Err(RmuxError::Server(format!(
                        "queued {} lifecycle target retired during parsing",
                        self.command
                    )))
                }
                LeaseResolution::Replaced => {
                    return Err(RmuxError::Server(format!(
                        "queued {} lifecycle target was replaced during parsing",
                        self.command
                    )))
                }
            }
        } else {
            None
        };
        let mut targets = Vec::new();
        match (self.command, invocation) {
            ("rename-window", QueueInvocation::Request(Request::RenameWindow(request))) => {
                push_used(
                    &mut targets,
                    self.primary,
                    Target::Window(request.target.clone()),
                );
            }
            ("send-keys", QueueInvocation::Request(request)) => {
                if self.primary != RoleUse::Unused {
                    let target =
                        send_keys_target(request).ok_or_else(|| role_unavailable(self.command))?;
                    targets.push(Target::Pane(target));
                }
            }
            ("join-pane", QueueInvocation::Request(Request::JoinPane(request))) => {
                push_used(
                    &mut targets,
                    self.source,
                    Target::Pane(request.source.clone()),
                );
                push_used(
                    &mut targets,
                    self.destination,
                    Target::Pane(request.target.clone()),
                );
            }
            ("new-window", QueueInvocation::NewWindow(command)) => {
                push_used(
                    &mut targets,
                    self.primary,
                    Target::Session(command.target.clone()),
                );
            }
            ("lock-session", QueueInvocation::Request(Request::LockSession(request))) => {
                push_used(
                    &mut targets,
                    self.primary,
                    Target::Session(request.target.clone()),
                );
            }
            ("display-message", QueueInvocation::Request(request)) => {
                if self.primary != RoleUse::Unused {
                    targets.push(
                        display_message_target(request)
                            .ok_or_else(|| role_unavailable(self.command))?,
                    );
                }
            }
            ("capture-pane", QueueInvocation::Request(Request::CapturePane(request))) => {
                push_used(
                    &mut targets,
                    self.primary,
                    Target::Pane(request.target.clone()),
                );
            }
            _ => return Err(role_unavailable(self.command)),
        }

        let identities = targets
            .into_iter()
            .map(|target| StableTargetIdentity::capture(state, target))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(QueueLifecycleTargetCapture {
            identities,
            retained_target: self.uses_implicit_role().then_some(retained_target),
            retained_identity,
        })
    }

    fn uses_implicit_role(&self) -> bool {
        [self.primary, self.source, self.destination].contains(&RoleUse::Implicit)
    }

    fn uses_any_role(&self) -> bool {
        [self.primary, self.source, self.destination]
            .into_iter()
            .any(|role| role != RoleUse::Unused)
    }
}

impl QueueLifecycleTargetCapture {
    pub(super) fn into_parts(
        self,
    ) -> (
        Vec<StableTargetIdentity>,
        Option<Arc<LifecycleTargetLease>>,
        Option<StableTargetIdentity>,
    ) {
        (
            self.identities,
            self.retained_target,
            self.retained_identity,
        )
    }
}

fn role_for_flag(command: &ParsedCommand, flag: char) -> Result<RoleUse, RmuxError> {
    Ok(if has_flag(command, flag)? {
        RoleUse::Explicit
    } else {
        RoleUse::Implicit
    })
}

fn has_flag(command: &ParsedCommand, flag: char) -> Result<bool, RmuxError> {
    has_short_option_value(command, flag).ok_or_else(|| {
        RmuxError::Server(format!(
            "queued {} lifecycle guard has no short-option inventory",
            command.name()
        ))
    })
}

fn push_used(targets: &mut Vec<Target>, role: RoleUse, target: Target) {
    if role != RoleUse::Unused {
        targets.push(target);
    }
}

fn send_keys_target(request: &Request) -> Option<rmux_proto::PaneTarget> {
    match request {
        Request::SendKeys(request) => Some(request.target.clone()),
        Request::SendKeysExt(request) => request.target.clone(),
        Request::SendKeysExt2(request) => request.target.clone(),
        _ => None,
    }
}

fn display_message_target(request: &Request) -> Option<Target> {
    match request {
        Request::DisplayMessage(request) => request.target.clone(),
        Request::DisplayMessageExt(request) => request.target.clone(),
        _ => None,
    }
}

fn role_unavailable(command: &str) -> RmuxError {
    RmuxError::Server(format!(
        "queued {command} lifecycle target role was unavailable after parsing"
    ))
}

#[cfg(test)]
mod tests {
    use rmux_core::command_parser::CommandParser;

    use super::*;

    fn plan(command: &str) -> Option<QueueLifecycleTargetPlan> {
        let parsed = CommandParser::new().parse(command).expect("command parses");
        QueueLifecycleTargetPlan::for_command(&parsed.commands()[0])
            .expect("covered command has option inventory")
    }

    #[test]
    fn normal_path_scope_is_explicit_and_complete() {
        for command in [
            "rename-window name",
            "send-keys",
            "join-pane",
            "new-window",
            "lock-session",
            "display-message -p",
            "capture-pane -p",
        ] {
            assert!(plan(command).is_some(), "{command}");
        }
        for special in [
            "command-prompt -t client",
            "confirm-before -t client 'display-message ok'",
            "display-panes -t client",
            "if-shell true ''",
            "source-file file.conf",
            "run-shell true",
        ] {
            assert!(plan(special).is_none(), "{special}");
        }
    }

    #[test]
    fn join_roles_track_source_and_destination_independently() {
        let source = plan("join-pane -s alpha:0.0").expect("join plan");
        assert_eq!(source.source, RoleUse::Explicit);
        assert_eq!(source.destination, RoleUse::Implicit);

        let destination = plan("join-pane -t beta:0.0").expect("join plan");
        assert_eq!(destination.source, RoleUse::Implicit);
        assert_eq!(destination.destination, RoleUse::Explicit);

        let both = plan("join-pane -s alpha:0.0 -t beta:0.0").expect("join plan");
        assert!(!both.uses_implicit_role());
    }

    #[test]
    fn target_client_roles_do_not_consume_the_lifecycle_target() {
        let send = plan("send-keys -c client C-a").expect("send-keys plan");
        assert_eq!(send.primary, RoleUse::Unused);
        assert!(!send.uses_implicit_role());

        let display = plan("display-message -c client hello").expect("display plan");
        assert_eq!(display.primary, RoleUse::Unused);
        assert!(!display.uses_implicit_role());
    }
}
