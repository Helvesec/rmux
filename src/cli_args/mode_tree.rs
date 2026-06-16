use clap::{ArgAction, Args};

use super::QueuedCommand;

#[derive(Debug, Clone, Args)]
pub(crate) struct ChooseTreeArgs {
    #[arg(short = 'G', action = ArgAction::SetTrue)]
    pub(crate) show_all_group_members: bool,
    #[arg(short = 'N', action = ArgAction::Count)]
    pub(crate) preview: u8,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reversed: bool,
    #[arg(short = 's', action = ArgAction::SetTrue)]
    pub(crate) sessions_collapsed: bool,
    #[arg(short = 'w', action = ArgAction::SetTrue)]
    pub(crate) windows_collapsed: bool,
    #[arg(short = 'y', action = ArgAction::SetTrue, hide = true)]
    unsupported_auto_accept: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) row_format: Option<String>,
    #[arg(short = 'f', allow_hyphen_values = true)]
    pub(crate) filter_format: Option<String>,
    #[arg(short = 'K', allow_hyphen_values = true)]
    pub(crate) key_format: Option<String>,
    #[arg(short = 'O', allow_hyphen_values = true)]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_pane: Option<String>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) template: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl ChooseTreeArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.unsupported_auto_accept {
            return Err(unknown_flag_error("choose-tree", "-y"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ChooseBufferArgs {
    #[arg(short = 'N', action = ArgAction::Count)]
    pub(crate) preview: u8,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reversed: bool,
    #[arg(short = 'y', action = ArgAction::SetTrue, hide = true)]
    unsupported_auto_accept: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) row_format: Option<String>,
    #[arg(short = 'f', allow_hyphen_values = true)]
    pub(crate) filter_format: Option<String>,
    #[arg(short = 'K', allow_hyphen_values = true)]
    pub(crate) key_format: Option<String>,
    #[arg(short = 'O', allow_hyphen_values = true)]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_pane: Option<String>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) template: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl ChooseBufferArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.unsupported_auto_accept {
            return Err(unknown_flag_error("choose-buffer", "-y"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ChooseClientArgs {
    #[arg(short = 'N', action = ArgAction::Count)]
    pub(crate) preview: u8,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reversed: bool,
    #[arg(short = 'y', action = ArgAction::SetTrue, hide = true)]
    unsupported_auto_accept: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) row_format: Option<String>,
    #[arg(short = 'f', allow_hyphen_values = true)]
    pub(crate) filter_format: Option<String>,
    #[arg(short = 'K', allow_hyphen_values = true)]
    pub(crate) key_format: Option<String>,
    #[arg(short = 'O', allow_hyphen_values = true)]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_pane: Option<String>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) template: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl ChooseClientArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.unsupported_auto_accept {
            return Err(unknown_flag_error("choose-client", "-y"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct CustomizeModeArgs {
    #[arg(short = 'N', action = ArgAction::Count)]
    pub(crate) preview: u8,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) row_format: Option<String>,
    #[arg(short = 'f', allow_hyphen_values = true)]
    pub(crate) filter_format: Option<String>,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_pane: Option<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for ChooseTreeArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

impl QueuedCommand for ChooseBufferArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

impl QueuedCommand for ChooseClientArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

impl QueuedCommand for CustomizeModeArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

fn unknown_flag_error(command_name: &str, flag: &str) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command {command_name}: unknown flag {flag}"),
    )
}
