use std::sync::Arc;

use rmux_core::{command_parser::ParsedCommand, TargetFindContext};
use rmux_proto::{RmuxError, Target};

use super::super::lifecycle_support::{LeaseResolution, LifecycleTargetLease};
use super::super::{StablePaneOutputIdentity, StableTargetIdentity};
use super::implicit_pane_target;
use super::queue::{QueueExecutionContext, QueueInvocation};
use super::queue_exact_target::has_short_option_value;
use crate::handler::RequestHandler;
use crate::pane_terminals::HandlerState;

#[derive(Debug)]
pub(super) struct QueueSpecialTargetPlan {
    command: &'static str,
    explicit: bool,
}

#[derive(Debug, Clone)]
pub(super) struct QueueSpecialTargetBinding {
    explicit: bool,
    target: Option<Target>,
    target_identity: Option<StableTargetIdentity>,
    pane_output_identity: Option<StablePaneOutputIdentity>,
    retained_target: Option<Arc<LifecycleTargetLease>>,
    retained_identity: Option<StableTargetIdentity>,
}

#[derive(Debug)]
pub(super) struct QueueSpecialTargetPending {
    explicit: bool,
    target: Option<Target>,
    retained_target: Option<Arc<LifecycleTargetLease>>,
    retained_live_target: Option<Target>,
}

impl QueueSpecialTargetPlan {
    pub(super) fn for_command(command: &ParsedCommand) -> Result<Option<Self>, RmuxError> {
        let command_name = match command.name() {
            "if-shell" => "if-shell",
            "source-file" => "source-file",
            "run-shell" => "run-shell",
            _ => return Ok(None),
        };
        let explicit = has_short_option_value(command, 't').ok_or_else(|| {
            RmuxError::Server(format!(
                "queued {command_name} lifecycle guard has no short-option inventory"
            ))
        })?;
        Ok(Some(Self {
            command: command_name,
            explicit,
        }))
    }

    pub(super) fn resolve_parse_target(
        &self,
        retained_target: Option<&Arc<LifecycleTargetLease>>,
        state: &HandlerState,
    ) -> Result<Option<Target>, RmuxError> {
        if self.explicit {
            return Ok(None);
        }
        let Some(retained_target) = retained_target else {
            return Ok(None);
        };
        match retained_target.resolve(state) {
            LeaseResolution::Live(target) => Ok(Some(target)),
            LeaseResolution::Retired(_) => Err(RmuxError::Server(format!(
                "queued {} lifecycle target retired before parsing",
                self.command
            ))),
            LeaseResolution::Replaced => Err(RmuxError::Server(format!(
                "queued {} lifecycle target was replaced before parsing",
                self.command
            ))),
        }
    }

    pub(super) fn bind(
        self,
        invocation: &mut QueueInvocation,
        state: &HandlerState,
        find_context: &TargetFindContext,
        retained_target: Option<Arc<LifecycleTargetLease>>,
    ) -> Result<Option<QueueSpecialTargetPending>, RmuxError> {
        if self.explicit {
            let target = invocation_target(invocation);
            return Ok(Some(QueueSpecialTargetPending {
                explicit: true,
                target,
                retained_target: None,
                retained_live_target: None,
            }));
        }

        let Some(retained_target) = retained_target else {
            return Ok(None);
        };
        let retained_live_target = match retained_target.resolve(state) {
            LeaseResolution::Live(target) => target,
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
        };
        let target = match self.command {
            "if-shell" => retained_live_target.clone(),
            "source-file" | "run-shell" => Target::Pane(implicit_pane_target(
                &state.sessions,
                find_context,
                self.command,
            )?),
            _ => unreachable!("special target plan only covers three commands"),
        };
        bind_invocation_target(invocation, target.clone())?;
        Ok(Some(QueueSpecialTargetPending {
            explicit: false,
            target: Some(target),
            retained_target: Some(retained_target),
            retained_live_target: Some(retained_live_target),
        }))
    }
}

impl QueueSpecialTargetPending {
    pub(super) fn capture(
        self,
        state: &mut HandlerState,
    ) -> Result<QueueSpecialTargetBinding, RmuxError> {
        let target_identity = self
            .target
            .clone()
            .map(|target| StableTargetIdentity::capture(state, target))
            .transpose()?;
        let pane_output_identity = self
            .target
            .as_ref()
            .and_then(|target| StablePaneOutputIdentity::capture_for_target(state, target));
        let retained_identity = self
            .retained_live_target
            .map(|target| StableTargetIdentity::capture(state, target))
            .transpose()?;
        Ok(QueueSpecialTargetBinding {
            explicit: self.explicit,
            target: self.target,
            target_identity,
            pane_output_identity,
            retained_target: self.retained_target,
            retained_identity,
        })
    }
}

impl QueueSpecialTargetBinding {
    pub(super) fn is_explicit(&self) -> bool {
        self.explicit
    }

    pub(super) fn require_live(&self, state: &HandlerState) -> Result<(), RmuxError> {
        if let Some(retained_target) = self.retained_target.as_ref() {
            match retained_target.resolve(state) {
                LeaseResolution::Live(target) => self
                    .retained_identity
                    .as_ref()
                    .ok_or_else(|| {
                        RmuxError::Server("queued lifecycle identity was unavailable".to_owned())
                    })?
                    .require(state, &target, "queued lifecycle")?,
                LeaseResolution::Retired(_) => {
                    return Err(RmuxError::Server(
                        "queued lifecycle target retired before execution".to_owned(),
                    ))
                }
                LeaseResolution::Replaced => {
                    return Err(RmuxError::Server(
                        "queued lifecycle target was replaced before execution".to_owned(),
                    ))
                }
            }
        }
        if let (Some(identity), Some(target)) = (&self.target_identity, &self.target) {
            identity.require(state, target, "queued special")?;
        }
        if let Some(identity) = self.pane_output_identity.as_ref() {
            identity.require(state, "queued special")?;
        }
        Ok(())
    }

    pub(super) async fn require_live_for(&self, handler: &RequestHandler) -> Result<(), RmuxError> {
        let state = handler.state.lock().await;
        self.require_live(&state)
    }

    pub(super) fn child_context(&self, context: &QueueExecutionContext) -> QueueExecutionContext {
        let context = if self.explicit {
            context
                .clone()
                .with_current_target(self.target.clone())
                .without_retained_lifecycle_target()
        } else {
            context
                .clone()
                .with_implicit_current_target(self.target.clone())
                .with_retained_lifecycle_target(self.retained_target.clone())
        };
        context
            .with_pinned_current_target_identity(self.target_identity.clone())
            .with_pinned_pane_output_identity(self.pane_output_identity.clone())
    }
}

fn invocation_target(invocation: &QueueInvocation) -> Option<Target> {
    match invocation {
        QueueInvocation::IfShell(command) => command.target.clone(),
        QueueInvocation::SourceFile(command) => command.target.clone().map(Target::Pane),
        QueueInvocation::RunShell(command) => command.request.target.clone().map(Target::Pane),
        _ => None,
    }
}

fn bind_invocation_target(
    invocation: &mut QueueInvocation,
    target: Target,
) -> Result<(), RmuxError> {
    match (invocation, target) {
        (QueueInvocation::IfShell(command), target) if command.target.is_none() => {
            command.target = Some(target);
            Ok(())
        }
        (QueueInvocation::SourceFile(command), Target::Pane(target))
            if command.target.is_none() =>
        {
            command.target = Some(target);
            Ok(())
        }
        (QueueInvocation::RunShell(command), Target::Pane(target))
            if command.request.target.is_none() =>
        {
            command.request.target = Some(target);
            Ok(())
        }
        _ => Err(RmuxError::Server(
            "queued special lifecycle target could not be bound".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use rmux_core::command_parser::CommandParser;

    use super::*;

    fn plan(command: &str) -> QueueSpecialTargetPlan {
        let parsed = CommandParser::new().parse(command).expect("command parses");
        QueueSpecialTargetPlan::for_command(&parsed.commands()[0])
            .expect("special command has option inventory")
            .expect("special command is covered")
    }

    #[test]
    fn special_scope_and_explicit_overrides_are_small_and_declared() {
        assert!(!plan("if-shell -F 1 ''").explicit);
        assert!(plan("if-shell -t beta -F 1 ''").explicit);
        assert!(!plan("source-file file.conf").explicit);
        assert!(plan("source-file -t beta file.conf").explicit);
        assert!(!plan("run-shell -C ''").explicit);
        assert!(plan("run-shell -bC -t beta ''").explicit);
    }
}
