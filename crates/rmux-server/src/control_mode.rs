use rmux_proto::ControlMode;

use crate::outer_terminal::OuterTerminalContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlModeUpgrade {
    pub(crate) mode: ControlMode,
    pub(crate) terminal_context: OuterTerminalContext,
}
