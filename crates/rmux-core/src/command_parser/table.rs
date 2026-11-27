/// Frozen tmux command table entry used for command lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEntry {
    /// The canonical tmux command name.
    pub name: &'static str,
    /// The exact tmux alias, when the frozen entry declares one.
    pub alias: Option<&'static str>,
}

/// Frozen command inventory from `/opt/rmux/reference/tmux`
/// `31d77e29b6c9fbb07d032018da78db3a8a38d979` `cmd.c:121`.
pub const COMMAND_TABLE: &[CommandEntry] = crate::command_schema::metadata::TMUX_COMMANDS;
