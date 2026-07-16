use clap::{ArgAction, Args};

use super::QueuedCommand;

#[derive(Debug, Clone, Args)]
pub(crate) struct PromptArgs {
    #[arg(short = '1', action = ArgAction::SetTrue)]
    pub(crate) single: bool,
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) background: bool,
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) command_error: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue)]
    pub(crate) backspace_exit: bool,
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub(crate) format_template: bool,
    #[arg(short = 'i', action = ArgAction::SetTrue)]
    pub(crate) incremental: bool,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) key: bool,
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) literal: bool,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) numeric: bool,
    #[arg(short = 'I', allow_hyphen_values = true)]
    pub(crate) inputs: Option<String>,
    #[arg(short = 'p', allow_hyphen_values = true)]
    pub(crate) prompts: Option<String>,
    #[arg(short = 'T', allow_hyphen_values = true)]
    pub(crate) prompt_type: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) template: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl PromptArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ConfirmBeforeArgs {
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) background: bool,
    #[arg(short = 'y', action = ArgAction::SetTrue)]
    pub(crate) default_yes: bool,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) confirm_key: Option<String>,
    #[arg(short = 'p', allow_hyphen_values = true)]
    pub(crate) prompt: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(required = true, allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

/// Arguments for `clear-prompt-history` / `show-prompt-history`.
#[derive(Debug, Clone, Args)]
pub(crate) struct PromptHistoryArgs {
    /// Optional prompt type filter (command, search, target, window-target).
    #[arg(short = 'T', allow_hyphen_values = true)]
    pub(crate) prompt_type: Option<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for PromptArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

impl QueuedCommand for ConfirmBeforeArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

impl QueuedCommand for PromptHistoryArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}
