use std::sync::Arc;

use rmux_proto::RmuxError;

use crate::ClientError;

use super::terminal::RawTerminal;

pub(super) trait AttachActionExecutor {
    fn handle_lock(&mut self, command: &str) -> std::result::Result<(), ClientError>;
    fn handle_suspend(&mut self) -> std::result::Result<(), ClientError>;
    fn handle_detach_kill(&mut self) -> std::result::Result<(), ClientError>;
    fn handle_detach_exec(&mut self, command: &str) -> std::result::Result<(), ClientError>;
}

#[derive(Debug)]
pub(super) enum AttachAction {
    Lock(String),
    Suspend,
    DetachKill,
    DetachExec(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttachActionOutcome {
    Unlock,
    Exit,
}

#[derive(Debug)]
pub(super) struct ManagedTerminalActions {
    terminal: Arc<RawTerminal>,
}

impl ManagedTerminalActions {
    pub(super) fn new(terminal: Arc<RawTerminal>) -> Self {
        Self { terminal }
    }
}

impl AttachActionExecutor for ManagedTerminalActions {
    fn handle_lock(&mut self, command: &str) -> std::result::Result<(), ClientError> {
        self.terminal
            .run_lock_command(command)
            .map_err(ClientError::from)
    }

    fn handle_suspend(&mut self) -> std::result::Result<(), ClientError> {
        self.terminal.suspend_self().map_err(ClientError::from)
    }

    fn handle_detach_kill(&mut self) -> std::result::Result<(), ClientError> {
        self.terminal.restore().map_err(ClientError::from)
    }

    fn handle_detach_exec(&mut self, command: &str) -> std::result::Result<(), ClientError> {
        self.terminal
            .run_detach_exec_command(command)
            .map_err(ClientError::from)
    }
}

#[derive(Debug, Default)]
pub(super) struct StreamOnlyActions;

impl AttachActionExecutor for StreamOnlyActions {
    fn handle_lock(&mut self, _command: &str) -> std::result::Result<(), ClientError> {
        Err(unmanaged_terminal_error("lock"))
    }

    fn handle_suspend(&mut self) -> std::result::Result<(), ClientError> {
        Err(unmanaged_terminal_error("suspend"))
    }

    fn handle_detach_kill(&mut self) -> std::result::Result<(), ClientError> {
        Ok(())
    }

    fn handle_detach_exec(&mut self, _command: &str) -> std::result::Result<(), ClientError> {
        Err(unmanaged_terminal_error("detach exec"))
    }
}

fn unmanaged_terminal_error(action: &str) -> ClientError {
    ClientError::Protocol(RmuxError::Decode(format!(
        "received unexpected {action} request without a managed terminal"
    )))
}

pub(super) fn run_attach_action(
    actions: &mut impl AttachActionExecutor,
    action: AttachAction,
) -> std::result::Result<AttachActionOutcome, ClientError> {
    match action {
        AttachAction::Lock(command) => {
            actions.handle_lock(&command)?;
            Ok(AttachActionOutcome::Unlock)
        }
        AttachAction::Suspend => {
            actions.handle_suspend()?;
            Ok(AttachActionOutcome::Unlock)
        }
        AttachAction::DetachKill => {
            actions.handle_detach_kill()?;
            Ok(AttachActionOutcome::Exit)
        }
        AttachAction::DetachExec(command) => {
            actions.handle_detach_exec(&command)?;
            Ok(AttachActionOutcome::Exit)
        }
    }
}
