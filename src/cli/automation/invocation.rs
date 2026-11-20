use std::ffi::OsString;
use std::time::Instant;

use rmux_bootstrap::deadline::StartupDeadline;

use crate::cli::ExitFailure;
use crate::cli_args::{parse, Command};

/// A standalone wait owns one budget, including runtime alias negotiation.
/// Queued commands retain their individual budgets at typed dispatch.
pub(in crate::cli) struct InvocationBudget {
    command: Option<Command>,
    deadline: Option<StartupDeadline>,
    started_at: Instant,
}

impl InvocationBudget {
    pub(in crate::cli) fn from_args(args: &[OsString]) -> Self {
        let started_at = Instant::now();
        let command = parse(args.to_vec())
            .ok()
            .and_then(|cli| {
                if cli.control_mode != 0 || cli.no_fork || cli.shell_command.is_some() {
                    return None;
                }
                let mut commands = cli.into_command_queue();
                (commands.len() == 1).then(|| commands.remove(0))
            })
            .filter(|command| {
                matches!(command, Command::WaitPane(_))
                    || matches!(command, Command::SendKeys(args) if args.has_wait())
            });
        let deadline = command
            .as_ref()
            .map(|command| deadline_for(command, started_at));
        Self {
            command,
            deadline,
            started_at,
        }
    }

    /// Runtime aliases may change the timeout, wait kind, or queue shape.
    /// Charge a standalone canonical wait for time already spent resolving it.
    pub(in crate::cli) fn reconcile(&mut self, commands: &[Command]) {
        self.command = match commands {
            [command @ Command::WaitPane(_)] => Some(command.clone()),
            [command @ Command::SendKeys(args)] if args.has_wait() => Some(command.clone()),
            _ => None,
        };
        self.deadline = self
            .command
            .as_ref()
            .map(|command| deadline_for(command, self.started_at));
    }

    pub(in crate::cli) fn deadline(&self) -> Option<StartupDeadline> {
        self.deadline
    }

    pub(in crate::cli) fn finish(
        self,
        result: Result<i32, ExitFailure>,
    ) -> Result<i32, ExitFailure> {
        match (result, self.command) {
            (Err(error), Some(command)) if error.is_timeout() => {
                super::wait::invocation_timeout(&command, self.started_at)
            }
            (result, _) => result,
        }
    }
}

fn deadline_for(command: &Command, started_at: Instant) -> StartupDeadline {
    let timeout = match command {
        Command::WaitPane(args) => args.timeout,
        Command::SendKeys(args) => args.timeout,
        _ => unreachable!("only standalone wait commands own a budget"),
    };
    StartupDeadline::from_timeout_at(
        Some(timeout.unwrap_or(super::common::DEFAULT_TIMEOUT)),
        started_at,
    )
}
